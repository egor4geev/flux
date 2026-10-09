//! A small dialog with a text field: a title, the field, a line that says what is wrong with the
//! text, optional checkboxes, Cancel and the main button (New Branch…, Rename…, Checkout Tag or
//! Revision…, Unstash as Branch…). Shown as an overlay window
//! (`Workspace::toggle_modal(window, cx, |window, cx| InputDialog::new(…)…)`); ↵ confirms when the
//! text is valid, Esc cancels.
//!
//! The owner says what is valid (`validate`, called on every change) and what to do with the text
//! and the checkboxes (`on_confirm`, called once; the dialog closes after it).

use gpui::{
    App, ClickEvent, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, KeyBinding, Render, SharedString, Subscription, Window, actions, div, prelude::*,
    px,
};

use crate::i18n::tr;
use crate::input::{InputEvent, TextInput};
use crate::theme::{self, Theme};
use crate::ui::{self, CheckState};

actions!(input_dialog, [Confirm, Dismiss]);

const WIDTH: f32 = 440.;

pub fn init(cx: &mut App) {
    let context = Some("InputDialog");
    cx.bind_keys([
        KeyBinding::new("enter", Confirm, context),
        KeyBinding::new("escape", Dismiss, context),
    ]);
}

type Validate = Box<dyn Fn(&str, &[bool], &App) -> Result<(), SharedString>>;
type OnConfirm = Box<dyn FnOnce(String, Vec<bool>, &mut Window, &mut App)>;

pub struct InputDialog {
    title: SharedString,
    /// A line under the title: what the dialog is about ("from main").
    subtitle: Option<SharedString>,
    input: Entity<TextInput>,
    checkboxes: Vec<(SharedString, bool)>,
    confirm_label: SharedString,
    validate: Option<Validate>,
    on_confirm: Option<OnConfirm>,
    /// Why the text can't be taken now; shown once the user typed something.
    error: Option<SharedString>,
    edited: bool,
    _subscription: Subscription,
}

impl EventEmitter<DismissEvent> for InputDialog {}

impl InputDialog {
    pub fn new(
        title: impl Into<SharedString>,
        placeholder: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = placeholder.into();
        let input = cx.new(|cx| TextInput::new(placeholder, cx).code());
        let subscription = cx.subscribe_in(&input, window, |this, _, event, _, cx| match event {
            InputEvent::Changed => {
                this.edited = true;
                this.check(cx);
            }
        });
        Self {
            title: title.into(),
            subtitle: None,
            input,
            checkboxes: Vec::new(),
            confirm_label: tr("OK").into(),
            validate: None,
            on_confirm: None,
            error: None,
            edited: false,
            _subscription: subscription,
        }
    }

    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// The initial text, all of it selected.
    pub fn text(self, text: &str, cx: &mut Context<Self>) -> Self {
        self.input.update(cx, |input, cx| {
            input.set_text(text, cx);
            input.select_all(cx);
        });
        self
    }

    pub fn checkbox(mut self, label: impl Into<SharedString>, checked: bool) -> Self {
        self.checkboxes.push((label.into(), checked));
        self
    }

    pub fn confirm_label(mut self, label: impl Into<SharedString>) -> Self {
        self.confirm_label = label.into();
        self
    }

    /// What is wrong with a text (and the checkboxes' state), if anything.
    pub fn validate(
        mut self,
        validate: impl Fn(&str, &[bool], &App) -> Result<(), SharedString> + 'static,
        cx: &mut Context<Self>,
    ) -> Self {
        self.validate = Some(Box::new(validate));
        self.check(cx);
        self
    }

    /// What to do with the text (trimmed) and the checkboxes.
    pub fn on_confirm(
        mut self,
        on_confirm: impl FnOnce(String, Vec<bool>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_confirm = Some(Box::new(on_confirm));
        self
    }

    fn values(&self) -> Vec<bool> {
        self.checkboxes
            .iter()
            .map(|(_, checked)| *checked)
            .collect()
    }

    fn check(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text();
        let values = self.values();
        self.error = match &self.validate {
            Some(validate) => validate(text.trim(), &values, cx).err(),
            None => None,
        };
        cx.notify();
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.check(cx);
        if self.error.is_some() {
            self.edited = true;
            return cx.notify();
        }
        let text = self.input.read(cx).text().trim().to_string();
        let values = self.values();
        cx.emit(DismissEvent);
        if let Some(on_confirm) = self.on_confirm.take() {
            on_confirm(text, values, window, cx);
        }
    }

    fn toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some((_, checked)) = self.checkboxes.get_mut(index) {
            *checked = !*checked;
        }
        self.check(cx);
    }
}

// Focus is the field's: nothing else in the dialog takes it, so clicks on the checkboxes and buttons
// keep typing in the field (and don't count as leaving the dialog).
impl Focusable for InputDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for InputDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let valid = self.error.is_none();
        let error = self.error.clone().filter(|_| self.edited);
        let checkboxes: Vec<_> = self
            .checkboxes
            .iter()
            .enumerate()
            .map(|(index, (label, checked))| {
                div()
                    .id(("input-dialog-check", index))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .cursor_pointer()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.text_muted)
                    .hover(move |style| style.text_color(ui.foreground))
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle(index, cx)),
                    )
                    .child(ui::checkbox(
                        ("input-dialog-box", index),
                        CheckState::from_bool(*checked),
                        ui,
                    ))
                    .child(label.clone())
            })
            .collect();
        ui::popover(ui)
            .key_context("InputDialog")
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm(window, cx)))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .w(px(WIDTH))
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.title.clone()),
                    )
                    .children(self.subtitle.clone().map(|subtitle| {
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .child(subtitle)
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(self.input.clone())
                    .children(error.map(|error| {
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.error)
                            .child(error)
                    })),
            )
            .when(!checkboxes.is_empty(), |dialog| {
                dialog.child(div().flex().flex_col().gap_1p5().children(checkboxes))
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        ui::text_button("input-dialog-cancel", tr("Cancel"), false, ui)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        ui::primary_button(
                            "input-dialog-ok",
                            self.confirm_label.clone(),
                            valid,
                            ui,
                        )
                        .when(valid, |button| {
                            button.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.confirm(window, cx)
                            }))
                        }),
                    ),
            )
    }
}
