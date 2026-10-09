//! The terminal panel: an island under the editor with terminal tabs (⌥F12 shows and hides it, ⌘T
//! opens a new terminal). Each tab is a [`TerminalGroup`]; a tab dragged onto the editor's tab
//! strip moves there, and back.
//!
//! The header is a strip of tab pills, like the editor's: a click activates, a middle click or ×
//! closes (asking first if a command runs in the tab), the right button opens the tab's menu, and
//! a pill can be dragged — along the strip to reorder, or onto the editor's strip. On the right:
//! split the active tab, and hide the panel. The panel's height follows a handle in the gap above
//! the island.

use std::path::{Path, PathBuf};

use gpui::{
    Action, AnyElement, App, Axis, ClickEvent, Context, CursorStyle, DismissEvent, DragMoveEvent,
    Entity, EventEmitter, FocusHandle, Focusable, FontWeight, KeyBinding, MouseButton,
    MouseDownEvent, MouseUpEvent, Pixels, Point, Render, ScrollHandle, SharedString, Subscription,
    Window, actions, div, prelude::*, px,
};

use crate::context_menu::ContextMenu;
use crate::dialog::Dialog;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::terminal_group::{
    DraggedTerminal, SplitDown, SplitRight, TerminalGroup, TerminalGroupEvent,
};
use crate::terminal_view::TerminalLink;
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, GAP, RADIUS_LG, RADIUS_MD};
use crate::workspace::{NextTab, PrevTab, tilde};

actions!(
    terminal,
    [
        NewTerminal,
        TogglePanel,
        HidePanel,
        MoveToEditor,
        MoveToPanel
    ]
);

// Commands of the panel's tabs (their menu).
actions!(terminal, [CloseTab, CloseOtherTabs]);

pub fn init(cx: &mut App) {
    let workspace = Some("Workspace");
    cx.bind_keys([
        // As in JetBrains IDEs: ⌘T is a new terminal in a terminal; elsewhere it is Update Project
        // (`git`).
        KeyBinding::new("cmd-t", NewTerminal, Some("Terminal")),
        // As the Terminal tool window in JetBrains IDEs.
        KeyBinding::new("alt-f12", TogglePanel, workspace),
        KeyBinding::new("shift-escape", HidePanel, Some("Terminal")),
    ]);
}

/// The tab strip at the top of the panel, and its pills.
const HEADER_HEIGHT: f32 = 38.;
const TAB_HEIGHT: f32 = 26.;
/// Long tab labels are shortened in the middle.
const TAB_LABEL_MAX_CHARS: usize = 28;
/// The height handle occupies the gap between the editor island and the panel: from the island's
/// border (1 px) for the height of the gap. Its middle is that much above the panel's top.
const RESIZE_HANDLE_HEIGHT: f32 = GAP;
const RESIZE_HANDLE_OFFSET: f32 = 1. + RESIZE_HANDLE_HEIGHT / 2.;
/// Hover groups: a pill (its × shows) and the resize handle (its line lights up).
const TAB_GROUP: &str = "terminal-tab";
const RESIZE_GROUP: &str = "terminal-panel-resize";

#[derive(Debug, Clone, PartialEq)]
pub enum TerminalPanelEvent {
    /// The last tab closed: the window hides the panel.
    Empty,
    /// ⌘-click on a link in a terminal.
    OpenLink(TerminalLink),
    /// A terminal tab from the editor area was dropped on the panel's tab strip at `index`: the
    /// window takes it out of the editor tabs and gives it to [`TerminalPanel::add_group`].
    MoveToPanel {
        group: Entity<TerminalGroup>,
        index: usize,
    },
    /// A new terminal's shell didn't start: the reason (also shown in the panel).
    ShellFailed(SharedString),
}

struct PanelTab {
    group: Entity<TerminalGroup>,
    _subscription: Subscription,
}

/// A tab's context menu: drawn at the click point.
struct Menu {
    menu: Entity<ContextMenu>,
    position: Point<Pixels>,
    _subscriptions: [Subscription; 2],
}

/// Dragging the panel's top edge.
#[derive(Debug, Clone, Copy)]
struct DraggedEdge;

impl Render for DraggedEdge {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// The window's terminal panel.
pub struct TerminalPanel {
    tabs: Vec<PanelTab>,
    active: usize,
    /// New terminals start here (the project root).
    root: Option<PathBuf>,
    /// Shared with the Git window: they take turns in the island under the editor.
    height: ui::BottomIslandHeight,
    /// The top edge is being dragged: the handle's line stays lit.
    resizing: bool,
    /// Focus of the empty panel.
    focus_handle: FocusHandle,
    /// Why the last terminal couldn't start.
    error: Option<SharedString>,
    tab_scroll: ScrollHandle,
    /// Where a dragged terminal tab would land in the strip: the index it would get.
    drop_index: Option<usize>,
    menu: Option<Menu>,
}

impl EventEmitter<TerminalPanelEvent> for TerminalPanel {}

impl TerminalPanel {
    pub fn new(
        root: Option<PathBuf>,
        height: ui::BottomIslandHeight,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            tabs: Vec::new(),
            active: 0,
            root,
            height,
            resizing: false,
            focus_handle: cx.focus_handle(),
            error: None,
            tab_scroll: ScrollHandle::new(),
            drop_index: None,
            menu: None,
        }
    }

    /// A new project root: where new terminals start.
    pub fn set_root(&mut self, root: Option<PathBuf>) {
        self.root = root;
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn groups(&self) -> Vec<Entity<TerminalGroup>> {
        self.tabs.iter().map(|tab| tab.group.clone()).collect()
    }

    pub fn active_group(&self) -> Option<Entity<TerminalGroup>> {
        self.tabs.get(self.active).map(|tab| tab.group.clone())
    }

    fn index_of(&self, group: &Entity<TerminalGroup>) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.group == *group)
    }

    /// ⌘T: a new terminal tab in the project root, focused.
    pub fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match TerminalGroup::spawn(self.root.clone(), self.root.clone(), window, cx) {
            Ok(group) => {
                self.error = None;
                self.add_group(group, None, window, cx);
            }
            Err(err) => {
                self.error = Some(trf("Couldn't start the shell: {0}", &[&err]).into());
                cx.emit(TerminalPanelEvent::ShellFailed(err.to_string().into()));
                cx.notify();
            }
        }
    }

    /// Adds a tab (a new one, or one moved from the editor area) at `index` (by default, after the
    /// active one) and focuses it.
    pub fn add_group(
        &mut self,
        group: Entity<TerminalGroup>,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let subscription = cx.subscribe_in(&group, window, Self::on_group_event);
        let index = index
            .unwrap_or(if self.tabs.is_empty() {
                0
            } else {
                self.active + 1
            })
            .min(self.tabs.len());
        self.tabs.insert(
            index,
            PanelTab {
                group,
                _subscription: subscription,
            },
        );
        self.activate(index, window, cx);
    }

    /// Takes a tab out of the panel without closing its terminals (it moves to the editor area).
    /// `false` if it isn't here.
    pub fn remove_group(&mut self, group: &Entity<TerminalGroup>, cx: &mut Context<Self>) -> bool {
        let Some(index) = self.index_of(group) else {
            return false;
        };
        self.tabs.remove(index);
        if index < self.active || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        cx.notify();
        true
    }

    /// Focuses the active terminal; an empty panel gets a new one.
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_group() {
            Some(group) => window.focus(&group.focus_handle(cx)),
            None => self.new_terminal(window, cx),
        }
    }

    /// Whether focus is somewhere in the panel: a terminal, the empty panel, a tab's menu.
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.contains_focused(window, cx)
            || self
                .tabs
                .iter()
                .any(|tab| tab.group.read(cx).contains_focus(window, cx))
            || self
                .menu
                .as_ref()
                .is_some_and(|menu| menu.menu.focus_handle(cx).is_focused(window))
    }

    /// Makes a tab active and moves focus into it; its bell mark goes away.
    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let group = tab.group.clone();
        self.active = index;
        self.tab_scroll.scroll_to_item(index);
        group.update(cx, |group, cx| group.clear_bell(cx));
        window.focus(&group.focus_handle(cx));
        cx.notify();
    }

    /// ⌘⇧] / ⌘⇧[ in the panel: the next or the previous tab, around the end.
    fn cycle(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.tabs.len() as isize;
        if len > 0 {
            let index = (self.active as isize + step).rem_euclid(len);
            self.activate(index as usize, window, cx);
        }
    }

    /// Closes a tab: its terminals end. Asks first if a command runs in it.
    fn close_tab(
        &mut self,
        group: Entity<TerminalGroup>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let confirm = group.update(cx, |group, cx| group.confirm_close(window, cx));
        cx.spawn_in(window, async move |this, cx| {
            if confirm.await {
                this.update_in(cx, |this, window, cx| this.discard(&group, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Closes every tab but the active one, asking once if commands run in them.
    fn close_other_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.active_group() else {
            return;
        };
        let others: Vec<_> = self.groups().into_iter().filter(|g| *g != active).collect();
        let running: Vec<String> = others
            .iter()
            .flat_map(|group| group.read(cx).running_processes(cx))
            .collect();
        let confirm = (!running.is_empty()).then(|| {
            crate::terminal_group::confirm_terminate(
                Dialog::warning(tr("Terminate running processes?")).message(trf(
                    "Still running in other tabs: {0}.",
                    &[&running.join(", ")],
                )),
                window,
                cx,
            )
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Some(answer) = confirm
                && !answer.await
            {
                return;
            }
            this.update_in(cx, |this, window, cx| {
                for group in &others {
                    this.discard(group, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Drops a tab whose terminals are done (closed, or the last pane's shell exited). Focus goes
    /// to the new active tab if it was in the dropped one; the last tab gone empties the panel.
    fn discard(
        &mut self,
        group: &Entity<TerminalGroup>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let had_focus = self.focus_handle.contains_focused(window, cx)
            || group.read(cx).contains_focus(window, cx);
        if !self.remove_group(group, cx) {
            return;
        }
        if self.tabs.is_empty() {
            return cx.emit(TerminalPanelEvent::Empty);
        }
        if had_focus {
            self.activate(self.active, window, cx);
        }
    }

    fn on_group_event(
        &mut self,
        group: &Entity<TerminalGroup>,
        event: &TerminalGroupEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TerminalGroupEvent::TitleChanged | TerminalGroupEvent::Bell => cx.notify(),
            TerminalGroupEvent::OpenLink(link) => {
                cx.emit(TerminalPanelEvent::OpenLink(link.clone()))
            }
            TerminalGroupEvent::Empty => self.discard(group, window, cx),
            TerminalGroupEvent::ShellFailed(reason) => {
                cx.emit(TerminalPanelEvent::ShellFailed(reason.clone()))
            }
        }
    }

    // --- Tab menu ---

    /// Right-click on a tab: it becomes active and focused (the menu's commands go to it), then
    /// the menu opens at the cursor.
    fn secondary_click(
        &mut self,
        index: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate(index, window, cx);
        let several = self.tabs.len() > 1;
        let menu = cx.new(|cx| {
            ContextMenu::new(window, cx)
                .entry(tr("Split Right"), SplitRight)
                .entry(tr("Split Down"), SplitDown)
                .separator()
                .entry(tr("Move to Editor"), MoveToEditor)
                .separator()
                .entry(tr("Close"), CloseTab)
                .entry_if(several, tr("Close Other Tabs"), CloseOtherTabs)
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

    /// Closes the menu (if it is still the same one); Esc in the menu returns focus to the tab.
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
        if had_focus && let Some(group) = self.active_group() {
            window.focus(&group.focus_handle(cx));
        }
        cx.notify();
    }

    // --- Dragging tabs ---

    /// A terminal tab is dragged over the pill at `index`: it would land before it, or after it in
    /// the pill's right half.
    fn drag_over_tab(
        &mut self,
        index: usize,
        event: &DragMoveEvent<DraggedTerminal>,
        cx: &mut Context<Self>,
    ) {
        let position = event.event.position;
        if !event.bounds.contains(&position) {
            return;
        }
        let after = position.x > event.bounds.center().x;
        self.set_drop_index(Some(index + usize::from(after)), cx);
    }

    /// Outside the header: nowhere.
    fn drag_over_header(&mut self, event: &DragMoveEvent<DraggedTerminal>, cx: &mut Context<Self>) {
        if !event.bounds.contains(&event.event.position) {
            self.set_drop_index(None, cx);
        }
    }

    /// Over the free space after the pills: at the end.
    fn drag_over_tail(&mut self, event: &DragMoveEvent<DraggedTerminal>, cx: &mut Context<Self>) {
        if event.bounds.contains(&event.event.position) {
            self.set_drop_index(Some(self.tabs.len()), cx);
        }
    }

    fn set_drop_index(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        if self.drop_index != index {
            self.drop_index = index;
            cx.notify();
        }
    }

    /// A terminal tab dropped on the strip: one of ours moves to its new place; one from the editor
    /// area is handed over by the window ([`TerminalPanelEvent::MoveToPanel`]).
    fn drop_tab(&mut self, dragged: &DraggedTerminal, window: &mut Window, cx: &mut Context<Self>) {
        let index = self.drop_index.take().unwrap_or(self.tabs.len());
        cx.notify();
        let group = dragged.group.clone();
        let Some(from) = self.index_of(&group) else {
            return cx.emit(TerminalPanelEvent::MoveToPanel { group, index });
        };
        // Taking the tab out shifts the places after it by one.
        let to = if from < index { index - 1 } else { index };
        if to == from {
            return self.activate(from, window, cx);
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.activate(to, window, cx);
    }

    // --- Display ---

    fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let titles: Vec<SharedString> = self
            .tabs
            .iter()
            .map(|tab| tab.group.read(cx).title(cx))
            .collect();
        let drop_index = self.drop_index.filter(|_| cx.has_active_drag());
        let tabs: Vec<AnyElement> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                // Tabs named alike tell themselves apart by their directory.
                let repeated = titles
                    .iter()
                    .enumerate()
                    .any(|(other, title)| other != index && *title == titles[index]);
                let detail = repeated
                    .then(|| tab.group.read(cx).cwd(cx))
                    .flatten()
                    .and_then(|cwd| directory_name(&cwd));
                self.render_tab(index, tab, titles[index].clone(), detail, drop_index, cx)
            })
            .collect();
        // The pills take what they need and scroll when they don't fit; "+" follows them, the
        // free space after it also takes a dropped tab (to the end).
        let strip = div()
            .id("terminal-tabs")
            .flex_initial()
            .min_w_0()
            .h_full()
            .pl_1p5()
            .pr_0p5()
            .flex()
            .items_center()
            .gap_1()
            .overflow_x_scroll()
            .track_scroll(&self.tab_scroll)
            .children(tabs);
        let new_terminal = ui::icon_button("terminal-new", IconName::Plus, ui)
            .tooltip(ui::tooltip(
                tr("New Terminal"),
                ui::shortcut_for(&NewTerminal, window),
            ))
            .on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| this.new_terminal(window, cx)),
            );
        let tail = div()
            .id("terminal-tabs-tail")
            .flex_1()
            .h_full()
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedTerminal>, _, cx| {
                    this.drag_over_tail(event, cx)
                }),
            );

        // Keys as in a terminal, even while focus is elsewhere.
        let terminal_focus = self.active_group().map(|group| group.focus_handle(cx));
        let keys = |action: &dyn Action| match &terminal_focus {
            Some(focus) => ui::shortcut_in(action, focus, window),
            None => ui::shortcut_for(action, window),
        };
        let error = (!self.tabs.is_empty())
            .then(|| self.error.clone())
            .flatten()
            .map(|error| {
                div()
                    .flex_none()
                    .max_w(px(320.))
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_color(ui.error)
                    .child(icon(IconName::Warning, ui.error).size(px(13.)))
                    .child(div().min_w_0().truncate().child(error))
            });
        let split = |id: &'static str, name: IconName, label: &'static str, axis: Axis| {
            ui::icon_button(id, name, ui)
                .tooltip(ui::tooltip(
                    label,
                    keys(if axis == Axis::Horizontal {
                        &SplitRight
                    } else {
                        &SplitDown
                    }),
                ))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    if let Some(group) = this.active_group() {
                        group.update(cx, |group, cx| group.split(axis, window, cx));
                    }
                }))
        };
        let has_tabs = !self.tabs.is_empty();
        div()
            .id("terminal-header")
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .pr(px(GAP - 2.))
            .flex()
            .items_center()
            .gap_1()
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedTerminal>, _, cx| {
                    this.drag_over_header(event, cx)
                }),
            )
            .on_drop(cx.listener(|this, dragged: &DraggedTerminal, window, cx| {
                this.drop_tab(dragged, window, cx)
            }))
            .child(strip)
            .child(new_terminal)
            .child(tail)
            .children(error)
            .when(has_tabs, |header| {
                header
                    .child(split(
                        "terminal-split-right",
                        IconName::SplitRight,
                        tr("Split Right"),
                        Axis::Horizontal,
                    ))
                    .child(split(
                        "terminal-split-down",
                        IconName::SplitDown,
                        tr("Split Down"),
                        Axis::Vertical,
                    ))
            })
            .child(
                ui::icon_button("terminal-hide", IconName::ChevronDown, ui)
                    .tooltip(ui::tooltip(tr("Hide"), keys(&HidePanel)))
                    .on_click(|_, window, cx| window.dispatch_action(HidePanel.boxed_clone(), cx)),
            )
    }

    fn render_tab(
        &self,
        index: usize,
        tab: &PanelTab,
        title: SharedString,
        detail: Option<String>,
        drop_index: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let active = index == self.active;
        let group = tab.group.clone();
        let bell = !active && group.read(cx).has_bell();
        let tooltip = group.read(cx).cwd(cx).map(|cwd| tilde(&cwd));
        let dragged = DraggedTerminal {
            group: group.clone(),
            title: title.clone(),
        };
        let close = {
            let group = group.clone();
            div()
                .id("close")
                .group("terminal-tab-close")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(ui::RADIUS_XS))
                .hover(move |style| style.bg(ui.pressed))
                .when(bell || !active, |close| {
                    close
                        .invisible()
                        .group_hover(TAB_GROUP, |style| style.visible())
                })
                // Clicking × must not activate the tab or start dragging it.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.close_tab(group.clone(), window, cx)
                }))
                .child(
                    icon(IconName::Close, ui.dim)
                        .size(px(12.))
                        .group_hover("terminal-tab-close", move |style| {
                            style.text_color(ui.foreground)
                        }),
                )
        };
        let bell_dot = bell.then(|| {
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .group_hover(TAB_GROUP, |style| style.invisible())
                .child(div().size(px(7.)).rounded(px(4.)).bg(ui.warning))
        });
        let activate = group.clone();
        let middle_close = group.clone();
        div()
            .id(("terminal-tab", group.entity_id()))
            .group(TAB_GROUP)
            .relative()
            .flex_none()
            .h(px(TAB_HEIGHT))
            .pl_2()
            .pr_1()
            .flex()
            .items_center()
            .gap_1p5()
            .rounded(px(RADIUS_MD))
            .border_1()
            .text_color(if active { ui.foreground } else { ui.text_muted })
            .when(active, |tab| {
                tab.bg(ui.pressed)
                    .border_color(ui.island_border)
                    .font_weight(FontWeight::MEDIUM)
            })
            .when(!active, |tab| {
                tab.border_color(gpui::transparent_black())
                    .hover(|style| style.bg(ui.hover).text_color(ui.foreground))
            })
            .when_some(tooltip, |tab, cwd| tab.tooltip(ui::tooltip(cwd, None)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    if let Some(index) = this.index_of(&activate) {
                        this.activate(index, window, cx)
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.secondary_click(index, event.position, window, cx)
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, _: &MouseUpEvent, window, cx| {
                    this.close_tab(middle_close.clone(), window, cx)
                }),
            )
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .on_drag_move(cx.listener(
                move |this, event: &DragMoveEvent<DraggedTerminal>, _, cx| {
                    this.drag_over_tab(index, event, cx)
                },
            ))
            .when(drop_index == Some(index), |tab| {
                tab.child(drop_marker(false, ui))
            })
            .when(
                drop_index == Some(self.tabs.len()) && index + 1 == self.tabs.len(),
                |tab| tab.child(drop_marker(true, ui)),
            )
            .child(icon(IconName::Terminal, ui.green).size(px(13.)))
            .child(
                div()
                    .whitespace_nowrap()
                    .child(shorten(&title, TAB_LABEL_MAX_CHARS)),
            )
            .children(detail.map(|detail| {
                div()
                    .whitespace_nowrap()
                    .text_color(ui.dim)
                    .child(shorten(&detail, TAB_LABEL_MAX_CHARS))
            }))
            .child(
                div()
                    .relative()
                    .flex_none()
                    .size(px(18.))
                    .children(bell_dot)
                    .child(close),
            )
            .into_any_element()
    }

    /// No tabs: why the last terminal didn't start (and a retry), or a way to open one.
    fn render_empty(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let (glyph, color, text) = match &self.error {
            Some(error) => (IconName::Warning, ui.error, error.clone()),
            None => (IconName::Terminal, ui.green, tr("No terminals").into()),
        };
        let button = if self.error.is_some() {
            ui::text_button("terminal-retry", tr("Try Again"), false, ui)
        } else {
            ui::text_button("terminal-open", tr("New Terminal"), false, ui)
        };
        let keys = self
            .error
            .is_none()
            .then(|| ui::shortcut_for(&NewTerminal, window))
            .flatten();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .size(px(36.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(RADIUS_MD))
                    .bg(UiColors::tint(color, 0.14))
                    .child(icon(glyph, color).size(px(18.))),
            )
            .child(
                div()
                    .max_w(px(520.))
                    .text_center()
                    .text_color(if self.error.is_some() {
                        ui.error
                    } else {
                        ui.text_muted
                    })
                    .child(text),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        button.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.new_terminal(window, cx)
                        })),
                    )
                    .children(keys.map(|keys| ui::keys(&keys, ui))),
            )
    }
}

/// The marker of where a dragged tab would land: an accent bar in the gap before a pill, or after
/// the last one (`after`).
fn drop_marker(after: bool, ui: UiColors) -> impl IntoElement {
    div()
        .absolute()
        .top(px(3.))
        .bottom(px(3.))
        .when(after, |marker| marker.right(px(-3.)))
        .when(!after, |marker| marker.left(px(-3.)))
        .w(px(2.))
        .rounded(px(1.))
        .bg(ui.accent)
}

/// The panel's top edge, in the gap between islands: an accent line on hover and while dragged.
fn resize_handle(resizing: bool, ui: UiColors) -> impl IntoElement {
    div()
        .id("terminal-panel-resize")
        .group(RESIZE_GROUP)
        .absolute()
        .top(px(-(1. + RESIZE_HANDLE_HEIGHT)))
        .left_0()
        .right_0()
        .h(px(RESIZE_HANDLE_HEIGHT))
        .px(px(RADIUS_LG))
        .flex()
        .items_center()
        .cursor(CursorStyle::ResizeUpDown)
        .on_drag(DraggedEdge, |_, _, _, cx| cx.new(|_| DraggedEdge))
        .child(
            div()
                .w_full()
                .h(px(2.))
                .rounded(px(1.))
                .bg(ui.focus_border)
                .when(!resizing, |line| {
                    line.invisible()
                        .group_hover(RESIZE_GROUP, |style| style.visible())
                }),
        )
}

/// The last component of a directory, for telling tabs apart.
fn directory_name(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    Some(name)
}

/// Shortens in the middle: "start…end".
fn shorten(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_string();
    }
    let head = (max_chars - 1) / 2;
    let tail = max_chars - 1 - head;
    let mut short: String = chars[..head].iter().collect();
    short.push('…');
    short.extend(&chars[chars.len() - tail..]);
    short
}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self.active_group() {
            Some(group) => group.focus_handle(cx),
            None => self.focus_handle.clone(),
        }
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        self.resizing &= cx.has_active_drag();
        // The window may have shrunk since the height was set.
        let max_height =
            f32::from(window.viewport_size().height) * ui::BottomIslandHeight::MAX_SHARE;
        let height = self.height.get_within(max_height);
        let body = match self.active_group() {
            Some(group) => div()
                .flex_1()
                .min_h_0()
                .pt_1()
                .pb_1p5()
                .child(group)
                .into_any_element(),
            None => self.render_empty(window, cx).into_any_element(),
        };
        div()
            .track_focus(&self.focus_handle)
            .relative()
            .h(px(height))
            .flex()
            .flex_col()
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_SM))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle(1, window, cx)))
            .on_action(cx.listener(|this, _: &PrevTab, window, cx| this.cycle(-1, window, cx)))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if let Some(group) = this.active_group() {
                    this.close_tab(group, window, cx)
                }
            }))
            .on_action(
                cx.listener(|this, _: &CloseOtherTabs, window, cx| {
                    this.close_other_tabs(window, cx)
                }),
            )
            .on_drag_move(
                cx.listener(move |this, event: &DragMoveEvent<DraggedEdge>, _, cx| {
                    // The handle sits in the gap above the edge: the edge follows the mouse
                    // without jumping.
                    let height = f32::from(event.bounds.bottom() - event.event.position.y)
                        - RESIZE_HANDLE_OFFSET;
                    this.height.set_within(height, max_height);
                    this.resizing = true;
                    cx.notify();
                }),
            )
            .child(self.render_header(window, cx))
            .child(ui::divider(ui).mx(px(GAP)))
            .child(body)
            .child(resize_handle(self.resizing, ui))
            .children(
                self.menu
                    .as_ref()
                    .map(|menu| ContextMenu::overlay(&menu.menu, menu.position)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_labels_are_shortened_in_the_middle() {
        assert_eq!(shorten("zsh", 8), "zsh");
        assert_eq!(shorten("very-long-process", 7), "ver…ess");
    }

    #[test]
    fn directory_name_is_the_last_component() {
        assert_eq!(
            directory_name(Path::new("/Users/me/dev/flux")).as_deref(),
            Some("flux")
        );
        assert_eq!(directory_name(Path::new("/")), None);
    }
}
