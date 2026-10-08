//! A terminal on screen: one shell session ([`flux_term::Terminal`]) with its keyboard, IME,
//! mouse, clipboard, and drawing ([`TerminalElement`]).
//!
//! The panes of a terminal tab are `TerminalView`s ([`crate::terminal_group`]). The search bar and
//! the ⌘-links are separate modules that keep their state here: [`crate::terminal_search`],
//! [`crate::terminal_links`].
//!
//! Keys: a bound action wins (⌘C, ⌘V, ⌘F, ⌘W, the window's shortcuts); other keys that aren't
//! text are encoded by [`flux_term::keys`] in `on_key_down`; text, including what Option types and
//! IME compositions, arrives through the input handler and is written as UTF-8.
//!
//! The mouse: when the program asked for mouse reports (vim with `mouse=a`, htop) presses, drags,
//! and the wheel go to it ([`flux_term::mouse`]); ⇧ keeps the mouse for selection. Otherwise a drag
//! selects (a double click — words, a triple click — lines, ⌥ — a rectangle, ⇧-click extends),
//! dragging past the top or bottom edge scrolls, the wheel scrolls the scrollback (on the alternate
//! screen of `less` or `man` it sends arrow keys), the right button opens a menu.

use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use flux_term::keys::{self, Modifiers};
use flux_term::mouse::{self, MouseAction};
use flux_term::{Palette, Rgb, ScrollDelta, Terminal, TerminalEvent, TerminalOptions};
use futures::StreamExt;
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, DismissEvent, Entity, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, Hsla, KeyBinding, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollWheelEvent,
    SharedString, Subscription, Task, UTF16Selection, Window, actions, div, prelude::*, px,
};

use crate::context_menu::ContextMenu;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::terminal_element::{TerminalElement, TerminalLayout};
use crate::theme::{self, Theme};
use crate::ui;

actions!(
    terminal,
    [
        Copy,
        Paste,
        SelectAll,
        Clear,
        ScrollPageUp,
        ScrollPageDown,
        ScrollToTop,
        ScrollToBottom,
    ]
);

pub fn init(cx: &mut App) {
    let context = Some("Terminal");
    cx.bind_keys([
        KeyBinding::new("cmd-c", Copy, context),
        KeyBinding::new("cmd-v", Paste, context),
        KeyBinding::new("cmd-a", SelectAll, context),
        KeyBinding::new("cmd-k", Clear, context),
        KeyBinding::new("shift-pageup", ScrollPageUp, context),
        KeyBinding::new("shift-pagedown", ScrollPageDown, context),
        KeyBinding::new("cmd-home", ScrollToTop, context),
        KeyBinding::new("cmd-end", ScrollToBottom, context),
    ]);
}

/// How long the process name and directory may lag behind: they are re-read at most this often
/// while output is coming.
const PROCESS_REFRESH: Duration = Duration::from_millis(250);
/// A shell that fails this soon after starting keeps its pane, so its error can be read (a broken
/// profile, a missing shell); later exits close the pane at once.
const EARLY_EXIT: Duration = Duration::from_secs(1);
/// How often a selection dragged past the top or bottom edge scrolls, and how many lines a step
/// may take at most (the farther the pointer, the faster).
const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(50);
const AUTOSCROLL_MAX_LINES: i32 = 8;
/// One wheel event sends at most this many wheel reports to a program.
const MAX_WHEEL_REPORTS: u32 = 10;

/// Where a ⌘-click in the output leads, already checked against the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalLink {
    /// A file, with the 1-based line and column if the output had them.
    File {
        path: PathBuf,
        line: Option<u32>,
        column: Option<u32>,
    },
    Directory(PathBuf),
    Url(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalViewEvent {
    /// The tab label may have changed: the title, the foreground process, the directory.
    TitleChanged,
    /// The bell rang (BEL).
    Bell,
    /// The shell exited: the pane goes away.
    Exited,
    /// ⌘-click on a link in the output.
    OpenLink(TerminalLink),
    /// Focus came into this terminal: its group makes it the active pane.
    Focused,
}

/// The right-click menu and where it was opened.
struct Menu {
    menu: Entity<ContextMenu>,
    position: Point<Pixels>,
    _subscriptions: [Subscription; 2],
}

/// One terminal session on screen.
pub struct TerminalView {
    pub(crate) terminal: Terminal,
    pub(crate) focus_handle: FocusHandle,
    /// The project root: relative paths in the output are also looked up there (links).
    pub(crate) root: Option<PathBuf>,
    /// The title the program set (OSC 0, 2), if any.
    pub(crate) title: Option<String>,
    /// The foreground process ("zsh", "cargo") and the shell's directory, re-read as output comes.
    pub(crate) process_name: Option<String>,
    pub(crate) cwd: Option<PathBuf>,
    /// The shell has exited (with its code, if it exited normally).
    pub(crate) exited: Option<Option<i32>>,
    /// The geometry of the last frame: the mouse and the IME map points to cells with it.
    pub(crate) layout: Option<TerminalLayout>,
    /// An IME composition in progress, drawn at the cursor until it is committed.
    pub(crate) marked_text: Option<String>,
    /// Blink phase: the cursor is currently drawn.
    pub(crate) cursor_visible: bool,
    blink_task: Option<Task<()>>,
    /// A mouse selection is being dragged.
    selecting: bool,
    /// The pointer while a selection is dragged: dragging past an edge keeps scrolling from it.
    drag_position: Option<Point<Pixels>>,
    autoscroll: Option<Task<()>>,
    /// A button whose press went to the program: its motion and release go there too.
    reported_button: Option<mouse::MouseButton>,
    /// The cell of the last motion report: motion is reported once per cell, not per pixel.
    reported_cell: Option<(usize, usize)>,
    /// Wheel lines not scrolled yet: a trackpad moves by fractions of a line.
    scroll_remainder: f32,
    menu: Option<Menu>,
    /// Search in the output: the bar and the matches the element draws.
    pub(crate) search: crate::terminal_search::SearchState,
    /// ⌘-links: the link under the mouse while ⌘ is held is underlined by the element.
    pub(crate) links: crate::terminal_links::LinkState,
    started: Instant,
    process_refresh: Option<Task<()>>,
    _events: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

impl TerminalView {
    /// Starts the user's shell in `cwd` (the project root, or the directory of the pane being
    /// split). `root` is the project, where relative paths in the output are also looked up.
    pub fn spawn(
        cwd: Option<PathBuf>,
        root: Option<PathBuf>,
        window: &mut Window,
        cx: &mut App,
    ) -> std::io::Result<Entity<Self>> {
        let options = TerminalOptions {
            cwd: cwd.clone(),
            palette: palette(Theme::get(cx)),
            ..TerminalOptions::default()
        };
        let (terminal, events) = Terminal::spawn(options)?;
        Ok(cx.new(|cx: &mut Context<Self>| {
            let events_task = cx.spawn(async move |this, cx| {
                let mut events = events;
                while let Some(event) = events.next().await {
                    let handled = this.update(cx, |this, cx| this.handle_event(event, cx));
                    if handled.is_err() {
                        break;
                    }
                }
            });
            let focus_handle = cx.focus_handle();
            let subscriptions = vec![
                cx.on_focus(&focus_handle, window, |this, window, cx| {
                    this.terminal.report_focus(true);
                    this.restart_blink(window, cx);
                    cx.emit(TerminalViewEvent::Focused);
                }),
                cx.on_blur(&focus_handle, window, |this, window, cx| {
                    this.terminal.report_focus(false);
                    this.restart_blink(window, cx);
                }),
                cx.observe_window_activation(window, Self::restart_blink),
                cx.observe_global::<Theme>(|this, cx| {
                    this.terminal.set_palette(palette(Theme::get(cx)));
                    cx.notify();
                }),
            ];
            Self {
                terminal,
                focus_handle,
                root,
                title: None,
                process_name: None,
                cwd,
                exited: None,
                layout: None,
                marked_text: None,
                cursor_visible: true,
                blink_task: None,
                selecting: false,
                drag_position: None,
                autoscroll: None,
                reported_button: None,
                reported_cell: None,
                scroll_remainder: 0.,
                menu: None,
                search: Default::default(),
                links: Default::default(),
                started: Instant::now(),
                process_refresh: None,
                _events: events_task,
                _subscriptions: subscriptions,
            }
        }))
    }

    /// The tab label: the foreground process, else the program's title, else "Terminal".
    pub fn label(&self) -> SharedString {
        self.process_name
            .clone()
            .or_else(|| self.title.clone())
            .unwrap_or_else(|| tr("Terminal").to_string())
            .into()
    }

    /// The command running in the shell (closing the terminal would kill it), by name.
    pub fn running_process(&self) -> Option<String> {
        if self.exited.is_some() {
            return None;
        }
        self.terminal.running_process().map(|process| process.name)
    }

    /// The shell's current directory: a split opens there.
    pub fn cwd(&self) -> Option<PathBuf> {
        self.terminal.cwd().or_else(|| self.cwd.clone())
    }

    fn handle_event(&mut self, event: TerminalEvent, cx: &mut Context<Self>) {
        match event {
            TerminalEvent::Wakeup => {
                self.schedule_process_refresh(cx);
                crate::terminal_search::output_changed(self, cx);
                cx.notify();
            }
            TerminalEvent::Title(title) => {
                self.title = title;
                cx.emit(TerminalViewEvent::TitleChanged);
            }
            TerminalEvent::Bell => cx.emit(TerminalViewEvent::Bell),
            TerminalEvent::Clipboard(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text))
            }
            TerminalEvent::Exit(code) => {
                self.exited = Some(code);
                self.blink_task = None;
                self.autoscroll = None;
                let failed = code != Some(0);
                // A failure right after the start leaves the pane with its output and a note;
                // otherwise the pane closes.
                if !(failed && self.started.elapsed() < EARLY_EXIT) {
                    cx.emit(TerminalViewEvent::Exited);
                }
                cx.notify();
            }
        }
    }

    /// Re-reads the foreground process and the directory a little after output: at most once per
    /// [`PROCESS_REFRESH`], and always after the last burst.
    fn schedule_process_refresh(&mut self, cx: &mut Context<Self>) {
        if self.process_refresh.is_some() {
            return;
        }
        self.process_refresh = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PROCESS_REFRESH).await;
            this.update(cx, |this, cx| {
                this.process_refresh = None;
                this.refresh_process_info(cx);
            })
            .ok();
        }));
    }

    fn refresh_process_info(&mut self, cx: &mut Context<Self>) {
        if self.exited.is_some() {
            return;
        }
        let name = self
            .terminal
            .foreground_process()
            .map(|process| process.name);
        let cwd = self.terminal.cwd().or_else(|| self.cwd.clone());
        if name != self.process_name || cwd != self.cwd {
            self.process_name = name;
            self.cwd = cwd;
            cx.emit(TerminalViewEvent::TitleChanged);
        }
    }

    // --- Keyboard and clipboard ---

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Keys typed into the search bar (inside the view) are not for the program.
        if !self.focus_handle.is_focused(window) {
            return;
        }
        let keystroke = &event.keystroke;
        let modifiers = key_modifiers(&keystroke.modifiers);
        if let Some(bytes) = keys::encode(&keystroke.key, modifiers, self.terminal.mode()) {
            self.terminal.input(bytes);
            self.typed(cx);
            cx.stop_propagation();
        }
    }

    /// After input: the cursor is shown at once and the selection is gone.
    fn typed(&mut self, cx: &mut Context<Self>) {
        self.selecting = false;
        self.pause_blink(cx);
        cx.notify();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.terminal.selection_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.terminal.paste(&text);
            self.typed(cx);
        }
    }

    fn scroll(&mut self, delta: ScrollDelta, cx: &mut Context<Self>) {
        self.terminal.scroll(delta);
        cx.notify();
    }

    // --- Mouse ---

    /// Sends a mouse event to the program if it takes the mouse (vim with `mouse=a`, htop, tmux).
    /// `false` — the terminal handles the event itself.
    fn report_mouse(
        &mut self,
        button: Option<mouse::MouseButton>,
        action: MouseAction,
        position: Point<Pixels>,
        modifiers: &gpui::Modifiers,
    ) -> bool {
        let Some(layout) = self.layout else {
            return false;
        };
        let (row, column) = layout.viewport_cell(position);
        let event = mouse::MouseEvent {
            button,
            action,
            column,
            row,
            modifiers: key_modifiers(modifiers),
        };
        let mode = self.terminal.mode();
        if !mouse::is_reported(&event, mode) {
            return false;
        }
        if action == MouseAction::Move {
            if self.reported_cell == Some((row, column)) {
                return true;
            }
            self.reported_cell = Some((row, column));
        }
        if let Some(bytes) = mouse::encode(&event, mode) {
            self.terminal.write(bytes);
        }
        true
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        if event.button == MouseButton::Left
            && crate::terminal_links::cmd_click(self, event, window, cx)
        {
            return;
        }
        let Some(button) = term_button(event.button) else {
            return;
        };
        if self.report_mouse(
            Some(button),
            MouseAction::Press,
            event.position,
            &event.modifiers,
        ) {
            self.reported_button = Some(button);
            return;
        }
        match event.button {
            MouseButton::Left => self.start_selection(event, cx),
            MouseButton::Right => self.open_menu(event.position, window, cx),
            _ => {}
        }
    }

    fn start_selection(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some(layout) = self.layout else {
            return;
        };
        let (point, side) = layout.grid_point(event.position);
        let kind = match event.click_count {
            2 => flux_term::SelectionKind::Word,
            3.. => flux_term::SelectionKind::Line,
            _ if event.modifiers.alt => flux_term::SelectionKind::Block,
            _ => flux_term::SelectionKind::Simple,
        };
        if event.modifiers.shift && event.click_count == 1 {
            self.terminal.update_selection(point, side);
        } else {
            self.terminal.start_selection(kind, point, side);
        }
        self.selecting = true;
        self.drag_position = Some(event.position);
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        crate::terminal_links::mouse_moved(self, event, window, cx);
        if let Some(button) = self.reported_button {
            // A drag with a button the program got: the motion is its too.
            self.report_mouse(
                Some(button),
                MouseAction::Move,
                event.position,
                &event.modifiers,
            );
            return;
        }
        if event.pressed_button.is_none()
            && self.report_mouse(None, MouseAction::Move, event.position, &event.modifiers)
        {
            return;
        }
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            self.selecting = false;
            self.autoscroll = None;
            return;
        }
        let Some(layout) = self.layout else {
            return;
        };
        self.drag_position = Some(event.position);
        let (point, side) = layout.grid_point(event.position);
        self.terminal.update_selection(point, side);
        if layout.rows_outside(event.position) != 0 {
            self.start_autoscroll(cx);
        } else {
            self.autoscroll = None;
        }
        cx.notify();
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(button) = self.reported_button
            && term_button(event.button) == Some(button)
        {
            self.report_mouse(
                Some(button),
                MouseAction::Release,
                event.position,
                &event.modifiers,
            );
            self.reported_button = None;
            self.reported_cell = None;
            return;
        }
        if event.button != MouseButton::Left || !self.selecting {
            return;
        }
        self.selecting = false;
        self.autoscroll = None;
        self.drag_position = None;
        // A click without a drag leaves an empty selection: nothing to keep.
        if self.terminal.selection_text().is_none() {
            self.terminal.clear_selection();
        }
        cx.notify();
    }

    /// The selection is dragged past the top or bottom edge: the view keeps scrolling and the
    /// selection follows until the pointer comes back or the button is released.
    fn start_autoscroll(&mut self, cx: &mut Context<Self>) {
        if self.autoscroll.is_some() {
            return;
        }
        self.autoscroll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AUTOSCROLL_INTERVAL).await;
                let more = this
                    .update(cx, |this, cx| this.autoscroll_step(cx))
                    .unwrap_or(false);
                if !more {
                    break;
                }
            }
        }));
    }

    fn autoscroll_step(&mut self, cx: &mut Context<Self>) -> bool {
        let (Some(layout), Some(position)) = (self.layout, self.drag_position) else {
            return false;
        };
        let outside = layout.rows_outside(position);
        if !self.selecting || outside == 0 {
            self.autoscroll = None;
            return false;
        }
        // The selection reaches the edge row first, then the view moves on: up into the history
        // above the top edge, down below the bottom one.
        let (point, side) = layout.grid_point(position);
        self.terminal.update_selection(point, side);
        let lines = outside.clamp(-AUTOSCROLL_MAX_LINES, AUTOSCROLL_MAX_LINES);
        self.terminal.scroll(ScrollDelta::Lines(-lines));
        cx.notify();
        true
    }

    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let line_height = px(theme::TERMINAL_LINE_HEIGHT);
        let delta = event.delta.pixel_delta(line_height);
        self.scroll_remainder += f32::from(delta.y) / theme::TERMINAL_LINE_HEIGHT;
        let lines = self.scroll_remainder.trunc() as i32;
        if lines == 0 {
            return;
        }
        self.scroll_remainder -= lines as f32;
        // Positive is up: away from the live screen.
        let button = if lines > 0 {
            mouse::MouseButton::WheelUp
        } else {
            mouse::MouseButton::WheelDown
        };
        if self.report_mouse(
            Some(button),
            MouseAction::Press,
            event.position,
            &event.modifiers,
        ) {
            for _ in 1..lines.unsigned_abs().min(MAX_WHEEL_REPORTS) {
                self.report_mouse(
                    Some(button),
                    MouseAction::Press,
                    event.position,
                    &event.modifiers,
                );
            }
            return;
        }
        if let Some(bytes) = mouse::alternate_scroll(lines, self.terminal.mode()) {
            self.terminal.write(bytes);
            return;
        }
        self.scroll(ScrollDelta::Lines(lines), cx);
    }

    // --- Context menu ---

    fn open_menu(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let has_selection = self.terminal.selection_text().is_some();
        let can_paste = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .is_some_and(|text| !text.is_empty());
        let menu = cx.new(|cx| {
            ContextMenu::new(window, cx)
                .entry_if(has_selection, tr("Copy"), Copy)
                .entry_if(can_paste, tr("Paste"), Paste)
                .entry(tr("Select All"), SelectAll)
                .entry(tr("Clear"), Clear)
                .separator()
                .entry(tr("Split Right"), crate::terminal_group::SplitRight)
                .entry(tr("Split Down"), crate::terminal_group::SplitDown)
                .separator()
                .entry(tr("Close"), crate::terminal_group::ClosePane)
        });
        let focus = menu.focus_handle(cx);
        let subscriptions = [
            cx.subscribe_in(&menu, window, |this, menu, _: &DismissEvent, window, cx| {
                this.close_menu(menu, window, cx)
            }),
            cx.on_focus_out(&focus, window, {
                let menu = menu.clone();
                move |this, _, window, cx| this.close_menu(&menu, window, cx)
            }),
        ];
        window.focus(&focus);
        self.menu = Some(Menu {
            menu,
            position,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    /// Closes the menu (if it is still the same one); focus returns to the terminal if the menu had
    /// it.
    fn close_menu(
        &mut self,
        menu: &Entity<ContextMenu>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu.as_ref().is_none_or(|open| open.menu != *menu) {
            return;
        }
        let had_focus = menu.focus_handle(cx).contains_focused(window, cx);
        self.menu = None;
        if had_focus {
            window.focus(&self.focus_handle);
        }
        cx.notify();
    }

    // --- Cursor blinking ---

    /// Called when focus or window activity changes: the cursor is shown at once, and the blink
    /// timer runs only while the terminal is focused and the window is active.
    fn restart_blink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let blinking = self.focus_handle.is_focused(window)
            && window.is_window_active()
            && self.exited.is_none();
        self.cursor_visible = true;
        self.blink_task = theme::CURSOR_BLINK
            .filter(|_| blinking)
            .map(|period| Self::blink(period, cx));
        cx.notify();
    }

    /// Input: the cursor is shown at once; the next blink comes after a full period.
    fn pause_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_visible = true;
        if self.blink_task.is_some()
            && let Some(period) = theme::CURSOR_BLINK
        {
            self.blink_task = Some(Self::blink(period, cx));
        }
    }

    fn blink(period: Duration, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(period).await;
                let toggled = this.update(cx, |this, cx| {
                    this.cursor_visible = !this.cursor_visible;
                    cx.notify();
                });
                if toggled.is_err() {
                    break;
                }
            }
        })
    }

    // --- Display ---

    /// Under the output of a shell that failed right after starting: why the pane stayed, and how
    /// to close it.
    fn render_exit_note(&self, window: &Window, cx: &Context<Self>) -> Option<impl IntoElement> {
        let code = self.exited?;
        let ui = Theme::ui(cx);
        let message = match code {
            Some(code) => trf("The shell exited with code {0}", &[&code]),
            None => tr("The shell was terminated").to_string(),
        };
        let keys = ui::shortcut_in(
            &crate::terminal_group::ClosePane,
            &self.focus_handle,
            window,
        );
        Some(
            div()
                .flex_none()
                .mx_2()
                .mb_1p5()
                .px_2()
                .h(px(28.))
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(ui::RADIUS_SM))
                .bg(gpui::Hsla {
                    a: 0.12,
                    ..ui.warning
                })
                .font_family(theme::UI_FONT)
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.foreground)
                .child(icon(IconName::Warning, ui.warning).size(px(13.)))
                .child(div().flex_1().min_w_0().truncate().child(message))
                .children(keys.map(|keys| ui::hint_bar(&[(keys.as_ref(), tr("Close"))], ui))),
        )
    }
}

fn key_modifiers(modifiers: &gpui::Modifiers) -> Modifiers {
    Modifiers {
        control: modifiers.control,
        alt: modifiers.alt,
        shift: modifiers.shift,
        command: modifiers.platform,
    }
}

fn term_button(button: MouseButton) -> Option<mouse::MouseButton> {
    match button {
        MouseButton::Left => Some(mouse::MouseButton::Left),
        MouseButton::Middle => Some(mouse::MouseButton::Middle),
        MouseButton::Right => Some(mouse::MouseButton::Right),
        MouseButton::Navigate(_) => None,
    }
}

/// The terminal's palette from the theme (alpha dropped).
pub(crate) fn palette(theme: &Theme) -> Palette {
    let colors = &theme.terminal;
    Palette {
        foreground: rgb(colors.foreground),
        background: rgb(colors.background),
        cursor: rgb(colors.cursor),
        ansi: colors.ansi.map(rgb),
    }
}

fn rgb(color: Hsla) -> Rgb {
    let rgba = color.to_rgb();
    let channel = |c: f32| (c * 255.).round().clamp(0., 255.) as u8;
    Rgb::new(channel(rgba.r), channel(rgba.g), channel(rgba.b))
}

/// The IME sees an empty line: the terminal's text isn't editable, compositions are drawn at the
/// cursor and committed text is written to the program.
impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_text
            .as_ref()
            .map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked_text = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = None;
        if !text.is_empty() {
            self.terminal.input(text.as_bytes().to_vec());
        }
        self.typed(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = (!text.is_empty()).then(|| text.to_string());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.layout?.cursor_bounds()
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A program that takes the mouse gets an arrow, as in other terminals; ⌘ over a link — a hand.
        let takes_mouse = self
            .layout
            .is_some_and(|layout| layout.mode.mouse != flux_term::MouseMode::None);
        let pointer = if self.links.hovered.is_some() {
            CursorStyle::PointingHand
        } else if takes_mouse {
            CursorStyle::Arrow
        } else {
            CursorStyle::IBeam
        };
        div()
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.terminal.select_all();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Clear, _, cx| {
                this.terminal.clear();
                cx.notify();
            }))
            .on_action(
                cx.listener(|this, _: &ScrollPageUp, _, cx| this.scroll(ScrollDelta::PageUp, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ScrollPageDown, _, cx| {
                    this.scroll(ScrollDelta::PageDown, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &ScrollToTop, _, cx| this.scroll(ScrollDelta::Top, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ScrollToBottom, _, cx| this.scroll(ScrollDelta::Bottom, cx)),
            )
            .map(|root| crate::terminal_search::actions(root, cx))
            .children(crate::terminal_search::render_bar(self, window, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px_2()
                    .py_1()
                    .overflow_hidden()
                    .cursor(pointer)
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
                    .on_mouse_move(cx.listener(Self::mouse_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
                    .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
                    .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
                    // A drag released outside the terminal still ends there.
                    .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
                    .on_mouse_up_out(MouseButton::Right, cx.listener(Self::mouse_up))
                    .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::mouse_up))
                    .on_scroll_wheel(cx.listener(Self::scroll_wheel))
                    .on_modifiers_changed(cx.listener(|this, event, window, cx| {
                        crate::terminal_links::modifiers_changed(this, event, window, cx)
                    }))
                    .child(TerminalElement::new(cx.entity())),
            )
            .children(self.render_exit_note(window, cx))
            .children(
                self.menu
                    .as_ref()
                    .map(|menu| ContextMenu::overlay(&menu.menu, menu.position)),
            )
    }
}
