//! A plugin's QR code (API 0.5, `ui.element-kind.qr`): a sign-in link for a phone's camera. Dark
//! modules on a light square whatever the theme, with the quiet zone the readers need (four
//! modules), each module a whole number of device pixels — crisp, not a scaled picture.

use gpui::{
    App, Bounds, IntoElement, ParentElement, Pixels, Styled, Window, canvas, div, fill, point, px,
    rgb, size,
};

/// The side of the code, quiet zone included.
pub const SIZE: f32 = 200.;
/// The light margin around the modules, in modules.
const QUIET_ZONE: i32 = 4;

/// The modules of `text`'s code, row by row (`true` — dark), or `None` when it is too long for a
/// code.
pub fn modules(text: &str) -> Option<Vec<Vec<bool>>> {
    let code = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium).ok()?;
    let side = code.size();
    Some(
        (0..side)
            .map(|y| (0..side).map(|x| code.get_module(x, y)).collect())
            .collect(),
    )
}

/// The code of `text`, [`SIZE`] square; a text too long for a code draws the light square alone.
pub fn render(text: &str) -> impl IntoElement + use<> {
    let modules = modules(text).unwrap_or_default();
    div()
        .flex_none()
        .size(px(SIZE))
        .rounded(px(crate::ui::RADIUS_MD))
        .bg(rgb(0xffffff))
        .child(
            canvas(
                |_, _, _| {},
                move |bounds: Bounds<Pixels>, _, window: &mut Window, _: &mut App| {
                    paint(&modules, bounds, window)
                },
            )
            .size_full(),
        )
}

/// Runs of dark modules as rectangles, snapped to device pixels.
fn paint(modules: &[Vec<bool>], bounds: Bounds<Pixels>, window: &mut Window) {
    let side = modules.len() as i32;
    if side == 0 {
        return;
    }
    let scale = window.scale_factor();
    let total = side + 2 * QUIET_ZONE;
    // A whole number of device pixels per module; the code centered in the square.
    let module_device = ((f32::from(bounds.size.width) * scale) / total as f32).floor();
    if module_device < 1. {
        return;
    }
    let module = module_device / scale;
    let origin_device = |start: Pixels, length: Pixels| {
        ((f32::from(start) * scale)
            + ((f32::from(length) * scale) - module_device * side as f32) / 2.)
            .round()
            / scale
    };
    let left = origin_device(bounds.origin.x, bounds.size.width);
    let top = origin_device(bounds.origin.y, bounds.size.height);
    let dark = rgb(0x111111);
    for (y, row) in modules.iter().enumerate() {
        let mut x = 0;
        while x < row.len() {
            if !row[x] {
                x += 1;
                continue;
            }
            let start = x;
            while x < row.len() && row[x] {
                x += 1;
            }
            window.paint_quad(fill(
                Bounds::new(
                    point(
                        px(left + start as f32 * module),
                        px(top + y as f32 * module),
                    ),
                    size(px((x - start) as f32 * module), px(module)),
                ),
                dark,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_makes_a_square_code_with_finder_patterns() {
        let modules = modules("https://example.com/login?token=abc").unwrap();
        let side = modules.len();
        assert!(side >= 21);
        assert!(modules.iter().all(|row| row.len() == side));
        // The top-left finder pattern: a dark 7×7 frame.
        assert!((0..7).all(|i| modules[0][i] && modules[i][0] && modules[6][i]));
        assert!(!modules[1][1]);
    }
}
