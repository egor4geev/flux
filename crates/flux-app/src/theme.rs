//! Цвета и метрики. Пока константы; конфиг тем — позже.

use gpui::{Hsla, rgb, rgba};

pub const FONT_FAMILY: &str = "Menlo";
pub const FONT_SIZE: f32 = 14.;
pub const LINE_HEIGHT: f32 = 21.;
pub const TAB_WIDTH: usize = 4;
/// Отступ текста от гаттера.
pub const TEXT_PADDING: f32 = 8.;
/// Сколько строк держать между курсором и краем окна при автоскролле.
pub const SCROLL_MARGIN_LINES: usize = 3;

pub fn background() -> Hsla {
    rgb(0x0d1117).into()
}

pub fn foreground() -> Hsla {
    rgb(0xc9d1d9).into()
}

pub fn dim() -> Hsla {
    rgb(0x6e7681).into()
}

pub fn current_line() -> Hsla {
    rgb(0x161b22).into()
}

pub fn selection() -> Hsla {
    rgba(0x388bfd55).into()
}

pub fn cursor() -> Hsla {
    rgb(0x58a6ff).into()
}

pub fn status_bar() -> Hsla {
    rgb(0x010409).into()
}

pub fn border() -> Hsla {
    rgb(0x21262d).into()
}
