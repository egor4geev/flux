//! `dialogs.ask-form` (plugin API 0.3, part 10.3): a Flux dialog with several fields — a line of
//! text, several lines, a dropdown, checkboxes and notes between them — as the Open Task dialog of
//! JetBrains (the issue's state, a branch, the context) or a work log entry. A field can be enabled
//! by a checkbox (`enabled-by`); the plugin asks again with an `error` above the buttons when an
//! answer doesn't do. The answer goes back as `form-answered`: every field's value by its key (the
//! text, the chosen option's value, "true" / "false"), or none — cancelled. Forms wait for an open
//! modal as text questions do.
//!
//! The keyboard: Tab / ⇧Tab go from field to field (a disabled one is skipped), ↵ confirms (in a
//! field of several lines it starts a new line, and ⌘↵ confirms), Esc cancels, Space checks a
//! focused checkbox and opens a focused dropdown (↓ and ↑ too).
//!
//! A field of several lines is in the interface's font, growing with its text; with `code` — in
//! the code font, as the commit message (Flux's editor).
//!
//! Motion (ADR-040): an answered form stays a moment ([`RE_ASK_GRACE`], the answer gone at once):
//! a plugin that finds a value wrong asks again at once, and the same dialog takes the new form in
//! place — the error slides in and the dialog grows to it — instead of closing and opening again.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use flux_core::Document;
use flux_plugin::api::dialogs::{ChoiceOption, DialogLevel, FieldKind, Form, FormField};
use flux_plugin::api::events::Event;
use futures::channel::oneshot;
use gpui::{
    Action, AnyElement, App, Bounds, ClickEvent, Context, CursorStyle, DismissEvent, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, Global, KeyBinding, MouseButton, Pixels,
    Render, SharedString, Subscription, WeakEntity, Window, actions, canvas, div, point,
    prelude::*, px,
};

use crate::context_menu::ContextMenu;
use crate::editor::{self, Editor};
use crate::i18n::tr;
use crate::icons::{IconName, icon};
use crate::input::TextInput;
use crate::motion::{self, TransitionExt as _};
use crate::plugins::PendingQuestion;
use crate::prompt_input::PromptInput;
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, CheckState};
use crate::workspace::Workspace;

actions!(
    plugin_form,
    [
        Confirm,
        Dismiss,
        FocusNext,
        FocusPrevious,
        /// Space on a focused checkbox.
        ToggleFocused,
        /// Space, ↓ or ↑ on a focused dropdown.
        OpenChoice,
        /// ↵ in a field of several lines in the interface's font: a new line.
        NewLine,
    ]
);

/// A dropdown's option was chosen: (the field, the option).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = plugin_form, no_json)]
pub struct ChooseOption {
    pub field: usize,
    pub option: usize,
}

const WIDTH: f32 = 520.;
/// The column of the labels on the left of the fields.
const LABEL_WIDTH: f32 = 140.;
/// A field of several lines: about five lines of text.
const MULTILINE_HEIGHT: f32 = 100.;
/// A field of several lines in the interface's font grows to this many rows, then scrolls.
const LINES_ROWS: usize = 8;
/// The height of a field (as `TextInput`'s): a dropdown lines up with the text fields.
const FIELD_HEIGHT: f32 = 30.;

/// The form's keys are bound when the first form shows.
struct FormKeys;

impl Global for FormKeys {}

fn bind_keys(cx: &mut App) {
    if cx.has_global::<FormKeys>() {
        return;
    }
    cx.set_global(FormKeys);
    let form = Some("PluginForm");
    cx.bind_keys([
        KeyBinding::new("enter", Confirm, form),
        KeyBinding::new("cmd-enter", Confirm, form),
        KeyBinding::new("escape", Dismiss, form),
        KeyBinding::new("tab", FocusNext, form),
        KeyBinding::new("shift-tab", FocusPrevious, form),
    ]);
    let toggle = Some("PluginFormToggle");
    cx.bind_keys([KeyBinding::new("space", ToggleFocused, toggle)]);
    let choice = Some("PluginFormChoice");
    cx.bind_keys([
        KeyBinding::new("space", OpenChoice, choice),
        KeyBinding::new("down", OpenChoice, choice),
        KeyBinding::new("up", OpenChoice, choice),
    ]);
    cx.bind_keys([KeyBinding::new("enter", NewLine, Some("PluginFormLines"))]);
}

/// How long an answered form stays for its plugin to ask again (a wrong value) before it closes:
/// under what reads as at once, over what a plugin checking its values takes.
const RE_ASK_GRACE: Duration = Duration::from_millis(60);

/// A dimmed field: its checkbox (`enabled-by`) is off.
const DISABLED_OPACITY: f32 = 0.45;

thread_local! {
    /// The form answered last, staying for [`RE_ASK_GRACE`]: its plugin's next form takes it.
    static ANSWERED: RefCell<Option<(Arc<str>, WeakEntity<FormDialog>)>> =
        const { RefCell::new(None) };
}

/// Shows a plugin's form; the answer goes back to the plugin as `form-answered`. Over Settings
/// (the plugin's page asked for it) it opens on top of them; with another dialog or a popup of
/// the user's open, it waits for it. Asked again right after an answer, the answered dialog takes
/// it in place.
pub(crate) fn ask_form(
    workspace: &mut Workspace,
    plugin: &str,
    id: u64,
    form: &Form,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let plugin: Arc<str> = Arc::from(plugin);
    let answered = ANSWERED
        .with_borrow(|answered| {
            answered
                .as_ref()
                .filter(|(asker, _)| *asker == plugin)
                .and_then(|(_, dialog)| dialog.upgrade())
        })
        .filter(|dialog| dialog.read(cx).answered.is_some());
    if let Some(dialog) = answered {
        let (sender, receiver) = oneshot::channel::<Vec<(String, String)>>();
        dialog.update(cx, |dialog, cx| {
            dialog.ask_again(form.clone(), sender, window, cx)
        });
        send_answer(workspace, plugin, id, receiver, cx);
        return;
    }
    if !workspace.can_ask_plugin_question() {
        let pending = PendingQuestion::Form {
            plugin,
            id,
            form: form.clone(),
        };
        workspace
            .plugins
            .update(cx, |store, _| store.queue_question(pending));
    } else {
        show_form(workspace, plugin, id, form.clone(), window, cx);
    }
}

/// Shows a form now (the overlay window is free, or Settings are open with nothing over them).
pub(crate) fn show_form(
    workspace: &mut Workspace,
    plugin: Arc<str>,
    id: u64,
    form: Form,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    bind_keys(cx);
    let (sender, receiver) = oneshot::channel::<Vec<(String, String)>>();
    let asker = plugin.clone();
    workspace.open_plugin_dialog(window, cx, move |window, cx| {
        FormDialog::new(asker, form, sender, window, cx)
    });
    send_answer(workspace, plugin, id, receiver, cx);
}

/// Sends the plugin the answer the dialog gives through `receiver`.
fn send_answer(
    workspace: &Workspace,
    plugin: Arc<str>,
    id: u64,
    receiver: oneshot::Receiver<Vec<(String, String)>>,
    cx: &mut Context<Workspace>,
) {
    let plugins = workspace.plugins.downgrade();
    cx.spawn(async move |_, cx| {
        // Cancelled, the dialog drops the sender: the answer is none.
        let answer = receiver.await.ok();
        plugins
            .update(cx, |store, _| {
                store.send_to(&plugin, Event::FormAnswered((id, answer)))
            })
            .ok();
    })
    .detach();
}

/// A plugin's form.
pub struct FormDialog {
    /// The plugin that asks.
    plugin: Arc<str>,
    level: DialogLevel,
    title: SharedString,
    message: Option<SharedString>,
    error: Option<SharedString>,
    confirm: SharedString,
    fields: Vec<Field>,
    /// The dialog's own focus: the fields are inside it, so the window's modal layer knows the focus
    /// is the dialog's whichever field has it.
    focus_handle: FocusHandle,
    sender: Option<oneshot::Sender<Vec<(String, String)>>>,
    menu: Option<OpenMenu>,
    /// The window focuses the dialog itself: the first field takes the keyboard then (on the first
    /// frame — focus events don't come to a window that isn't active).
    focus_first: bool,
    /// When the form was answered: it stays [`RE_ASK_GRACE`] for its plugin to ask again.
    answered: Option<Instant>,
}

struct Field {
    key: String,
    label: SharedString,
    /// The key of the checkbox that enables the field.
    enabled_by: Option<String>,
    input: Input,
}

enum Input {
    Text(Entity<TextInput>),
    /// Several lines in the code font (`code`): Flux's editor.
    Multiline(Entity<Editor>),
    /// Several lines in the interface's font.
    Lines(Entity<PromptInput>),
    Choice {
        options: Vec<ChoiceOption>,
        selected: usize,
        focus: FocusHandle,
        /// Where the dropdown was drawn: its menu opens under it.
        bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    },
    Toggle {
        on: bool,
        focus: FocusHandle,
    },
    Note(SharedString),
}

/// A dropdown's open menu.
struct OpenMenu {
    menu: Entity<ContextMenu>,
    /// The dropdown's field: the keyboard goes back to it.
    field: usize,
    position: gpui::Point<Pixels>,
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<DismissEvent> for FormDialog {}

impl FormDialog {
    pub fn new(
        plugin: Arc<str>,
        form: Form,
        sender: oneshot::Sender<Vec<(String, String)>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            plugin,
            level: form.level,
            title: form.title.into(),
            message: form.message.map(Into::into),
            error: form.error.map(Into::into),
            confirm: form.confirm.into(),
            fields: Self::build_fields(form.fields, window, cx),
            focus_handle: cx.focus_handle(),
            sender: Some(sender),
            menu: None,
            focus_first: true,
            answered: None,
        }
    }

    /// The plugin asked again right after the answer (`error` says what was wrong): the dialog
    /// takes the new form in place, the focus staying on the field it was in.
    fn ask_again(
        &mut self,
        form: Form,
        sender: oneshot::Sender<Vec<(String, String)>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = self
            .focused_field(window, cx)
            .map(|index| self.fields[index].key.clone());
        self.level = form.level;
        self.title = form.title.into();
        self.message = form.message.map(Into::into);
        self.error = form.error.map(Into::into);
        self.confirm = form.confirm.into();
        self.fields = Self::build_fields(form.fields, window, cx);
        self.sender = Some(sender);
        self.menu = None;
        self.answered = None;
        let start = focused
            .and_then(|key| self.fields.iter().position(|field| field.key == key))
            .unwrap_or(0);
        self.focus_field(start, true, window, cx);
        cx.notify();
    }

    fn build_fields(
        fields: Vec<FormField>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Field> {
        fields
            .into_iter()
            .map(|field| {
                let placeholder: SharedString =
                    field.placeholder.clone().unwrap_or_default().into();
                let code = field.code;
                let input = match field.kind {
                    FieldKind::Text(text) => Input::Text(cx.new(|cx| {
                        let input = TextInput::new(placeholder, cx);
                        let mut input = if field.code { input.code() } else { input };
                        input.set_text(&text, cx);
                        input
                    })),
                    FieldKind::Multiline(text) if code => Input::Multiline(cx.new(|cx| {
                        let mut editor = Editor::new(Document::from_text(&text), window, cx);
                        editor.message = Some(placeholder);
                        editor
                    })),
                    FieldKind::Multiline(text) => Input::Lines(cx.new(|cx| {
                        let mut input = PromptInput::new(placeholder, cx).max_rows(LINES_ROWS);
                        input.set_text(&text, cx);
                        input
                    })),
                    FieldKind::Choice((options, value)) => {
                        let selected = value
                            .and_then(|value| options.iter().position(|o| o.value == value))
                            .unwrap_or(0);
                        Input::Choice {
                            options,
                            selected,
                            focus: cx.focus_handle(),
                            bounds: Rc::default(),
                        }
                    }
                    FieldKind::Toggle(on) => Input::Toggle {
                        on,
                        focus: cx.focus_handle(),
                    },
                    FieldKind::Note(text) => Input::Note(text.into()),
                };
                Field {
                    key: field.key,
                    label: field.label.into(),
                    enabled_by: field.enabled_by,
                    input,
                }
            })
            .collect()
    }

    /// Whether the field takes input now: its checkbox (`enabled-by`) is on.
    fn enabled(&self, index: usize) -> bool {
        let Some(key) = &self.fields[index].enabled_by else {
            return true;
        };
        self.fields
            .iter()
            .any(|field| field.key == *key && matches!(field.input, Input::Toggle { on: true, .. }))
    }

    fn field_focus(&self, index: usize, cx: &App) -> Option<FocusHandle> {
        match &self.fields[index].input {
            Input::Text(input) => Some(input.focus_handle(cx)),
            Input::Multiline(editor) => Some(editor.focus_handle(cx)),
            Input::Lines(input) => Some(input.focus_handle(cx)),
            Input::Choice { focus, .. } | Input::Toggle { focus, .. } => Some(focus.clone()),
            Input::Note(_) => None,
        }
    }

    /// The field that has the keyboard.
    fn focused_field(&self, window: &Window, cx: &App) -> Option<usize> {
        (0..self.fields.len()).find(|&index| {
            self.field_focus(index, cx)
                .is_some_and(|focus| focus.contains_focused(window, cx))
        })
    }

    /// Focuses the first enabled field from `start` on, forward or back (round the end).
    fn focus_field(
        &mut self,
        start: usize,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.fields.len();
        for step in 0..count {
            let index = if forward {
                (start + step) % count
            } else {
                (start + count - step % count) % count
            };
            if self.enabled(index)
                && let Some(focus) = self.field_focus(index, cx)
            {
                window.focus(&focus);
                cx.notify();
                return;
            }
        }
    }

    fn focus_next(&mut self, _: &FocusNext, window: &mut Window, cx: &mut Context<Self>) {
        let start = self.focused_field(window, cx).map_or(0, |index| index + 1);
        self.focus_field(start, true, window, cx);
    }

    fn focus_previous(&mut self, _: &FocusPrevious, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.fields.len();
        if count == 0 {
            return;
        }
        let start = self
            .focused_field(window, cx)
            .map_or(count - 1, |index| (index + count - 1) % count);
        self.focus_field(start, false, window, cx);
    }

    fn toggle(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.enabled(index) {
            return;
        }
        if let Input::Toggle { on, focus } = &mut self.fields[index].input {
            *on = !*on;
            window.focus(focus);
        }
        cx.notify();
    }

    fn toggle_focused(&mut self, _: &ToggleFocused, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.focused_field(window, cx) {
            self.toggle(index, window, cx);
        }
    }

    fn open_focused_choice(&mut self, _: &OpenChoice, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.focused_field(window, cx) {
            self.open_choice(index, window, cx);
        }
    }

    /// The dropdown's menu under it; the chosen option comes back as [`ChooseOption`].
    fn open_choice(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            return cx.notify();
        }
        if !self.enabled(index) {
            return;
        }
        let Input::Choice {
            options,
            selected,
            focus,
            bounds,
        } = &self.fields[index].input
        else {
            return;
        };
        // The menu sends the choice to whoever had the keyboard: the dropdown, inside the form.
        window.focus(focus);
        let Some(bounds) = bounds.get() else {
            return;
        };
        let (options, selected) = (options.clone(), *selected);
        let menu = cx.new(|cx| {
            let mut menu = ContextMenu::new(window, cx);
            for (option, choice) in options.iter().enumerate() {
                let label = if option == selected {
                    format!("✓  {}", choice.title)
                } else {
                    format!("    {}", choice.title)
                };
                menu = menu.entry(
                    label,
                    ChooseOption {
                        field: index,
                        option,
                    },
                );
            }
            menu
        });
        let menu_focus = menu.focus_handle(cx);
        let subscriptions = [
            cx.subscribe_in(&menu, window, |this, menu, _: &DismissEvent, window, cx| {
                this.close_menu(menu, window, cx)
            }),
            cx.on_focus_out(&menu_focus, window, {
                let menu = menu.clone();
                move |this, _, window, cx| this.close_menu(&menu, window, cx)
            }),
        ];
        window.focus(&menu_focus);
        self.menu = Some(OpenMenu {
            menu,
            field: index,
            position: point(bounds.left(), bounds.bottom() + px(4.)),
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    fn close_menu(
        &mut self,
        menu: &Entity<ContextMenu>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(open) = self.menu.take_if(|open| open.menu == *menu) else {
            return;
        };
        // Esc in the menu: the keyboard goes back to the dropdown (a choice has put it there
        // already).
        if menu.focus_handle(cx).contains_focused(window, cx)
            && let Some(focus) = self.field_focus(open.field, cx)
        {
            window.focus(&focus);
        }
        cx.notify();
    }

    fn choose(&mut self, action: &ChooseOption, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Field {
            input:
                Input::Choice {
                    options,
                    selected,
                    focus,
                    ..
                },
            ..
        }) = self.fields.get_mut(action.field)
            && action.option < options.len()
        {
            *selected = action.option;
            window.focus(focus);
        }
        cx.notify();
    }

    /// The values by the fields' keys (notes aren't fields).
    fn values(&self, cx: &App) -> Vec<(String, String)> {
        self.fields
            .iter()
            .filter_map(|field| {
                let value = match &field.input {
                    Input::Text(input) => input.read(cx).text(),
                    Input::Multiline(editor) => editor.read(cx).document.text().to_string(),
                    Input::Lines(input) => input.read(cx).text(),
                    Input::Choice {
                        options, selected, ..
                    } => options.get(*selected)?.value.clone(),
                    Input::Toggle { on, .. } => on.to_string(),
                    Input::Note(_) => return None,
                };
                Some((field.key.clone(), value))
            })
            .collect()
    }

    fn confirm(&mut self, _: &Confirm, _: &mut Window, cx: &mut Context<Self>) {
        if self.answered.is_some() {
            return;
        }
        let values = self.values(cx);
        if let Some(sender) = self.sender.take() {
            sender.send(values).ok();
        }
        if !motion::enabled() {
            cx.emit(DismissEvent);
            return;
        }
        // A moment for the plugin to ask again: then the dialog takes the new form in place.
        let at = Instant::now();
        self.answered = Some(at);
        ANSWERED.set(Some((self.plugin.clone(), cx.entity().downgrade())));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RE_ASK_GRACE).await;
            this.update(cx, |this, cx| {
                if this.answered == Some(at) {
                    cx.emit(DismissEvent);
                }
            })
            .ok();
        })
        .detach();
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        // The sender goes with the dialog: the answer is none.
        cx.emit(DismissEvent);
    }
}

impl Focusable for FormDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

// --- Drawing ---

impl FormDialog {
    fn render_field(
        &self,
        index: usize,
        labels: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let field = &self.fields[index];
        let enabled = self.enabled(index);
        let control: AnyElement = match &field.input {
            Input::Text(input) => input.clone().into_any_element(),
            Input::Multiline(editor) => {
                let focused = editor.focus_handle(cx).is_focused(window);
                let field = div()
                    .h(px(MULTILINE_HEIGHT))
                    .pl_1()
                    .pt_1()
                    .rounded(px(ui::RADIUS_MD))
                    .bg(ui.input_background)
                    .border_1()
                    // Tab goes to the next field rather than into the text.
                    .capture_action(cx.listener(|this, _: &editor::Tab, window, cx| {
                        cx.stop_propagation();
                        this.focus_next(&FocusNext, window, cx)
                    }))
                    .child(editor.clone());
                ui::focus_field(field, ("plugin-form-focus", index), focused, ui).into_any_element()
            }
            Input::Lines(input) => {
                let focus = input.focus_handle(cx);
                let focused = focus.is_focused(window);
                let target = input.clone();
                let field = div()
                    .key_context("PluginFormLines")
                    .min_h(px(MULTILINE_HEIGHT))
                    .px_2p5()
                    .py(px(6.))
                    .rounded(px(ui::RADIUS_MD))
                    .bg(ui.input_background)
                    .border_1()
                    .cursor(CursorStyle::IBeam)
                    // A click anywhere in the box puts the caret into the field.
                    .on_mouse_down(MouseButton::Left, move |_, window, _| window.focus(&focus))
                    .on_action(move |_: &NewLine, _, cx| {
                        target.update(cx, |input, cx| input.insert("\n", cx))
                    })
                    .child(input.clone());
                ui::focus_field(field, ("plugin-form-focus", index), focused, ui).into_any_element()
            }
            Input::Choice {
                options,
                selected,
                focus,
                bounds,
            } => self.render_choice(index, options, *selected, focus, bounds, ui, window, cx),
            Input::Toggle { on, focus } => {
                let focused = focus.is_focused(window);
                div()
                    .id(("plugin-form-toggle", index))
                    .key_context("PluginFormToggle")
                    .track_focus(focus)
                    .flex()
                    .items_center()
                    .gap_2()
                    .py_0p5()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.toggle(index, window, cx)
                    }))
                    .child(ui::focus_halo(
                        ui::checkbox(("plugin-form-check", index), CheckState::from_bool(*on), ui),
                        ("plugin-form-check-focus", index),
                        focused,
                        ui,
                    ))
                    .child(field.label.clone())
                    .into_any_element()
            }
            Input::Note(text) => div()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.text_muted)
                .child(text.clone())
                .into_any_element(),
        };
        // A disabled field: dimmed, and the mouse doesn't reach it. Its checkbox flipped, it dims
        // and comes back with a fade.
        let control = div()
            .relative()
            .flex_1()
            .min_w_0()
            .child(control)
            .when(!enabled, |control| {
                control.child(div().absolute().inset_0().occlude())
            })
            .with_transition(
                ("plugin-form-dim", index),
                if enabled { 1. } else { DISABLED_OPACITY },
                motion::FADE,
                |control, opacity| control.opacity(opacity),
            );
        let label = match &field.input {
            Input::Text(_) | Input::Multiline(_) | Input::Lines(_) | Input::Choice { .. } => {
                field.label.clone()
            }
            Input::Toggle { .. } | Input::Note(_) => SharedString::default(),
        };
        let multiline = matches!(field.input, Input::Multiline(_) | Input::Lines(_));
        div()
            .flex()
            .gap_3()
            .when(!multiline, |row| row.items_center())
            .when(labels, |row| {
                row.child(
                    div()
                        .flex_none()
                        .w(px(LABEL_WIDTH))
                        .when(multiline, |label| label.pt(px(6.)))
                        .text_color(if enabled { ui.text_muted } else { ui.dim })
                        .child(label),
                )
            })
            .child(control)
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_choice(
        &self,
        index: usize,
        options: &[ChoiceOption],
        selected: usize,
        focus: &FocusHandle,
        bounds: &Rc<Cell<Option<Bounds<Pixels>>>>,
        ui: UiColors,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focused = focus.is_focused(window);
        let title = options
            .get(selected)
            .map(|option| option.title.clone())
            .unwrap_or_default();
        let bounds = bounds.clone();
        let choice = div()
            .id(("plugin-form-choice", index))
            .key_context("PluginFormChoice")
            .track_focus(focus)
            .relative()
            .h(px(FIELD_HEIGHT))
            .px_2p5()
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(ui::RADIUS_MD))
            .bg(ui.input_background)
            .border_1()
            .when(!focused, |choice| {
                choice.hover(move |style| style.border_color(ui.elevated_border))
            })
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_choice(index, window, cx)
            }))
            .child(div().flex_1().min_w_0().truncate().child(title))
            .child(icon(IconName::ChevronDown, ui.text_muted).size(px(12.)))
            .child(
                canvas(move |area, _, _| bounds.set(Some(area)), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            );
        ui::focus_field(choice, ("plugin-form-choice-focus", index), focused, ui).into_any_element()
    }
}

impl Render for FormDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        if self.focus_first && self.focus_handle.is_focused(window) {
            self.focus_first = false;
            cx.defer_in(window, |this, window, cx| {
                this.focus_field(0, true, window, cx)
            });
        }
        // The label column shows when a field to its right has a label.
        let labels = self.fields.iter().any(|field| {
            !field.label.is_empty()
                && matches!(
                    field.input,
                    Input::Text(_) | Input::Multiline(_) | Input::Lines(_) | Input::Choice { .. }
                )
        });
        let fields: Vec<AnyElement> = (0..self.fields.len())
            .map(|index| self.render_field(index, labels, window, cx))
            .collect();
        let glyph = match self.level {
            DialogLevel::Info => None,
            DialogLevel::Warning => Some((IconName::Warning, ui.warning)),
            DialogLevel::Critical => Some((IconName::Error, ui.error)),
        };
        ui::popover(ui)
            .key_context("PluginForm")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.dismiss(cx)))
            // Esc in a field of several lines, with nothing to clear in it.
            .on_action(cx.listener(|this, _: &editor::Cancel, _, cx| this.dismiss(cx)))
            .on_action(cx.listener(Self::focus_next))
            .on_action(cx.listener(Self::focus_previous))
            .on_action(cx.listener(Self::toggle_focused))
            .on_action(cx.listener(Self::open_focused_choice))
            .on_action(cx.listener(Self::choose))
            .w(px(WIDTH))
            .max_w(gpui::relative(0.9))
            .p_5()
            .flex()
            .flex_col()
            .gap_3()
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_MD))
            .text_color(ui.foreground)
            .child(
                div()
                    .flex()
                    .gap_3()
                    .children(glyph.map(|(glyph, color)| {
                        div()
                            .flex_none()
                            .pt(px(1.))
                            .child(icon(glyph, color).size(px(20.)))
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(self.title.clone()),
                            )
                            .children(
                                self.message
                                    .clone()
                                    .map(|message| div().text_color(ui.text_muted).child(message)),
                            ),
                    ),
            )
            .when(!fields.is_empty(), |dialog| {
                dialog.child(div().flex().flex_col().gap_2p5().children(fields))
            })
            // Asked again with an error, the dialog grows to it and it slides in (another one too);
            // the wrapper's margin takes back the gap before it while there is none.
            .child(div().mt(px(-12.)).child(motion::glide_height(
                "plugin-form-error",
                div().children(self.error.clone().map(|error| {
                    motion::Appear::slide(0., -6.).apply(
                        error.clone(),
                        div()
                            .pt_3()
                            .flex()
                            .items_start()
                            .gap_2()
                            .text_color(ui.error)
                            .child(
                                div()
                                    .flex_none()
                                    .pt(px(2.))
                                    .child(icon(IconName::Error, ui.error).size(px(14.))),
                            )
                            .child(div().flex_1().min_w_0().child(error)),
                    )
                })),
            )))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .pt_1()
                    .child(
                        ui::text_button("plugin-form-cancel", tr("Cancel"), false, ui)
                            .h(px(28.))
                            .px(px(14.))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.dismiss(cx))),
                    )
                    .child(
                        ui::primary_button("plugin-form-confirm", self.confirm.clone(), true, ui)
                            .h(px(28.))
                            .px(px(14.))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.confirm(&Confirm, window, cx)
                            })),
                    ),
            )
            .child(ContextMenu::slot(
                "plugin-form-menu",
                self.menu.as_ref().map(|open| (&open.menu, open.position)),
            ))
    }
}
