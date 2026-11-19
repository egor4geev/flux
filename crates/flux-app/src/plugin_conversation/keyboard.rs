//! The buttons under a message (API 0.5, part 5): a bot's keyboard, an advertisement's button —
//! rows of them right under the bubble and as wide as it, a row's buttons sharing its width
//! evenly, as the design system's quiet buttons (a border, the hover's tint). A mark in a
//! button's corner tells what it does: ↗ opens a page, ↪ puts a text into the field; none — the
//! plugin answers it. A press comes as `keyboard-pressed(message, button)`.
//!
//! The button in place of the field (`field-button`: "Join" a channel, "Mute" one) is drawn here
//! too: a full-width flat button where the field would be — or, where the user can't write, the
//! plugin's line there (`field-note`), dim and in the middle; and a bot's keyboard over the field
//! (`reply-keyboard`): its rows between the feed and the field, the same buttons a little taller,
//! a row's sharing its width, scrolling when it is taller than two fifths of the conversation. The
//! field's ⌨ button shows and hides it; a press comes as `keyboard-pressed("", button)`. It is
//! drawn only when the conversation is, and asks for no frames of its own.

use flux_plugin::api::events::UiEvent;
use flux_plugin::api::ui::{Composed, KeyboardButton, KeyboardKind, ReplyKeyboard, RowAction};
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, SharedString, div, prelude::*, px, relative,
};

use super::{COLUMN_PADDING, COLUMN_WIDTH, Conversation};
use crate::icons::{IconName, icon};
use crate::motion;
use crate::theme::{self, UiColors};
use crate::ui;

/// A button's height, and the gap between buttons and rows.
const BUTTON_HEIGHT: f32 = 30.;
const GAP: f32 = 4.;
/// Room on either side of a button's text (its mark sits in the right one).
const BUTTON_PADDING_X: f32 = 14.;

/// The widths of a row of `count` buttons across `width`: even shares, [`GAP`] between them.
pub(super) fn row_widths(count: usize, width: f32) -> Vec<f32> {
    if count == 0 {
        return Vec::new();
    }
    let share = ((width - GAP * (count - 1) as f32) / count as f32).max(0.);
    vec![share; count]
}

/// The width a keyboard asks of its bubble, its buttons' texts measured (`texts`, by rows): the
/// widest row with each of its buttons as wide as the row's widest text needs.
pub(super) fn needed_width(texts: &[Vec<f32>]) -> f32 {
    texts
        .iter()
        .filter(|row| !row.is_empty())
        .map(|row| {
            let widest = row.iter().copied().fold(0., f32::max) + 2. * BUTTON_PADDING_X;
            widest * row.len() as f32 + GAP * (row.len() - 1) as f32
        })
        .fold(0., f32::max)
}

/// The height the buttons take under a bubble, the gap over them included: the avatar beside
/// the message stands this much higher, by the bubble's foot.
pub(super) fn height(rows: usize) -> f32 {
    rows as f32 * (BUTTON_HEIGHT + GAP)
}

/// A button's height in a bot's keyboard over the field.
const REPLY_BUTTON_HEIGHT: f32 = 34.;
/// The most of the conversation's height the bot's keyboard takes: taller, it scrolls.
const REPLY_MOST: f32 = 0.4;

/// What the field shows of the bot's keyboard (`open`: the user's ⌨): (its rows over the field,
/// the ⌨ button in the field). A keyboard without rows is only a placeholder.
pub(super) fn reply_keyboard_shows(keyboard: Option<&ReplyKeyboard>, open: bool) -> (bool, bool) {
    let rows = keyboard.is_some_and(|keyboard| keyboard.rows.iter().any(|row| !row.is_empty()));
    (rows && open, rows)
}

/// The field's placeholder: the bot's keyboard's while it is there, else the composer's.
pub(super) fn field_placeholder(
    composer: Option<&str>,
    keyboard: Option<&ReplyKeyboard>,
) -> String {
    keyboard
        .and_then(|keyboard| keyboard.placeholder.clone())
        .filter(|placeholder| !placeholder.is_empty())
        .or_else(|| composer.map(str::to_string))
        .unwrap_or_default()
}

impl Conversation {
    /// The message's buttons, `width` wide (the bubble's).
    pub(super) fn render_keyboard(
        &self,
        message: &str,
        rows: &[Vec<KeyboardButton>],
        width: f32,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .w(px(width))
            .flex()
            .flex_col()
            .gap(px(GAP))
            .children(rows.iter().enumerate().map(|(row_index, row)| {
                let widths = row_widths(row.len(), width);
                div()
                    .flex()
                    .gap(px(GAP))
                    .children(row.iter().zip(widths).map(|(button, button_width)| {
                        let (message, id) = (message.to_string(), button.id.clone());
                        let mark = match button.kind {
                            KeyboardKind::Action => None,
                            KeyboardKind::Link => Some(IconName::ArrowUpRight),
                            KeyboardKind::Field => Some(IconName::CornerDownRight),
                        };
                        div()
                            .id(SharedString::from(format!(
                                "fg-key-{message}-{row_index}-{}",
                                button.id
                            )))
                            .relative()
                            .flex_none()
                            .w(px(button_width))
                            .h(px(BUTTON_HEIGHT))
                            .px(px(BUTTON_PADDING_X))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(ui::RADIUS_MD))
                            .bg(UiColors::tint(ui.foreground, 0.05))
                            .border_1()
                            .border_color(ui.input_border)
                            .text_size(px(theme::TEXT_SM))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(ui.foreground)
                            .cursor_pointer()
                            .hover(move |style| style.bg(ui.hover))
                            .active(move |style| motion::press(style.bg(ui.pressed)))
                            // A press here isn't a selection of the feed's text.
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation()
                            })
                            .child(div().min_w_0().truncate().child(button.text.clone()))
                            .children(mark.map(|mark| {
                                div()
                                    .absolute()
                                    .top(px(3.))
                                    .right(px(4.))
                                    .child(icon(mark, ui.text_muted).size(px(10.)))
                            }))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.emit(
                                    UiEvent::KeyboardPressed((message.clone(), id.clone())),
                                    cx,
                                )
                            }))
                    }))
            }))
            .into_any_element()
    }

    /// The bot's keyboard over the field (part 5), when it shows: its rows across the column,
    /// scrolling past two fifths of the conversation's height.
    pub(super) fn render_reply_keyboard(
        &self,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let keyboard = self.reply_keyboard.as_ref();
        if !reply_keyboard_shows(keyboard, self.keyboard_open).0
            || self.spec.composer.is_none()
            || self.spec.field_button.is_some()
        {
            return None;
        }
        let rows = keyboard?.rows.iter().filter(|row| !row.is_empty());
        Some(
            div()
                .flex_none()
                .max_h(relative(REPLY_MOST))
                .w_full()
                .flex()
                .justify_center()
                .px(px(COLUMN_PADDING))
                .pt(px(6.))
                .child(
                    div()
                        .id("fg-reply-keyboard")
                        .w_full()
                        .max_w(px(COLUMN_WIDTH))
                        .max_h_full()
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap(px(GAP))
                        .children(rows.enumerate().map(|(row_index, row)| {
                            div().flex().gap(px(GAP)).children(row.iter().map(|button| {
                                let id = button.id.clone();
                                div()
                                    .id(SharedString::from(format!(
                                        "fg-reply-key-{row_index}-{}",
                                        button.id
                                    )))
                                    .flex_1()
                                    .min_w_0()
                                    .h(px(REPLY_BUTTON_HEIGHT))
                                    .px(px(BUTTON_PADDING_X))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(ui::RADIUS_MD))
                                    .bg(UiColors::tint(ui.foreground, 0.05))
                                    .border_1()
                                    .border_color(ui.input_border)
                                    .text_size(px(theme::TEXT_SM))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(ui.foreground)
                                    .cursor_pointer()
                                    .hover(move |style| style.bg(ui.hover))
                                    .active(move |style| motion::press(style.bg(ui.pressed)))
                                    .child(div().min_w_0().truncate().child(button.text.clone()))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.emit(
                                            UiEvent::KeyboardPressed((String::new(), id.clone())),
                                            cx,
                                        )
                                    }))
                            }))
                        })),
                )
                .into_any_element(),
        )
    }

    /// The line in place of the field where the user can't write (`field-note`), as tall as the
    /// button in its place.
    pub(super) fn render_field_note(&self, ui: UiColors) -> Option<AnyElement> {
        let note = self.spec.field_note.clone()?;
        Some(
            div()
                .w_full()
                .flex()
                .justify_center()
                .px(px(COLUMN_PADDING))
                .pb(px(12.))
                .pt(px(6.))
                .child(
                    div()
                        .w_full()
                        .max_w(px(COLUMN_WIDTH))
                        .h(px(40.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.text_muted)
                        .child(note),
                )
                .into_any_element(),
        )
    }

    /// The button in place of the field: its label in the accent, across the column.
    pub(super) fn render_field_button(
        &self,
        button: &RowAction,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = button.id.clone();
        div()
            .w_full()
            .flex()
            .justify_center()
            .px(px(COLUMN_PADDING))
            .pb(px(12.))
            .pt(px(6.))
            .child(
                div()
                    .id(SharedString::from(format!("fg-field-button-{}", button.id)))
                    .w_full()
                    .max_w(px(COLUMN_WIDTH))
                    .h(px(40.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(11.))
                    .bg(ui.input_background)
                    .border_1()
                    .border_color(ui.input_border)
                    .text_size(px(theme::TEXT_MD))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(ui.accent_text)
                    .cursor_pointer()
                    .hover(move |style| style.bg(ui.hover))
                    .active(move |style| motion::press(style.bg(ui.pressed)))
                    .child(button.label.clone().unwrap_or_default())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let composed = Composed {
                            text: String::new(),
                            files: Vec::new(),
                            reply_to: None,
                        };
                        this.emit(UiEvent::ComposerAction((id.clone(), composed)), cx)
                    })),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rows_buttons_share_its_width_evenly() {
        assert!(row_widths(0, 300.).is_empty());
        assert_eq!(row_widths(1, 300.), vec![300.]);
        // Two buttons: one gap between them.
        assert_eq!(row_widths(2, 300.), vec![148., 148.]);
        let three = row_widths(3, 300.);
        assert_eq!(three.len(), 3);
        assert!((three.iter().sum::<f32>() + 2. * GAP - 300.).abs() < 0.01);
        // Never below nothing, however narrow.
        assert_eq!(row_widths(4, 4.), vec![0.; 4]);
    }

    #[test]
    fn a_keyboard_asks_for_its_widest_row() {
        assert_eq!(needed_width(&[]), 0.);
        assert_eq!(needed_width(&[Vec::new()]), 0.);
        // One button: its text and the padding.
        assert_eq!(needed_width(&[vec![60.]]), 60. + 2. * BUTTON_PADDING_X);
        // A row of two takes twice its widest button and a gap; the widest row wins.
        let two = 2. * (80. + 2. * BUTTON_PADDING_X) + GAP;
        assert_eq!(needed_width(&[vec![60.], vec![30., 80.]]), two);
        assert_eq!(height(0), 0.);
        assert_eq!(height(2), 2. * (BUTTON_HEIGHT + GAP));
    }

    fn keyboard(rows: Vec<Vec<&str>>, placeholder: Option<&str>) -> ReplyKeyboard {
        ReplyKeyboard {
            rows: rows
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|text| KeyboardButton {
                            id: text.to_string(),
                            text: text.to_string(),
                            kind: KeyboardKind::Action,
                        })
                        .collect()
                })
                .collect(),
            placeholder: placeholder.map(str::to_string),
            open: true,
        }
    }

    #[test]
    fn a_bots_keyboard_shows_over_the_field_behind_its_button() {
        // None: no rows, no ⌨.
        assert_eq!(reply_keyboard_shows(None, true), (false, false));
        let two = keyboard(vec![vec!["Moscow", "Paris"], vec!["Cancel"]], None);
        assert_eq!(reply_keyboard_shows(Some(&two), true), (true, true));
        // Hidden behind ⌨: the button stays.
        assert_eq!(reply_keyboard_shows(Some(&two), false), (false, true));
        // A forced reply: no rows (or empty ones) — only the placeholder.
        let reply = keyboard(vec![Vec::new()], Some("Your city"));
        assert_eq!(reply_keyboard_shows(Some(&reply), true), (false, false));
    }

    #[test]
    fn the_bots_placeholder_takes_the_fields_place() {
        let reply = keyboard(Vec::new(), Some("Your city"));
        assert_eq!(
            field_placeholder(Some("Message…"), Some(&reply)),
            "Your city"
        );
        let plain = keyboard(vec![vec!["Yes"]], Some(""));
        assert_eq!(
            field_placeholder(Some("Message…"), Some(&plain)),
            "Message…"
        );
        assert_eq!(field_placeholder(Some("Message…"), None), "Message…");
        assert_eq!(field_placeholder(None, None), "");
    }
}
