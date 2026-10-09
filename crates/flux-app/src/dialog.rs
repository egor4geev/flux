//! Dialogs of Flux: questions and messages drawn by the window in the design system instead of
//! macOS alerts (NSAlert) and gpui's gray fallback. [`ask`] takes a [`Dialog`] — a title, a
//! message, details in the code font (git's output), buttons with roles, "Don't ask again" — and
//! resolves to the answer. Plain `window.prompt` calls go through the same dialog: [`init`]
//! installs it as the window prompt builder. Only file panels (Open, Save As) stay native.
//!
//! gpui keeps one prompt per window: a second `window.prompt` replaces the first, whose answer is
//! lost. So a window has one [`DialogLayer`] with a queue: every prompt gets a small [`Ticket`]
//! view (gpui answers the prompt when the ticket emits), the layer keeps the tickets alive and
//! shows the dialogs one at a time, oldest first. When an answered ticket leaves gpui's slot empty
//! and more dialogs wait, the layer puts itself back with one more `window.prompt`.
//!
//! Keys: ↵ and Space press the focused button (the main one at first), Esc — Cancel, Tab / ← → move
//! between the buttons, ⌘D — the destructive alternative of a dialog with a main button ("Don't
//! Save", as in macOS), ⌘C — copy the details.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, AppContext as _, AsyncWindowContext, ClickEvent, ClipboardItem, Context, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, KeyBinding, PromptButton,
    PromptHandle, PromptLevel, PromptResponse, Render, RenderablePromptHandle, ScrollHandle,
    SharedString, Window, WindowId, actions, div, prelude::*, px,
};

use crate::i18n::tr;
use crate::icons::{IconName, icon};
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, CheckState, RADIUS_MD};

actions!(
    dialog,
    [
        /// Presses the focused button.
        Confirm,
        /// Presses Cancel.
        Cancel,
        /// Presses the destructive alternative ("Don't Save").
        PressDanger,
        NextButton,
        PreviousButton,
        CopyDetails,
    ]
);

/// Width of a dialog, and of one with details.
const WIDTH: f32 = 420.;
const WIDE: f32 = 560.;
/// The details scroll beyond this height.
const DETAILS_HEIGHT: f32 = 220.;
/// The dialog's top edge: under the title bar, a little lower, as a sheet.
const TOP: f32 = ui::TITLE_BAR_HEIGHT + 64.;
/// The answer of a dialog closed by Esc without a Cancel button.
const DISMISSED: usize = usize::MAX;

/// What a button does to the dialog's look and keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonRole {
    /// The default answer: accent background, ↵.
    Primary,
    Normal,
    /// Destroys something (Force Push, Delete, Don't Save): red text; never the default.
    Danger,
    /// Backs out: Esc, closing the dialog.
    Cancel,
}

#[derive(Debug, Clone)]
pub struct DialogButton {
    pub label: SharedString,
    pub role: ButtonRole,
}

/// A question or a message.
#[derive(Debug, Clone)]
pub struct Dialog {
    pub level: PromptLevel,
    pub title: SharedString,
    /// Text under the title (wraps).
    pub message: Option<SharedString>,
    /// Long text in the code font with a scroll (git's output, a hook's output).
    pub details: Option<SharedString>,
    /// The answer is an index into this list. They are shown as in macOS and JetBrains IDEs: the
    /// main button on the right, Cancel next to it, a destructive alternative of a dialog with a
    /// main button ("Don't Save") on the left.
    pub buttons: Vec<DialogButton>,
    /// Shows the "Don't ask again" checkbox; its state comes back in [`DialogAnswer`].
    pub dont_ask_again: bool,
}

impl Dialog {
    pub fn new(level: PromptLevel, title: impl Into<SharedString>) -> Self {
        Self {
            level,
            title: title.into(),
            message: None,
            details: None,
            buttons: Vec::new(),
            dont_ask_again: false,
        }
    }

    pub fn info(title: impl Into<SharedString>) -> Self {
        Self::new(PromptLevel::Info, title)
    }

    pub fn warning(title: impl Into<SharedString>) -> Self {
        Self::new(PromptLevel::Warning, title)
    }

    #[allow(dead_code)] // Kept with info and warning: every level has a constructor.
    pub fn critical(title: impl Into<SharedString>) -> Self {
        Self::new(PromptLevel::Critical, title)
    }

    pub fn message(mut self, message: impl Into<SharedString>) -> Self {
        self.message = Some(message.into());
        self
    }

    pub fn details(mut self, details: impl Into<SharedString>) -> Self {
        self.details = Some(details.into());
        self
    }

    pub fn button(mut self, label: impl Into<SharedString>, role: ButtonRole) -> Self {
        self.buttons.push(DialogButton {
            label: label.into(),
            role,
        });
        self
    }

    pub fn primary(self, label: impl Into<SharedString>) -> Self {
        self.button(label, ButtonRole::Primary)
    }

    pub fn normal(self, label: impl Into<SharedString>) -> Self {
        self.button(label, ButtonRole::Normal)
    }

    pub fn danger(self, label: impl Into<SharedString>) -> Self {
        self.button(label, ButtonRole::Danger)
    }

    pub fn cancel(self, label: impl Into<SharedString>) -> Self {
        self.button(label, ButtonRole::Cancel)
    }

    pub fn dont_ask_again(mut self) -> Self {
        self.dont_ask_again = true;
        self
    }

    /// [`ask`], for the button only: `Some(index)`, `None` — dismissed.
    pub fn show(
        self,
        window: &mut Window,
        cx: &mut App,
    ) -> impl Future<Output = Option<usize>> + use<> {
        let answer = ask(self, window, cx);
        async move { answer.await.map(|answer| answer.button) }
    }

    /// [`Dialog::show`] from an async task of the window.
    pub async fn show_async(self, cx: &mut AsyncWindowContext) -> Option<usize> {
        ask_async(self, cx).await.map(|answer| answer.button)
    }

    /// A dialog from a plain `window.prompt`: Cancel buttons back out, the first other button is
    /// the main one.
    fn from_prompt(
        level: PromptLevel,
        message: &str,
        detail: Option<&str>,
        buttons: &[PromptButton],
    ) -> Self {
        let mut dialog = Dialog::new(level, message.to_string());
        dialog.message = detail.map(|detail| detail.to_string().into());
        let mut primary = false;
        for button in buttons {
            let role = if matches!(button, PromptButton::Cancel(_)) {
                ButtonRole::Cancel
            } else if !primary {
                primary = true;
                ButtonRole::Primary
            } else {
                ButtonRole::Normal
            };
            dialog = dialog.button(button.label().clone(), role);
        }
        dialog
    }

    fn has(&self, role: ButtonRole) -> bool {
        self.buttons.iter().any(|button| button.role == role)
    }

    /// Button indices left to right: the destructive alternatives on the left (when there is a main
    /// button), then Cancel, the rest, and the main button on the right.
    fn display_order(&self) -> (Vec<usize>, Vec<usize>) {
        let alternatives = self.has(ButtonRole::Primary);
        let indices = |role: ButtonRole| {
            self.buttons
                .iter()
                .enumerate()
                .filter(move |(_, button)| button.role == role)
                .map(|(index, _)| index)
        };
        let left: Vec<usize> = if alternatives {
            indices(ButtonRole::Danger).collect()
        } else {
            Vec::new()
        };
        let mut right: Vec<usize> = indices(ButtonRole::Cancel)
            .chain(indices(ButtonRole::Normal))
            .collect();
        if !alternatives {
            right.extend(indices(ButtonRole::Danger));
        }
        right.extend(indices(ButtonRole::Primary));
        (left, right)
    }

    /// The button focused first: the main one, else Cancel, else the rightmost.
    fn default_button(&self) -> Option<usize> {
        let position = |role| self.buttons.iter().position(|b| b.role == role);
        position(ButtonRole::Primary)
            .or_else(|| position(ButtonRole::Cancel))
            .or_else(|| self.display_order().1.last().copied())
            .or_else(|| self.display_order().0.first().copied())
    }

    /// Esc: Cancel; a dialog with a single button (a message) closes with it.
    fn cancel_button(&self) -> Option<usize> {
        self.buttons
            .iter()
            .position(|button| button.role == ButtonRole::Cancel)
            .or_else(|| (self.buttons.len() == 1).then_some(0))
    }

    /// ⌘D: the only destructive button of a dialog that has a main one ("Don't Save").
    fn danger_button(&self) -> Option<usize> {
        if !self.has(ButtonRole::Primary) {
            return None;
        }
        let mut danger = self
            .buttons
            .iter()
            .enumerate()
            .filter(|(_, button)| button.role == ButtonRole::Danger)
            .map(|(index, _)| index);
        match (danger.next(), danger.next()) {
            (Some(index), None) => Some(index),
            _ => None,
        }
    }
}

/// The button pressed and the "Don't ask again" checkbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialogAnswer {
    pub button: usize,
    pub dont_ask_again: bool,
}

/// A dialog on its way from [`ask`] to the prompt builder.
struct Request {
    dialog: Dialog,
    dont_ask_again: Rc<Cell<bool>>,
}

thread_local! {
    /// The dialog [`ask`] is showing: `window.prompt` only carries the title and the labels.
    static PENDING: RefCell<Option<Request>> = const { RefCell::new(None) };
    /// The next `window.prompt` only puts a layer back into gpui's slot.
    static REATTACH: Cell<bool> = const { Cell::new(false) };
    /// The layer of each window that has dialogs.
    static LAYERS: RefCell<HashMap<WindowId, Entity<DialogLayer>>> = RefCell::new(HashMap::new());
}

/// Installs the Flux dialog as the window prompt builder: every `window.prompt` is drawn by it.
pub fn init(cx: &mut App) {
    let context = Some("Dialog");
    cx.bind_keys([
        KeyBinding::new("enter", Confirm, context),
        KeyBinding::new("space", Confirm, context),
        KeyBinding::new("escape", Cancel, context),
        KeyBinding::new("cmd-d", PressDanger, context),
        KeyBinding::new("tab", NextButton, context),
        KeyBinding::new("right", NextButton, context),
        KeyBinding::new("shift-tab", PreviousButton, context),
        KeyBinding::new("left", PreviousButton, context),
        KeyBinding::new("cmd-c", CopyDetails, context),
    ]);
    cx.set_prompt_builder(build);
}

/// Shows `dialog` over the window; resolves to the answer, `None` if the dialog went away without
/// one (the window closed, Esc in a dialog without Cancel).
pub fn ask(
    dialog: Dialog,
    window: &mut Window,
    cx: &mut App,
) -> impl Future<Output = Option<DialogAnswer>> + use<> {
    let count = dialog.buttons.len();
    let buttons: Vec<PromptButton> = dialog
        .buttons
        .iter()
        .map(|button| match button.role {
            ButtonRole::Cancel => PromptButton::cancel(button.label.clone()),
            _ => PromptButton::new(button.label.clone()),
        })
        .collect();
    let level = dialog.level;
    let title = dialog.title.clone();
    let detail = dialog.message.clone().or(dialog.details.clone());
    let dont_ask_again = Rc::new(Cell::new(false));
    PENDING.with_borrow_mut(|pending| {
        *pending = Some(Request {
            dialog,
            dont_ask_again: dont_ask_again.clone(),
        })
    });
    let receiver = window.prompt(
        level,
        &title,
        detail.as_ref().map(|d| d.as_ref()),
        &buttons,
        cx,
    );
    // Another builder (the scenario auto-responder) leaves the request.
    PENDING.with_borrow_mut(|pending| pending.take());
    async move {
        let button = receiver.await.ok()?;
        (button < count).then(|| DialogAnswer {
            button,
            dont_ask_again: dont_ask_again.get(),
        })
    }
}

/// [`ask`] from an async task of the window.
pub async fn ask_async(dialog: Dialog, cx: &mut AsyncWindowContext) -> Option<DialogAnswer> {
    let answer = cx.update(|window, cx| ask(dialog, window, cx)).ok()?;
    answer.await
}

/// The prompt builder: queues the dialog in the window's layer and gives gpui a ticket for it.
fn build(
    level: PromptLevel,
    message: &str,
    detail: Option<&str>,
    buttons: &[PromptButton],
    handle: PromptHandle,
    window: &mut Window,
    cx: &mut App,
) -> RenderablePromptHandle {
    build_with(level, message, detail, buttons, handle, None, window, cx)
}

/// A prompt builder that answers by itself (UI scenarios): `choose` gives the button and how long
/// the dialog stays on screen before it is pressed.
pub(crate) type AutoAnswer<'a> = &'a mut dyn FnMut(&Dialog) -> (usize, Duration);

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_with(
    level: PromptLevel,
    message: &str,
    detail: Option<&str>,
    buttons: &[PromptButton],
    handle: PromptHandle,
    auto: Option<AutoAnswer>,
    window: &mut Window,
    cx: &mut App,
) -> RenderablePromptHandle {
    let window_id = window.window_handle().window_id();
    let layer = LAYERS.with_borrow(|layers| layers.get(&window_id).cloned());
    let layer = match layer {
        Some(layer) => layer,
        None => {
            let layer = cx.new(DialogLayer::new);
            LAYERS.with_borrow_mut(|layers| layers.insert(window_id, layer.clone()));
            layer
        }
    };
    let ticket = cx.new(|_| Ticket {
        layer: layer.clone(),
    });
    if !REATTACH.replace(false) {
        let request = PENDING
            .with_borrow_mut(|pending| pending.take())
            .unwrap_or_else(|| Request {
                dialog: Dialog::from_prompt(level, message, detail, buttons),
                dont_ask_again: Rc::default(),
            });
        let auto = auto.map(|choose| choose(&request.dialog));
        let focused = window.focused(cx);
        layer.update(cx, |layer, cx| {
            layer.push(request, ticket.clone(), focused, cx)
        });
        if let Some((button, delay)) = auto {
            auto_answer(layer, ticket.clone(), button, delay, window, cx);
        }
    }
    handle.with_view(ticket, window, cx)
}

/// Presses `button` of the ticket's dialog once it is shown and has been on screen for `delay`.
fn auto_answer(
    layer: Entity<DialogLayer>,
    ticket: Entity<Ticket>,
    button: usize,
    delay: Duration,
    window: &mut Window,
    cx: &mut App,
) {
    window
        .spawn(cx, async move |cx| {
            loop {
                let shown = layer
                    .update(cx, |layer, cx| {
                        let shown = layer
                            .queue
                            .front()
                            .is_some_and(|queued| queued.ticket == ticket);
                        if shown && layer.selected != Some(button) {
                            layer.selected = Some(button);
                            cx.notify();
                        }
                        shown
                    })
                    .unwrap_or(true);
                if shown {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
            }
            cx.background_executor().timer(delay).await;
            cx.update(|window, cx| {
                layer.update(cx, |layer, cx| layer.answer(button, window, cx))
            })
            .ok();
        })
        .detach();
}

/// What gpui holds in its prompt slot: the layer, and the answer of one dialog.
struct Ticket {
    layer: Entity<DialogLayer>,
}

impl EventEmitter<PromptResponse> for Ticket {}

impl Focusable for Ticket {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.layer.read(cx).focus_handle.clone()
    }
}

impl Render for Ticket {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.layer.clone()
    }
}

struct Queued {
    dialog: Dialog,
    ticket: Entity<Ticket>,
    dont_ask_again: Rc<Cell<bool>>,
}

/// The dialogs of a window: the first one shown, the rest wait.
struct DialogLayer {
    focus_handle: FocusHandle,
    queue: VecDeque<Queued>,
    /// Where the focus goes back when the last dialog closes.
    restore_focus: Option<FocusHandle>,
    /// The focused button of the shown dialog.
    selected: Option<usize>,
    dont_ask_again: bool,
    details_scroll: ScrollHandle,
}

impl DialogLayer {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            queue: VecDeque::new(),
            restore_focus: None,
            selected: None,
            dont_ask_again: false,
            details_scroll: ScrollHandle::new(),
        }
    }

    fn push(
        &mut self,
        request: Request,
        ticket: Entity<Ticket>,
        focused: Option<FocusHandle>,
        cx: &mut Context<Self>,
    ) {
        if self.queue.is_empty() {
            self.restore_focus = focused.filter(|focus| *focus != self.focus_handle);
            self.selected = request.dialog.default_button();
            self.dont_ask_again = false;
        }
        self.queue.push_back(Queued {
            dialog: request.dialog,
            ticket,
            dont_ask_again: request.dont_ask_again,
        });
        cx.notify();
    }

    fn answer(&mut self, button: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shown) = self.queue.pop_front() else {
            return;
        };
        shown.dont_ask_again.set(self.dont_ask_again);
        // gpui sends the answer, empties its slot and focuses what was focused when the ticket came.
        shown
            .ticket
            .update(cx, |_, cx| cx.emit(PromptResponse(button)));
        self.dont_ask_again = false;
        self.details_scroll = ScrollHandle::new();
        self.selected = self
            .queue
            .front()
            .and_then(|next| next.dialog.default_button());
        let more = !self.queue.is_empty();
        let restore = if more {
            None
        } else {
            self.restore_focus.take()
        };
        let window_id = window.window_handle().window_id();
        // After gpui's handling of the answer.
        window.defer(cx, move |window, cx| {
            if more {
                REATTACH.set(true);
                drop(window.prompt(PromptLevel::Info, "", None, &[tr("OK")], cx));
            } else {
                LAYERS.with_borrow_mut(|layers| layers.remove(&window_id));
                if let Some(focus) = restore {
                    window.focus(&focus);
                }
            }
        });
        cx.notify();
    }

    fn shown(&self) -> Option<&Dialog> {
        self.queue.front().map(|queued| &queued.dialog)
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(button) = self.selected {
            self.answer(button, window, cx);
        }
    }

    fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        let button = self
            .shown()
            .map(|dialog| dialog.cancel_button().unwrap_or(DISMISSED));
        if let Some(button) = button {
            self.answer(button, window, cx);
        }
    }

    fn press_danger(&mut self, _: &PressDanger, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(button) = self.shown().and_then(Dialog::danger_button) {
            self.answer(button, window, cx);
        }
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(dialog) = self.shown() else {
            return;
        };
        let (left, right) = dialog.display_order();
        let order: Vec<usize> = left.into_iter().chain(right).collect();
        if order.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|selected| order.iter().position(|&index| index == selected))
            .unwrap_or(0) as isize;
        let next = (at + delta).rem_euclid(order.len() as isize) as usize;
        self.selected = Some(order[next]);
        cx.notify();
    }

    fn copy_details(&mut self, _: &CopyDetails, _: &mut Window, cx: &mut Context<Self>) {
        let text = self
            .shown()
            .and_then(|dialog| dialog.details.clone().or(dialog.message.clone()));
        if let Some(text) = text {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        }
    }

    fn render_button(
        &self,
        index: usize,
        button: &DialogButton,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let id = ("dialog-button", index);
        let element = match button.role {
            ButtonRole::Primary => ui::primary_button(id, button.label.clone(), true, ui),
            ButtonRole::Danger => ui::text_button(id, button.label.clone(), true, ui),
            ButtonRole::Normal | ButtonRole::Cancel => {
                ui::text_button(id, button.label.clone(), false, ui)
            }
        };
        element
            .h(px(28.))
            .px(px(14.))
            .text_size(px(theme::TEXT_MD))
            .when(self.selected == Some(index), |button| {
                button.shadow(ui::focus_ring(ui))
            })
            .on_click(
                cx.listener(move |this, _: &ClickEvent, window, cx| this.answer(index, window, cx)),
            )
    }

    fn render_details(
        &self,
        details: &SharedString,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .relative()
            .mt_3()
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(ui.input_border)
            .bg(ui.island)
            .child(
                div()
                    .id("dialog-details")
                    .max_h(px(DETAILS_HEIGHT))
                    .overflow_y_scroll()
                    .track_scroll(&self.details_scroll)
                    .px_3()
                    .py_2()
                    .pr(px(64.))
                    .font_family(theme::code_font())
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.foreground)
                    .children(
                        details
                            .lines()
                            .map(|line| div().min_h(px(16.)).child(line.to_string())),
                    ),
            )
            .child(
                div().absolute().top(px(6.)).right(px(6.)).child(
                    ui::text_button("dialog-copy", tr("Copy"), false, ui)
                        .h(px(22.))
                        .px(px(8.))
                        .bg(ui.elevated)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.copy_details(&CopyDetails, window, cx)
                        })),
                ),
            )
    }
}

impl Focusable for DialogLayer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DialogLayer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let root = div()
            .id("dialog-layer")
            .key_context("Dialog")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::press_danger))
            .on_action(cx.listener(|this, _: &NextButton, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &PreviousButton, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(Self::copy_details))
            .absolute()
            .size_full()
            // Modal: the window under the dialog takes no clicks, scrolls or hovers.
            .occlude()
            .bg(gpui::black().opacity(0.28))
            .flex()
            .flex_col()
            .items_center()
            .pt(px(TOP))
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_MD))
            .text_color(ui.foreground);
        let Some(dialog) = self.shown().cloned() else {
            return root;
        };
        let (glyph, color) = match dialog.level {
            PromptLevel::Info => (IconName::Info, ui.info),
            PromptLevel::Warning => (IconName::Warning, ui.warning),
            PromptLevel::Critical => (IconName::Error, ui.error),
        };
        let (left, right) = dialog.display_order();
        let buttons = |indices: Vec<usize>, this: &Self, cx: &mut Context<Self>| {
            indices
                .into_iter()
                .map(|index| {
                    this.render_button(index, &dialog.buttons[index], cx)
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        };
        let left = buttons(left, self, cx);
        let right = buttons(right, self, cx);
        let details = dialog
            .details
            .as_ref()
            .map(|details| self.render_details(details, ui, cx));
        let dont_ask = dialog.dont_ask_again.then(|| {
            div()
                .id("dialog-dont-ask")
                .mt_4()
                .flex()
                .items_center()
                .gap_1p5()
                .cursor_pointer()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.text_muted)
                .hover(move |style| style.text_color(ui.foreground))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.dont_ask_again = !this.dont_ask_again;
                    cx.notify();
                }))
                .child(ui::checkbox(
                    "dialog-dont-ask-box",
                    CheckState::from_bool(self.dont_ask_again),
                    ui,
                ))
                .child(tr("Don’t ask again"))
        });
        let width = if dialog.details.is_some() {
            WIDE
        } else {
            WIDTH
        };
        root.child(
            ui::popover(ui)
                .w(px(width))
                .max_w(gpui::relative(0.9))
                .p_5()
                .flex()
                .flex_col()
                // Clicks on the dialog stay on it.
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .child(
                            div()
                                .flex_none()
                                .pt(px(1.))
                                .child(icon(glyph, color).size(px(20.))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1p5()
                                .child(
                                    div()
                                        .text_size(px(theme::TEXT_LG))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(dialog.title.clone()),
                                )
                                .children(dialog.message.clone().map(|message| {
                                    div().text_color(ui.text_muted).children(
                                        message
                                            .split("\n\n")
                                            .map(|paragraph| {
                                                div().pb_1().child(paragraph.to_string())
                                            })
                                            .collect::<Vec<_>>(),
                                    )
                                }))
                                .children(details)
                                .children(dont_ask),
                        ),
                )
                .child(
                    div()
                        .mt_5()
                        .flex()
                        .items_center()
                        .gap_2()
                        .children(left)
                        .child(div().flex_1())
                        .children(right),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn save() -> Dialog {
        Dialog::warning("Save changes?")
            .primary("Save")
            .danger("Don't Save")
            .cancel("Cancel")
    }

    #[test]
    fn the_main_button_is_on_the_right_and_its_alternative_on_the_left() {
        assert_eq!(save().display_order(), (vec![1], vec![2, 0]));
        assert_eq!(save().default_button(), Some(0));
        assert_eq!(save().cancel_button(), Some(2));
        assert_eq!(save().danger_button(), Some(1));
    }

    #[test]
    fn a_destructive_question_focuses_cancel() {
        let delete = Dialog::warning("Delete?").danger("Delete").cancel("Cancel");
        assert_eq!(delete.display_order(), (vec![], vec![1, 0]));
        assert_eq!(delete.default_button(), Some(1));
        // ⌘D is only for an alternative to a main button.
        assert_eq!(delete.danger_button(), None);
    }

    #[test]
    fn a_message_closes_with_esc() {
        let message = Dialog::info("Done").primary("OK");
        assert_eq!(message.cancel_button(), Some(0));
        let two = Dialog::info("?").primary("Apply").normal("Later");
        assert_eq!(two.cancel_button(), None);
    }

    #[test]
    fn plain_prompts_get_roles() {
        let dialog = Dialog::from_prompt(
            PromptLevel::Warning,
            "Q",
            Some("d"),
            &[
                PromptButton::new("A"),
                PromptButton::new("B"),
                PromptButton::cancel("C"),
            ],
        );
        let roles: Vec<_> = dialog.buttons.iter().map(|b| b.role).collect();
        assert_eq!(
            roles,
            [ButtonRole::Primary, ButtonRole::Normal, ButtonRole::Cancel]
        );
        assert_eq!(dialog.message.as_ref().map(|m| m.as_ref()), Some("d"));
    }
}
