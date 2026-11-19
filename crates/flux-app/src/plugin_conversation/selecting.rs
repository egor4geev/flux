//! Selecting messages (API 0.5, part 5), as the messengers do: "Select" in a message's menu
//! (Flux's own, where the conversation has `selection-actions`) starts it with that message. A
//! click on a message then selects or clears it, a ⇧-click selects the run from the message
//! clicked last up to it (across days; what can't be selected left out), ⌘A every loaded one; Esc,
//! the bar's × or clearing the last one ends it. Notices and advertisements can't be selected.
//!
//! While selecting, every row has a circle at its left edge (the feed's column moves aside for
//! it, so the messages are laid out again once on the way in and once on the way out) — a check
//! in the accent for a selected one, whose row has the selection's tint across the feed — and a
//! cover over the row takes its clicks: a picture doesn't open, a link isn't followed, a button
//! isn't pressed, the hover bar doesn't show; the wheel goes through, so the feed scrolls and
//! loads on. The bot's keyboard and the field give way to the selection's bar, as tall as the
//! field: the count, the plugin's actions (`selection-action(id, the messages in the feed's
//! order)`, which ends the selection), Flux's own Copy — each message's text under its author
//! and time, as Telegram copies them — and ×. Nothing here asks for frames.

use std::collections::{HashMap, HashSet};

use flux_plugin::api::events::UiEvent;
use flux_plugin::api::ui::{Message, MessagePart};
use gpui::{
    AnyElement, ClickEvent, ClipboardItem, Context, FontWeight, MouseButton, MouseDownEvent,
    SharedString, Window, div, prelude::*, px,
};

use super::{COLUMN_PADDING, COLUMN_WIDTH, Conversation, civil_date, local_seconds, message_text};
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::theme::{self, UiColors};
use crate::ui;

/// Room for the circle at a row's left edge while selecting: the circle and the gap after it.
pub(super) const CHECK_SPACE: f32 = 30.;
const CHECK_SIZE: f32 = 20.;
/// The selection's bar: as tall as the field with one line.
const BAR_HEIGHT: f32 = 46.;

/// The messages selected, while selecting.
#[derive(Debug, Default)]
pub(super) struct Selecting {
    pub(super) chosen: HashSet<String>,
    /// The message clicked last: a ⇧-click selects the run from it.
    anchor: Option<String>,
}

impl Selecting {
    /// The message is selected.
    pub(super) fn has(&self, id: &str) -> bool {
        self.chosen.contains(id)
    }

    /// A message left the feed: it isn't selected any more.
    pub(super) fn forget(&mut self, id: &str) {
        self.chosen.remove(id);
        if self.anchor.as_deref() == Some(id) {
            self.anchor = None;
        }
    }

    /// A click on `id` (`shift` — a ⇧-click) among the feed's messages in `order`: a click selects
    /// the message or clears it; a ⇧-click selects every message that can be between the one
    /// clicked last and this one, both included (a plain click when there was none).
    pub(super) fn click(
        &mut self,
        id: &str,
        shift: bool,
        order: &[String],
        messages: &HashMap<String, Message>,
    ) {
        let can = |id: &String| messages.get(id).is_some_and(selectable);
        let from = self
            .anchor
            .as_ref()
            .filter(|_| shift)
            .and_then(|anchor| order.iter().position(|other| other == anchor));
        let to = order.iter().position(|other| other == id);
        match (from, to) {
            (Some(from), Some(to)) => {
                let run = &order[from.min(to)..=from.max(to)];
                self.chosen.extend(run.iter().filter(|id| can(id)).cloned());
            }
            _ if !messages.get(id).is_some_and(selectable) => return,
            _ => {
                if !self.chosen.remove(id) {
                    self.chosen.insert(id.to_string());
                }
            }
        }
        self.anchor = Some(id.to_string());
    }

    /// ⌘A: every loaded message that can be selected.
    pub(super) fn all(&mut self, order: &[String], messages: &HashMap<String, Message>) {
        self.chosen.extend(
            order
                .iter()
                .filter(|id| messages.get(*id).is_some_and(selectable))
                .cloned(),
        );
    }

    /// The selected messages in the feed's order.
    pub(super) fn in_order(&self, order: &[String]) -> Vec<String> {
        order
            .iter()
            .filter(|id| self.chosen.contains(*id))
            .cloned()
            .collect()
    }
}

/// A message that can be selected: neither a notice nor an advertisement.
pub(super) fn selectable(message: &Message) -> bool {
    message.sponsored.is_none() && !matches!(message.parts.first(), Some(MessagePart::Notice(_)))
}

/// The selected messages' text, as Telegram copies them: each one's author and time
/// ("Anna, [19.11.2026 14:32]") over its text, a blank line between them. `messages`: (the author,
/// the local time in seconds since 1970, the text).
pub(super) fn copied_text(messages: &[(String, i64, String)]) -> String {
    messages
        .iter()
        .map(|(author, local, text)| {
            let (year, month, day) = civil_date(local.div_euclid(86_400));
            let minutes = local.rem_euclid(86_400) / 60;
            format!(
                "{author}, [{day:02}.{month:02}.{year} {}:{:02}]\n{text}",
                minutes / 60,
                minutes % 60
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

impl Conversation {
    /// The message menu's "Select": selecting starts with the message; the keyboard goes to the
    /// feed (Esc, ⌘A).
    pub(super) fn start_selecting(
        &mut self,
        message: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.spec.selection_actions.is_empty() {
            return;
        }
        let mut selecting = Selecting::default();
        selecting.click(message, false, &self.order, &self.messages);
        if selecting.chosen.is_empty() {
            return;
        }
        self.selecting = Some(selecting);
        self.selection = None;
        self.remeasure_all();
        window.focus(&self.focus_handle);
        cx.notify();
    }

    /// Selecting ends: the rows lose their circles, the field comes back with the keyboard.
    pub(super) fn end_selecting(&mut self, cx: &mut Context<Self>) {
        if self.leave_selecting(cx) {
            self.take_keyboard = true;
        }
    }

    /// Selecting ends, the keyboard where it is: an action's picker or question takes it.
    fn leave_selecting(&mut self, cx: &mut Context<Self>) -> bool {
        if self.selecting.take().is_none() {
            return false;
        }
        self.remeasure_all();
        cx.notify();
        true
    }

    /// A click on a row while selecting; clearing the last selected message ends it.
    fn click_selecting(&mut self, id: &str, shift: bool, cx: &mut Context<Self>) {
        let Some(selecting) = self.selecting.as_mut() else {
            return;
        };
        selecting.click(id, shift, &self.order, &self.messages);
        if selecting.chosen.is_empty() {
            self.end_selecting(cx);
        }
        cx.notify();
    }

    /// ⌘A while selecting: every loaded message; otherwise the key goes on.
    pub(super) fn select_all_messages(
        &mut self,
        _: &super::SelectAllMessages,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.selecting.as_mut() {
            Some(selecting) => {
                selecting.all(&self.order, &self.messages);
                cx.notify();
            }
            None => cx.propagate(),
        }
    }

    /// An action of the bar: the plugin gets the messages, and selecting ends.
    fn choose_selection_action(&mut self, action: String, cx: &mut Context<Self>) {
        let Some(selecting) = self.selecting.as_ref() else {
            return;
        };
        let messages = selecting.in_order(&self.order);
        self.leave_selecting(cx);
        self.emit(UiEvent::SelectionAction((action, messages)), cx);
    }

    /// The bar's Copy: the selected messages' texts, each under its author and time.
    fn copy_selected(&mut self, cx: &mut Context<Self>) {
        let Some(selecting) = self.selecting.as_ref() else {
            return;
        };
        let messages: Vec<(String, i64, String)> = selecting
            .in_order(&self.order)
            .iter()
            .filter_map(|id| self.messages.get(id))
            .map(|message| {
                let author = match (&message.author, message.outgoing) {
                    (Some(author), _) => author.name.clone(),
                    (None, true) => tr("You").to_string(),
                    (None, false) => self
                        .title
                        .as_ref()
                        .map(|t| t.to_string())
                        .unwrap_or_default(),
                };
                (author, local_seconds(message.date), message_text(message))
            })
            .collect();
        cx.write_to_clipboard(ClipboardItem::new_string(copied_text(&messages)));
        self.end_selecting(cx);
    }

    /// A message's row while selecting: the circle at its left edge, the message beside it (in
    /// `column` less the circle's room) `top` under the row's top, the tint across the feed when
    /// it is selected, and the cover that takes its clicks.
    pub(super) fn render_selectable_row(
        &self,
        id: &str,
        column: f32,
        top: f32,
        body: AnyElement,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chosen = self.selecting.as_ref().is_some_and(|s| s.has(id));
        let can = self.messages.get(id).is_some_and(selectable);
        let circle = div()
            .flex_none()
            .size(px(CHECK_SIZE))
            .mb(px(3.))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .map(|circle| match (can, chosen) {
                (false, _) => circle,
                (true, true) => circle
                    .bg(ui.accent)
                    .child(icon(IconName::Check, ui.on_accent).size(px(12.))),
                (true, false) => circle
                    .border_1()
                    .border_color(UiColors::tint(ui.foreground, 0.35)),
            });
        let id = id.to_string();
        div()
            .id(SharedString::from(format!("fg-select-{id}")))
            .relative()
            .w_full()
            .flex()
            .justify_center()
            .pt(px(top))
            .when(chosen, |row| row.bg(UiColors::tint(ui.accent, 0.12)))
            .child(
                div()
                    .w(px(column))
                    .flex()
                    .items_end()
                    .child(div().flex_none().w(px(CHECK_SPACE)).child(circle))
                    .child(body),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .block_mouse_except_scroll()
                    .when(can, |cover| cover.cursor_pointer())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            window.focus(&this.focus_handle);
                            this.click_selecting(&id, event.modifiers.shift, cx)
                        }),
                    ),
            )
            .into_any_element()
    }

    /// The bar in place of the field while selecting: "Selected: 3", the plugin's actions, Copy,
    /// ×.
    pub(super) fn render_selection_bar(&self, ui: UiColors, cx: &mut Context<Self>) -> AnyElement {
        let count = self
            .selecting
            .as_ref()
            .map_or(0, |selecting| selecting.chosen.len());
        let actions = self.spec.selection_actions.iter().map(|action| {
            let label = action.label.clone().unwrap_or_else(|| action.id.clone());
            let element_id = SharedString::from(format!("fg-selection-{}", action.id));
            let button = if action.primary {
                ui::primary_button(element_id, label, true, ui)
            } else {
                ui::text_button(element_id, label, false, ui)
            };
            let action = action.id.clone();
            button.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.choose_selection_action(action.clone(), cx)
            }))
        });
        div()
            .w_full()
            .flex()
            .justify_center()
            .px(px(COLUMN_PADDING))
            .pb(px(12.))
            .pt(px(6.))
            .child(
                div()
                    .id("fg-selection-bar")
                    .w_full()
                    .max_w(px(COLUMN_WIDTH))
                    .h(px(BAR_HEIGHT))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .pl(px(14.))
                    .pr(px(8.))
                    .rounded(px(11.))
                    .bg(ui.input_background)
                    .border_1()
                    .border_color(ui.input_border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::TEXT_SM))
                            .font_weight(FontWeight::MEDIUM)
                            .child(trf("Selected: {0}", &[&count])),
                    )
                    .children(actions)
                    .child(
                        ui::text_button("fg-selection-copy", tr("Copy"), false, ui).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.copy_selected(cx)),
                        ),
                    )
                    .child(
                        ui::icon_button("fg-selection-close", IconName::Close, ui)
                            .tooltip(ui::tooltip(tr("Cancel"), Some("Esc".into())))
                            .on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.end_selecting(cx)),
                            ),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_plugin::api::ui::{MessageAuthor, RichSpan};

    fn text(id: &str, order: u64) -> Message {
        let mut message = notice(id, order);
        message.parts = vec![MessagePart::Text(vec![span(&format!("text {id}"))])];
        message
    }

    fn notice(id: &str, order: u64) -> Message {
        Message {
            id: id.to_string(),
            order,
            date: 1_763_550_000 + order as i64 * 86_400,
            outgoing: false,
            author: None,
            edited: false,
            delivery: None,
            forwarded: None,
            reply: None,
            parts: vec![MessagePart::Notice(vec![span("joined")])],
            reactions: Vec::new(),
            actions: None,
            views: None,
            topic: None,
            sponsored: None,
            keyboard: Vec::new(),
        }
    }

    fn span(text: &str) -> RichSpan {
        RichSpan {
            text: text.to_string(),
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            code: false,
            spoiler: false,
            highlight: false,
            link: None,
        }
    }

    /// m1 m2 (a notice) m3 m4 (an ad) m5, a day apart each.
    fn feed() -> (Vec<String>, HashMap<String, Message>) {
        let mut messages = vec![
            text("m1", 1),
            notice("m2", 2),
            text("m3", 3),
            text("m4", 4),
            text("m5", 5),
        ];
        messages[3].sponsored = Some("Ad".into());
        let order = messages.iter().map(|m| m.id.clone()).collect();
        (
            order,
            messages.into_iter().map(|m| (m.id.clone(), m)).collect(),
        )
    }

    fn chosen(selecting: &Selecting, order: &[String]) -> Vec<String> {
        selecting.in_order(order)
    }

    #[test]
    fn a_click_selects_and_clears_and_a_shift_click_selects_the_run() {
        let (order, messages) = feed();
        let mut selecting = Selecting::default();
        selecting.click("m5", false, &order, &messages);
        selecting.click("m3", false, &order, &messages);
        assert_eq!(chosen(&selecting, &order), ["m3", "m5"]);
        selecting.click("m5", false, &order, &messages);
        assert_eq!(chosen(&selecting, &order), ["m3"]);
        // ⇧-click from m5 (clicked last) back to m1, across the days: the notice and the ad stay
        // out.
        selecting.click("m5", false, &order, &messages);
        selecting.click("m1", true, &order, &messages);
        assert_eq!(chosen(&selecting, &order), ["m1", "m3", "m5"]);
        // A ⇧-click never clears.
        selecting.click("m3", true, &order, &messages);
        assert_eq!(chosen(&selecting, &order), ["m1", "m3", "m5"]);
    }

    #[test]
    fn notices_and_ads_cant_be_selected() {
        let (order, messages) = feed();
        let mut selecting = Selecting::default();
        selecting.click("m2", false, &order, &messages);
        selecting.click("m4", false, &order, &messages);
        assert!(selecting.chosen.is_empty());
        // A ⇧-click without a message clicked before is a click.
        selecting.click("m3", true, &order, &messages);
        assert_eq!(chosen(&selecting, &order), ["m3"]);
        selecting.all(&order, &messages);
        assert_eq!(chosen(&selecting, &order), ["m1", "m3", "m5"]);
        selecting.forget("m3");
        assert_eq!(chosen(&selecting, &order), ["m1", "m5"]);
    }

    #[test]
    fn copied_text_has_each_message_under_its_author_and_time() {
        let mut anna = text("m1", 1);
        anna.author = Some(MessageAuthor {
            name: "Anna".into(),
            color: 1,
            avatar: None,
        });
        // 2025-11-19 09:05 and 14:32 (local seconds).
        let morning = 1_763_543_100;
        let copied = copied_text(&[
            ("Anna".into(), morning, "Hi!\nHow are you?".into()),
            ("You".into(), morning + 5 * 3600 + 27 * 60, "Fine".into()),
        ]);
        assert_eq!(
            copied,
            "Anna, [19.11.2025 9:05]\nHi!\nHow are you?\n\nYou, [19.11.2025 14:32]\nFine"
        );
        assert!(selectable(&anna));
    }
}
