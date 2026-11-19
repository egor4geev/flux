//! The bar of a chat's pinned message (API 0.5, FluxGram part 5): `pinned` changes put it over
//! the feed — under the window's header, above the bar of what plays when both show. On its left
//! a line cut into as many pieces as there are pinned messages (at most [`PIECES`] in sight: the
//! rest scroll with the lit one; the newest is the lowest, as in the messengers), the message's
//! picture when it has one, the title in the accent over the message's line, the plugin's icon
//! buttons at the right end. A click on the bar comes as `pinned-clicked(message)` — the plugin
//! reveals the message and puts the next one; a click on a button as `pinned-action`.
//!
//! A new pinned message just takes the old one's place: the bar asks for no frames by itself.

use flux_plugin::api::events::UiEvent;
use flux_plugin::api::ui::{ImageShape, PinnedSpec};
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, SharedString, Window, div, prelude::*, px,
};

use super::{Conversation, render_picture};
use crate::icons::plugin_icon;
use crate::theme::{self, Theme};
use crate::ui;

/// The bar's height: the title over the line.
const HEIGHT: f32 = 44.;
/// The picture before the text, square.
const PICTURE: f32 = 32.;
/// At most this many pieces of the line are in sight.
pub(super) const PIECES: u32 = 4;
/// The line's height and the gap between its pieces.
const LINE: f32 = 32.;
const GAP: f32 = 2.;

impl Conversation {
    /// The bar of the pinned message, when the plugin put one.
    pub(super) fn render_pinned_bar(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let pinned = self.pinned.as_ref()?;
        let ui = Theme::ui(cx);
        let picture = pinned.image.as_ref().map(|image| {
            // The bar's own corners: a small square, whatever the message's shape.
            let spec = flux_plugin::api::ui::ImageSpec {
                shape: ImageShape::Square,
                label: None,
                play: false,
                progress: None,
                animation: None,
                ..image.clone()
            };
            div()
                .flex_none()
                .size(px(PICTURE))
                .rounded(px(ui::RADIUS_XS))
                .overflow_hidden()
                .child(render_picture(
                    &spec,
                    (PICTURE, PICTURE),
                    &self.plugin,
                    cx.entity_id(),
                    window,
                    cx,
                ))
        });
        let actions = pinned.actions.iter().map(|action| {
            let name = action.icon.clone().unwrap_or_default();
            let glyph = plugin_icon(&self.plugin, &name, &ui);
            let tooltip = action
                .tooltip
                .clone()
                .or_else(|| action.label.clone())
                .unwrap_or_default();
            let (message, id) = (pinned.message.clone(), action.id.clone());
            div()
                .id(SharedString::from(format!("fg-pinned-{}", action.id)))
                .flex_none()
                .size(px(ui::ICON_BUTTON_SIZE))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(ui::RADIUS_SM))
                .cursor_pointer()
                .hover(|style| style.bg(ui.hover))
                .active(|style| style.bg(ui.pressed))
                .children(glyph.map(|glyph| glyph.render(ui.text_muted)))
                .tooltip(ui::tooltip(tooltip, None))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.emit(UiEvent::PinnedAction((message.clone(), id.clone())), cx)
                }))
        });
        let message = pinned.message.clone();
        Some(
            div()
                .id("fg-pinned")
                .flex_none()
                .h(px(HEIGHT))
                .pl(px(12.))
                .pr(px(8.))
                .flex()
                .items_center()
                .gap(px(10.))
                .border_b_1()
                .border_color(ui.divider)
                .cursor_pointer()
                .hover(|style| style.bg(ui.hover))
                .active(|style| style.bg(ui.pressed))
                .child(line(pinned, ui.accent))
                .children(picture)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .text_size(px(theme::TEXT_SM))
                        .child(
                            div()
                                .truncate()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(ui.accent_text)
                                .child(pinned.title.clone()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_color(ui.foreground)
                                .child(pinned.text.replace('\n', " ")),
                        ),
                )
                .children(actions)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.emit(UiEvent::PinnedClicked(message.clone()), cx)
                }))
                .into_any_element(),
        )
    }
}

/// The line on the bar's left: a piece per pinned message, the shown one lit.
fn line(pinned: &PinnedSpec, accent: gpui::Hsla) -> AnyElement {
    let count = pinned.count.max(1);
    let lit = lit_piece(pinned.position, count);
    let (first, shown) = pieces_in_sight(lit, count);
    let piece = (LINE - GAP * (shown.min(PIECES) as f32 - 1.)) / shown.min(PIECES) as f32;
    div()
        .flex_none()
        .w(px(2.))
        .h(px(LINE))
        .flex()
        .flex_col()
        .gap(px(GAP))
        .overflow_hidden()
        .children((first..first + shown).map(|ix| {
            div()
                .flex_none()
                .w_full()
                .h(px(piece))
                .rounded(px(1.))
                .bg(match ix == lit {
                    true => accent,
                    false => accent.opacity(0.35),
                })
        }))
        .into_any_element()
}

/// The lit piece, from the top: the newest pinned message (position 0) is the lowest.
pub(super) fn lit_piece(position: u32, count: u32) -> u32 {
    count
        .saturating_sub(1)
        .saturating_sub(position.min(count.saturating_sub(1)))
}

/// The pieces in sight, from the top: the first one and how many — all of them up to
/// [`PIECES`]; more scroll so the lit one stays in sight, third from the top where it can.
pub(super) fn pieces_in_sight(lit: u32, count: u32) -> (u32, u32) {
    if count <= PIECES {
        return (0, count);
    }
    let first = lit.saturating_sub(2).min(count - PIECES);
    (first, PIECES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_pinned_is_the_lowest_piece_and_the_line_scrolls_with_it() {
        assert_eq!(lit_piece(0, 3), 2);
        assert_eq!(lit_piece(2, 3), 0);
        assert_eq!(lit_piece(0, 1), 0);
        assert_eq!(pieces_in_sight(1, 3), (0, 3));
        // Seven pinned: the newest shows the lowest four; going older, the lit one stays third.
        assert_eq!(pieces_in_sight(lit_piece(0, 7), 7), (3, 4));
        assert_eq!(pieces_in_sight(lit_piece(3, 7), 7), (1, 4));
        assert_eq!(pieces_in_sight(lit_piece(6, 7), 7), (0, 4));
    }
}
