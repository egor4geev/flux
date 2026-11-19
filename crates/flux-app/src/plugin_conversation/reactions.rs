//! Reactions (API 0.5, FluxGram part 5): the chips under a message — the emoji and the count, the
//! user's own in the accent — and the row of the reactions the user may put (`conversation-spec.
//! reactions`). The message's hover bar has a ☺ for it: the row opens over the ☺ as the design
//! system's popups do, the emoji at a comfortable size; when there are more than [`ROW`] a ⌄ at
//! its end opens all of them as a grid that scrolls. A click on one of them — or on a chip under
//! a message whose emoji is in the list (those have a hover state) — comes as
//! `reaction-toggled(message, emoji)`: the plugin puts the user's own, or takes it back. Paid and
//! custom reactions the plugin leaves out of the list are only shown.
//!
//! The row is drawn only while it is open; nothing asks for frames.

use flux_plugin::api::events::UiEvent;
use flux_plugin::api::ui::Message;
use gpui::{
    AnyElement, ClickEvent, Context, Corner, MouseButton, Pixels, Point, ScrollHandle,
    SharedString, Window, anchored, deferred, div, point, prelude::*, px,
};

use super::Conversation;
use crate::i18n::tr;
use crate::icons::{IconName, icon};
use crate::popup;
use crate::theme::{self, Theme, UiColors};
use crate::ui;

/// The row shows this many reactions; more — a ⌄ opens the grid.
const ROW: usize = 7;
/// The grid's columns and the rows in sight (the rest scroll).
const COLUMNS: usize = 8;
const GRID_ROWS: usize = 5;
/// A reaction's square, and the emoji in it.
const CELL: f32 = 34.;
const EMOJI: f32 = 20.;
/// The popup's inner padding.
const PADDING: f32 = 4.;

/// The open row of reactions: for which message, where its ☺ was clicked.
pub(super) struct Reacting {
    message: String,
    at: Point<Pixels>,
    /// All of them, as a grid.
    expanded: bool,
    scroll: ScrollHandle,
}

impl Conversation {
    /// Opens the row of reactions for `message` over the ☺ clicked at `at`.
    pub(super) fn open_reactions(
        &mut self,
        message: &str,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.reacting = Some(Reacting {
            message: message.to_string(),
            at,
            expanded: false,
            scroll: ScrollHandle::new(),
        });
        cx.notify();
    }

    /// The user's reaction `emoji` on `message`, put or taken back: the plugin says which.
    fn toggle_reaction(&mut self, message: &str, emoji: &str, cx: &mut Context<Self>) {
        self.reacting = None;
        self.emit(
            UiEvent::ReactionToggled((message.to_string(), emoji.to_string())),
            cx,
        );
        cx.notify();
    }

    /// The ☺ of the hover bar, where the user may put reactions.
    pub(super) fn render_react_button(
        &self,
        message: &Message,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.spec.reactions.is_empty() || !reacts(message) {
            return None;
        }
        let id = message.id.clone();
        Some(
            div()
                .id(SharedString::from(format!("fg-react-{id}")))
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(ui::RADIUS_SM))
                .hover(|style| style.bg(ui.hover))
                .child(icon(IconName::Smile, ui.text_muted))
                // Not over its own row: the two would take turns drawing.
                .when(self.reacting.is_none(), |button| {
                    button.tooltip(ui::tooltip(tr("Add Reaction"), None))
                })
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.open_reactions(&id, event.position(), cx)
                }))
                .into_any_element(),
        )
    }

    /// The reactions under a message: a chip whose emoji the user may put toggles it.
    pub(super) fn render_reaction_chips(
        &self,
        message: &Message,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_wrap()
            .gap(px(4.))
            .children(
                message
                    .reactions
                    .iter()
                    .enumerate()
                    .map(|(index, reaction)| {
                        let toggles =
                            reacts(message) && self.spec.reactions.contains(&reaction.emoji);
                        let chip = div()
                            .id(("fg-reaction", index))
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .px(px(7.))
                            .py(px(1.))
                            .rounded(px(10.))
                            .text_size(px(theme::TEXT_SM))
                            .border_1()
                            .map(|chip| {
                                if reaction.chosen {
                                    chip.bg(UiColors::tint(ui.accent, 0.25))
                                        .border_color(UiColors::tint(ui.accent, 0.6))
                                        .text_color(ui.accent_text)
                                } else {
                                    chip.bg(UiColors::tint(ui.foreground, 0.063))
                                        .border_color(gpui::transparent_black())
                                        .text_color(ui.text_muted)
                                }
                            })
                            .child(shown(&reaction.emoji))
                            .child(reaction.count.to_string());
                        if !toggles {
                            return chip;
                        }
                        let (id, emoji, chosen) =
                            (message.id.clone(), reaction.emoji.clone(), reaction.chosen);
                        chip.cursor_pointer()
                            .hover(move |style| {
                                if chosen {
                                    style.bg(UiColors::tint(ui.accent, 0.35))
                                } else {
                                    style.bg(UiColors::tint(ui.foreground, 0.12))
                                }
                            })
                            // Not a selection of the bubble's text.
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.toggle_reaction(&id, &emoji, cx)
                            }))
                    }),
            )
            .into_any_element()
    }

    /// The open row (or grid) of reactions, over its ☺.
    pub(super) fn render_reactions_row(
        &self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let open = self.reacting.as_ref()?;
        let message = self.messages.get(&open.message)?;
        let all = &self.spec.reactions;
        if all.is_empty() {
            return None;
        }
        let ui = Theme::ui(cx);
        let count = if open.expanded {
            all.len()
        } else {
            all.len().min(ROW)
        };
        let cells = all[..count].iter().enumerate().map(|(index, emoji)| {
            let chosen = message
                .reactions
                .iter()
                .any(|reaction| reaction.chosen && reaction.emoji == *emoji);
            let (id, emoji) = (message.id.clone(), emoji.clone());
            div()
                .id(("fg-react-cell", index))
                .flex_none()
                .size(px(CELL))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(ui::RADIUS_SM))
                .text_size(px(EMOJI))
                .cursor_pointer()
                .when(chosen, |cell| cell.bg(UiColors::tint(ui.accent, 0.25)))
                .hover(|style| style.bg(ui.hover))
                .child(shown(&emoji))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.toggle_reaction(&id, &emoji, cx)
                }))
        });
        let more = (!open.expanded && all.len() > ROW).then(|| {
            div()
                .id("fg-react-more")
                .flex_none()
                .size(px(CELL))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(ui::RADIUS_SM))
                .cursor_pointer()
                .hover(|style| style.bg(ui.hover))
                .child(icon(IconName::ChevronDown, ui.text_muted))
                .tooltip(ui::tooltip(tr("All Reactions"), None))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    if let Some(open) = this.reacting.as_mut() {
                        open.expanded = true;
                    }
                    cx.notify();
                }))
        });
        let content = if open.expanded {
            div()
                .id("fg-react-grid")
                .w(px(CELL * COLUMNS as f32))
                .max_h(px(CELL * GRID_ROWS as f32))
                .overflow_y_scroll()
                .track_scroll(&open.scroll)
                .flex()
                .flex_wrap()
                .children(cells)
        } else {
            div()
                .id("fg-react-row")
                .flex()
                .children(cells)
                .children(more)
        };
        let panel = popup::panel(ui)
            .p(px(PADDING))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.reacting = None;
                cx.notify();
            }))
            .child(content);
        // Over the ☺, from a little left of it: the row grows toward the message's side.
        let at = point(open.at.x - px(CELL / 2.), open.at.y - px(14.));
        Some(
            deferred(
                anchored()
                    .anchor(Corner::BottomLeft)
                    .position(at)
                    .snap_to_window_with_margin(px(popup::WINDOW_MARGIN))
                    .child(panel),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}

/// An emoji as it is drawn: a lone symbol whose text form is the default ("❤", as Telegram names
/// the reaction) with the emoji form's selector — a red heart, not a black one. The events keep
/// the plugin's own string.
fn shown(emoji: &str) -> String {
    let mut chars = emoji.chars();
    match (chars.next(), chars.next()) {
        (Some(symbol), None) if (symbol as u32) < 0x1F000 => format!("{symbol}\u{FE0F}"),
        _ => emoji.to_string(),
    }
}

/// A message the user may react to: not an advertisement, not a line of the chat itself.
fn reacts(message: &Message) -> bool {
    message.sponsored.is_none()
        && !matches!(
            message.parts.first(),
            Some(flux_plugin::api::ui::MessagePart::Notice(_))
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lone_symbol_is_drawn_as_an_emoji() {
        assert_eq!(shown("❤"), "❤\u{FE0F}");
        assert_eq!(shown("🔥"), "🔥");
        assert_eq!(shown("❤\u{FE0F}"), "❤\u{FE0F}");
        assert_eq!(shown("👍🏽"), "👍🏽");
    }
}
