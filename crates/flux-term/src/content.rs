//! What the UI draws: a snapshot of the visible rows with colors already resolved, the cursor, the
//! selection, and the terminal modes that change how input is encoded.

use alacritty_terminal::term::TermMode as AlacrittyMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb as AnsiRgb};

/// An sRGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// The same color at `factor` brightness (the dim attribute: SGR 2).
    pub fn scaled(self, factor: f32) -> Self {
        let scale = |c: u8| (f32::from(c) * factor).round().clamp(0., 255.) as u8;
        Self::new(scale(self.r), scale(self.g), scale(self.b))
    }
}

impl From<AnsiRgb> for Rgb {
    fn from(rgb: AnsiRgb) -> Self {
        Self::new(rgb.r, rgb.g, rgb.b)
    }
}

impl From<Rgb> for AnsiRgb {
    fn from(rgb: Rgb) -> Self {
        AnsiRgb {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
        }
    }
}

/// The colors cells are resolved with. The UI fills it from its theme; a program can override
/// single entries (OSC 4, 10, 11), and those overrides win.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    /// Default text color.
    pub foreground: Rgb,
    /// Default background: cells on it are reported without a background (the UI leaves them
    /// transparent); it is still used for inverse video and answered to programs that ask (OSC 11).
    pub background: Rgb,
    pub cursor: Rgb,
    /// Black, red, green, yellow, blue, magenta, cyan, white, then the same eight bright.
    pub ansi: [Rgb; 16],
}

impl Default for Palette {
    /// xterm's colors on a black background.
    fn default() -> Self {
        let ansi = [
            Rgb::new(0x00, 0x00, 0x00),
            Rgb::new(0xcd, 0x00, 0x00),
            Rgb::new(0x00, 0xcd, 0x00),
            Rgb::new(0xcd, 0xcd, 0x00),
            Rgb::new(0x00, 0x00, 0xee),
            Rgb::new(0xcd, 0x00, 0xcd),
            Rgb::new(0x00, 0xcd, 0xcd),
            Rgb::new(0xe5, 0xe5, 0xe5),
            Rgb::new(0x7f, 0x7f, 0x7f),
            Rgb::new(0xff, 0x00, 0x00),
            Rgb::new(0x00, 0xff, 0x00),
            Rgb::new(0xff, 0xff, 0x00),
            Rgb::new(0x5c, 0x5c, 0xff),
            Rgb::new(0xff, 0x00, 0xff),
            Rgb::new(0x00, 0xff, 0xff),
            Rgb::new(0xff, 0xff, 0xff),
        ];
        Self {
            foreground: Rgb::new(0xe5, 0xe5, 0xe5),
            background: Rgb::new(0x00, 0x00, 0x00),
            cursor: Rgb::new(0xe5, 0xe5, 0xe5),
            ansi,
        }
    }
}

/// Brightness of dim text (SGR 2) relative to its color.
const DIM_FACTOR: f32 = 0.66;

impl Palette {
    /// The color with an index of the 256-color table: the 16 ANSI colors, the 6×6×6 cube, the
    /// 24-step gray ramp.
    pub fn indexed(&self, index: u8) -> Rgb {
        match index {
            0..16 => self.ansi[index as usize],
            16..232 => {
                let i = index - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Rgb::new(level(i / 36), level(i / 6 % 6), level(i % 6))
            }
            _ => {
                let gray = 8 + (index - 232) * 10;
                Rgb::new(gray, gray, gray)
            }
        }
    }

    /// The color a program asks about (OSC 4, 10, 11, 12 queries): an index of the 256-color table,
    /// then 256 + the named color (foreground, background, cursor).
    pub(crate) fn requested(&self, index: usize) -> Rgb {
        match index {
            0..256 => self.indexed(index as u8),
            256 => self.foreground,
            257 => self.background,
            _ => self.cursor,
        }
    }

    /// A cell color: an override set by the program wins over the palette.
    pub(crate) fn resolve(&self, color: Color, overrides: &Colors) -> Rgb {
        match color {
            Color::Spec(rgb) => rgb.into(),
            Color::Indexed(index) => overrides[index as usize]
                .map(Rgb::from)
                .unwrap_or_else(|| self.indexed(index)),
            Color::Named(named) => self.named(named, overrides),
        }
    }

    fn named(&self, named: NamedColor, overrides: &Colors) -> Rgb {
        if let Some(rgb) = overrides[named] {
            return rgb.into();
        }
        let index = named as usize;
        match named {
            NamedColor::Foreground | NamedColor::BrightForeground => self.foreground,
            NamedColor::Background => self.background,
            NamedColor::Cursor => self.cursor,
            NamedColor::DimForeground => self.foreground.scaled(DIM_FACTOR),
            _ if index < 16 => self.ansi[index],
            // DimBlack..DimWhite follow Cursor in the enum.
            _ => {
                let base = index - NamedColor::DimBlack as usize;
                self.ansi[base.min(7)].scaled(DIM_FACTOR)
            }
        }
    }
}

/// A point in the grid. `line` 0 is the top of the screen (the live area, not scrolled back);
/// scrollback lines are negative. Columns count cells, so a wide character takes two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct GridPoint {
    pub line: i32,
    pub column: usize,
}

impl GridPoint {
    pub const fn new(line: i32, column: usize) -> Self {
        Self { line, column }
    }
}

/// Underline style of a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Underline {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

/// How a cell's text is drawn; colors are already resolved into [`RenderCell`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellFlags {
    pub bold: bool,
    pub italic: bool,
    pub underline: Underline,
    pub strikeout: bool,
    /// A wide character (CJK, most emoji): it takes this column and the next one.
    pub wide: bool,
}

impl CellFlags {
    pub(crate) fn from_cell(flags: Flags) -> Self {
        let underline = if flags.contains(Flags::UNDERCURL) {
            Underline::Curly
        } else if flags.contains(Flags::DOUBLE_UNDERLINE) {
            Underline::Double
        } else if flags.contains(Flags::DOTTED_UNDERLINE) {
            Underline::Dotted
        } else if flags.contains(Flags::DASHED_UNDERLINE) {
            Underline::Dashed
        } else if flags.contains(Flags::UNDERLINE) {
            Underline::Single
        } else {
            Underline::None
        };
        Self {
            bold: flags.contains(Flags::BOLD),
            italic: flags.contains(Flags::ITALIC),
            underline,
            strikeout: flags.contains(Flags::STRIKEOUT),
            wide: flags.contains(Flags::WIDE_CHAR),
        }
    }

    /// Nothing is drawn for a blank cell with these flags (no line through or under it).
    pub fn is_plain(&self) -> bool {
        self.underline == Underline::None && !self.strikeout
    }
}

/// A cell to draw. Blank cells on the default background are left out of [`Content::cells`].
#[derive(Debug, Clone, PartialEq)]
pub struct RenderCell {
    /// Row of the viewport (0 is the top visible row) and column.
    pub row: usize,
    pub column: usize,
    pub c: char,
    /// Combining characters drawn over `c`.
    pub zerowidth: Option<Vec<char>>,
    pub fg: Rgb,
    /// `None` is the default background: the UI leaves the cell transparent.
    pub bg: Option<Rgb>,
    pub flags: CellFlags,
}

/// Cursor shape requested by the program (DECSCUSR) or the default block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Beam,
}

/// The cursor in viewport coordinates; absent when it is hidden or scrolled out of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub row: usize,
    pub column: usize,
    pub shape: CursorShape,
    /// The cursor is on a wide character: it covers two columns.
    pub wide: bool,
}

/// Which mouse events the program asked to receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseMode {
    /// The terminal handles the mouse itself (selection, scrolling).
    #[default]
    None,
    /// Presses and releases (DECSET 1000).
    Click,
    /// Also motion while a button is pressed (1002).
    Drag,
    /// All motion (1003).
    Motion,
}

/// Terminal modes that change how keys, the mouse, paste, and focus are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TermMode {
    /// Cursor keys send SS3 sequences (DECCKM): `ESC O A` instead of `ESC [ A`.
    pub app_cursor: bool,
    /// Keypad application mode (DECKPAM).
    pub app_keypad: bool,
    /// A full-screen program (vim, less, htop) is on the alternate screen.
    pub alt_screen: bool,
    pub bracketed_paste: bool,
    /// The program wants `ESC [ I` / `ESC [ O` on focus changes.
    pub focus_reporting: bool,
    pub mouse: MouseMode,
    /// SGR mouse encoding (1006).
    pub mouse_sgr: bool,
    /// UTF-8 mouse encoding (1005).
    pub mouse_utf8: bool,
    /// On the alternate screen, the wheel sends arrow keys (1007).
    pub alternate_scroll: bool,
    /// Enter sends CR LF (LNM).
    pub line_feed_new_line: bool,
}

impl From<AlacrittyMode> for TermMode {
    fn from(mode: AlacrittyMode) -> Self {
        let mouse = if mode.contains(AlacrittyMode::MOUSE_MOTION) {
            MouseMode::Motion
        } else if mode.contains(AlacrittyMode::MOUSE_DRAG) {
            MouseMode::Drag
        } else if mode.contains(AlacrittyMode::MOUSE_REPORT_CLICK) {
            MouseMode::Click
        } else {
            MouseMode::None
        };
        Self {
            app_cursor: mode.contains(AlacrittyMode::APP_CURSOR),
            app_keypad: mode.contains(AlacrittyMode::APP_KEYPAD),
            alt_screen: mode.contains(AlacrittyMode::ALT_SCREEN),
            bracketed_paste: mode.contains(AlacrittyMode::BRACKETED_PASTE),
            focus_reporting: mode.contains(AlacrittyMode::FOCUS_IN_OUT),
            mouse,
            mouse_sgr: mode.contains(AlacrittyMode::SGR_MOUSE),
            mouse_utf8: mode.contains(AlacrittyMode::UTF8_MOUSE),
            alternate_scroll: mode.contains(AlacrittyMode::ALTERNATE_SCROLL),
            line_feed_new_line: mode.contains(AlacrittyMode::LINE_FEED_NEW_LINE),
        }
    }
}

/// The selection in grid coordinates, `start` ≤ `end`, both inclusive. A block selection is a
/// rectangle: the same columns on every line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionBounds {
    pub start: GridPoint,
    pub end: GridPoint,
    pub block: bool,
}

impl SelectionBounds {
    /// Whether the cell is selected.
    pub fn contains(&self, point: GridPoint) -> bool {
        if self.block {
            let left = self.start.column.min(self.end.column);
            let right = self.start.column.max(self.end.column);
            (self.start.line..=self.end.line).contains(&point.line)
                && (left..=right).contains(&point.column)
        } else {
            self.start <= point && point <= self.end
        }
    }
}

/// A snapshot of what is on screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Content {
    pub columns: usize,
    pub rows: usize,
    /// How many lines the view is scrolled back into the history; 0 is the live screen.
    pub display_offset: usize,
    /// Lines of scrollback above the screen.
    pub history_size: usize,
    /// Cells to draw, row by row, left to right.
    pub cells: Vec<RenderCell>,
    pub cursor: Option<Cursor>,
    pub selection: Option<SelectionBounds>,
    pub mode: TermMode,
}

impl Content {
    /// The grid line shown on a viewport row.
    pub fn grid_line(&self, row: usize) -> i32 {
        row as i32 - self.display_offset as i32
    }

    /// The viewport row a grid line is shown on, if it is visible.
    pub fn viewport_row(&self, line: i32) -> Option<usize> {
        let row = line + self.display_offset as i32;
        (0..self.rows as i32).contains(&row).then_some(row as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_cube_and_gray_ramp() {
        let palette = Palette::default();
        assert_eq!(palette.indexed(1), palette.ansi[1]);
        assert_eq!(palette.indexed(16), Rgb::new(0, 0, 0));
        assert_eq!(palette.indexed(21), Rgb::new(0, 0, 255));
        assert_eq!(palette.indexed(196), Rgb::new(255, 0, 0));
        assert_eq!(palette.indexed(232), Rgb::new(8, 8, 8));
        assert_eq!(palette.indexed(255), Rgb::new(238, 238, 238));
    }

    #[test]
    fn viewport_rows_follow_the_scroll() {
        let content = Content {
            columns: 80,
            rows: 24,
            display_offset: 10,
            history_size: 100,
            cells: Vec::new(),
            cursor: None,
            selection: None,
            mode: TermMode::default(),
        };
        assert_eq!(content.grid_line(0), -10);
        assert_eq!(content.viewport_row(-10), Some(0));
        assert_eq!(content.viewport_row(13), Some(23));
        assert_eq!(content.viewport_row(14), None);
        assert_eq!(content.viewport_row(-11), None);
    }

    #[test]
    fn selection_contains_cells_between_its_ends() {
        let selection = SelectionBounds {
            start: GridPoint::new(0, 5),
            end: GridPoint::new(2, 3),
            block: false,
        };
        assert!(!selection.contains(GridPoint::new(0, 4)));
        assert!(selection.contains(GridPoint::new(0, 5)));
        assert!(selection.contains(GridPoint::new(1, 0)));
        assert!(selection.contains(GridPoint::new(2, 3)));
        assert!(!selection.contains(GridPoint::new(2, 4)));
        let block = SelectionBounds {
            block: true,
            ..selection
        };
        assert!(block.contains(GridPoint::new(1, 4)));
        assert!(!block.contains(GridPoint::new(1, 0)));
    }
}
