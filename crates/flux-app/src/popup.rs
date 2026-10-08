//! Popups at a text position in the editor (the completion menu, hover): where the position is on
//! screen, which side of its line a popup goes to, and the popup surface itself.

use flux_core::text::line_start;
use gpui::{Div, Pixels, div, prelude::*, px};

use crate::editor::Editor;
use crate::theme::{self, UiColors};
use crate::ui::{self, RADIUS_LG};

/// Distance between a popup and its line, and between neighboring popups.
pub const POPUP_GAP: f32 = 4.;
/// A popup keeps this far from the window edges.
pub const WINDOW_MARGIN: f32 = 8.;

/// A text position on screen, in window coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    /// Left edge of the character at the position.
    pub x: Pixels,
    pub line_top: Pixels,
    pub line_bottom: Pixels,
}

/// Where `pos` is on screen, with the current scroll; `None` if it is not in the visible text.
///
/// The line shapes come from the previous frame, the scroll is the current one: the frame being
/// rendered draws the text with it (unless an autoscroll is pending — then the caller asks for one
/// more frame).
pub fn anchor(editor: &Editor, pos: usize) -> Option<Anchor> {
    let layout = editor.layout.as_ref()?;
    let text = editor.document.text();
    let pos = pos.min(text.len_chars());
    let line = text.char_to_line(pos);
    let x_in_line = layout
        .line(line)?
        .x_for_column(pos - line_start(text, line));
    let bounds = layout.text_bounds;
    let x = bounds.left() + px(theme::TEXT_PADDING - editor.scroll.x) + x_in_line;
    let line_top = bounds.top() + layout.line_height * line as f32 - px(editor.scroll.y);
    let line_bottom = line_top + layout.line_height;
    let visible = line_bottom > bounds.top()
        && line_top < bounds.bottom()
        && x >= bounds.left()
        && x <= bounds.right();
    visible.then_some(Anchor {
        x,
        line_top,
        line_bottom,
    })
}

/// Which side of its line a popup goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Below,
    Above,
}

/// The space for a popup above and below the line, within a window of height `window_height`.
pub fn space(anchor: &Anchor, window_height: Pixels) -> (Pixels, Pixels) {
    let margin = px(POPUP_GAP + WINDOW_MARGIN);
    let above = (anchor.line_top - margin).max(px(0.));
    let below = (window_height - anchor.line_bottom - margin).max(px(0.));
    (above, below)
}

/// The side for a popup of height `height`: `preferred` if it fits there, otherwise the other one if
/// it fits, otherwise the roomier one. Also returns the space on that side (the popup's height
/// limit).
pub fn side(
    anchor: &Anchor,
    height: Pixels,
    window_height: Pixels,
    preferred: Side,
) -> (Side, Pixels) {
    let (above, below) = space(anchor, window_height);
    let room = |side| match side {
        Side::Above => above,
        Side::Below => below,
    };
    let other = match preferred {
        Side::Above => Side::Below,
        Side::Below => Side::Above,
    };
    let side = if room(preferred) >= height {
        preferred
    } else if room(other) >= height || room(other) > room(preferred) {
        other
    } else {
        preferred
    };
    (side, room(side))
}

/// The popup surface: an elevated panel of the design system, as the context menu (a popover's
/// colors with the smaller radius of a compact list).
pub fn panel(ui: UiColors) -> Div {
    div()
        .relative()
        .rounded(px(RADIUS_LG))
        .bg(ui.elevated)
        .border_1()
        .border_color(ui.elevated_border)
        .shadow(ui::popover_shadow(ui))
        .font_family(theme::UI_FONT)
        .text_size(px(theme::TEXT_MD))
        .text_color(ui.foreground)
        .child(ui::sheen(ui, RADIUS_LG))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor_at(top: f32) -> Anchor {
        Anchor {
            x: px(100.),
            line_top: px(top),
            line_bottom: px(top + 21.),
        }
    }

    #[test]
    fn popups_go_where_they_fit() {
        let window = px(800.);
        // Near the top: below, as preferred.
        assert_eq!(
            side(&anchor_at(100.), px(200.), window, Side::Below).0,
            Side::Below
        );
        // Near the bottom: the menu flips above the line.
        let (side_, room) = side(&anchor_at(700.), px(200.), window, Side::Below);
        assert_eq!(side_, Side::Above);
        assert_eq!(room, px(700. - 12.));
        // A hover prefers above, but the first lines have no room there.
        assert_eq!(
            side(&anchor_at(40.), px(150.), window, Side::Above).0,
            Side::Below
        );
        // Fits nowhere: the roomier side, limited by its space.
        let (side_, room) = side(&anchor_at(300.), px(1000.), window, Side::Above);
        assert_eq!(side_, Side::Below);
        assert_eq!(room, px(800. - 321. - 12.));
    }
}
