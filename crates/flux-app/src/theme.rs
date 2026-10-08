//! The theme is data: UI colors and highlight scope styles. It is set globally (`cx.set_global`)
//! and read via [`Theme::get`] and [`Theme::ui`]. Font metrics, tab width, and blinking are
//! constants: they are future settings, not part of the theme.
//!
//! UI colors are the design system's tokens (wiki: "Design System"): glass surfaces (window frame,
//! islands, popovers), text at three levels, accent, states, and a palette of shades that carry
//! meaning (file types, categories, counters). Components are in `ui.rs`.

use std::sync::OnceLock;
use std::time::Duration;

use flux_syntax::Highlight;
use gpui::{App, Global, Hsla, rgb, rgba};

/// The UI font is the system font (SF Pro on macOS).
pub const UI_FONT: &str = ".SystemUIFont";
/// The code font is the first installed one from the list ([`init_fonts`]).
const CODE_FONTS: [&str; 4] = [
    "JetBrains Mono",
    "JetBrainsMono Nerd Font Mono",
    "SF Mono",
    "Menlo",
];
pub const FONT_SIZE: f32 = 14.;
pub const LINE_HEIGHT: f32 = 21.;
pub const TAB_WIDTH: usize = 4;
/// Padding between the gutter and the text.
pub const TEXT_PADDING: f32 = 8.;
/// How many lines to keep between the cursor and the window edge when auto-scrolling.
pub const SCROLL_MARGIN_LINES: usize = 3;
/// Cursor blink period; `None` means the cursor does not blink.
pub const CURSOR_BLINK: Option<Duration> = Some(Duration::from_millis(500));

/// Terminal text: the code font, a little smaller and denser than the editor's.
pub const TERMINAL_FONT_SIZE: f32 = 13.;
pub const TERMINAL_LINE_HEIGHT: f32 = 18.;

/// UI font sizes (for the [`UI_FONT`] font).
pub const TEXT_XS: f32 = 11.;
pub const TEXT_SM: f32 = 12.;
pub const TEXT_MD: f32 = 13.;
pub const TEXT_LG: f32 = 15.;
pub const TEXT_XL: f32 = 20.;

static CODE_FONT: OnceLock<&'static str> = OnceLock::new();

/// Picks the code font: the first installed one from [`CODE_FONTS`]. Without this check, gpui would
/// silently substitute a proportional system font.
pub fn init_fonts(cx: &App) {
    let installed = cx.text_system().all_font_names();
    let family = CODE_FONTS
        .into_iter()
        .find(|family| installed.iter().any(|name| name == family))
        .unwrap_or("Menlo");
    CODE_FONT.set(family).ok();
}

/// The code font: editor, search fields, result rows.
pub fn code_font() -> &'static str {
    CODE_FONT.get().copied().unwrap_or("Menlo")
}

/// UI colors. `Copy`: read from the global theme as a single value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UiColors {
    // --- Surfaces (glass). The alpha lets the blurred desktop show through. ---
    /// Window frame: the background under the islands, the title bar, and the status bar.
    pub frame: Hsla,
    /// Colored glow of the frame (a gradient from the top-left corner).
    pub frame_glow: Hsla,
    /// Island: a self-contained panel (tree, editor).
    pub island: Hsla,
    pub island_border: Hsla,
    /// Glass sheen along the top edge of an island and of a popover.
    pub sheen: Hsla,
    /// Popovers: menus, pickers, project search, tooltips.
    pub elevated: Hsla,
    pub elevated_border: Hsla,
    pub shadow: Hsla,
    /// Thin dividers inside the islands.
    pub divider: Hsla,

    // --- Text ---
    pub foreground: Hsla,
    /// Secondary text: paths, labels, inactive tabs.
    pub text_muted: Hsla,
    /// Tertiary: line numbers, hints, placeholders.
    pub dim: Hsla,
    pub text_disabled: Hsla,

    // --- Interaction ---
    pub accent: Hsla,
    /// Accent for text and icons on dark backgrounds (lighter than the main accent).
    pub accent_text: Hsla,
    /// Accent background: an enabled toggle, an action icon.
    pub accent_soft: Hsla,
    pub hover: Hsla,
    pub pressed: Hsla,
    /// Selected list row while the list has focus.
    pub list_selected: Hsla,
    /// Selected row without focus: the file of the active tab in the tree.
    pub list_selected_inactive: Hsla,
    pub input_background: Hsla,
    pub input_border: Hsla,
    /// Border of a focused field and the ring around it.
    pub focus_border: Hsla,
    pub focus_ring: Hsla,
    /// The directory the dragged file is about to be dropped on (file tree).
    pub drop_target: Hsla,
    /// A key in a shortcut hint.
    pub keycap: Hsla,
    pub keycap_border: Hsla,

    // --- States ---
    pub success: Hsla,
    pub warning: Hsla,
    pub error: Hsla,
    pub info: Hsla,
    /// Unsaved changes: the dot on a tab, the marker in the status bar.
    pub modified: Hsla,

    // --- Editor ---
    pub current_line: Hsla,
    pub selection: Hsla,
    pub cursor: Hsla,
    /// Matched characters in lists: fuzzy search, project search results.
    pub match_text: Hsla,
    /// Background of the matches found in the text, and of the current match.
    pub search_match: Hsla,
    pub search_match_active: Hsla,

    // --- Version control: a file's change against HEAD (names in the tree, tabs, the commit
    // window), as JetBrains IDEs color them. ---
    pub vcs_modified: Hsla,
    pub vcs_added: Hsla,
    pub vcs_deleted: Hsla,
    pub vcs_renamed: Hsla,
    /// Not tracked: "Unversioned Files".
    pub vcs_untracked: Hsla,
    pub vcs_conflict: Hsla,

    // --- Diffs: markers in the editor's gutter (solid), blocks in the diff viewer (background),
    // changed words inside a modified block (a stronger background). Added is green, modified blue,
    // deleted gray, as in JetBrains IDEs. ---
    pub diff_added: Hsla,
    pub diff_modified: Hsla,
    pub diff_deleted: Hsla,
    pub diff_added_bg: Hsla,
    pub diff_modified_bg: Hsla,
    pub diff_deleted_bg: Hsla,
    pub diff_added_word: Hsla,
    pub diff_modified_word: Hsla,
    pub diff_deleted_word: Hsla,

    // --- Shade palette: meaning, not decoration (file types, categories, counters). ---
    pub blue: Hsla,
    pub indigo: Hsla,
    pub violet: Hsla,
    pub pink: Hsla,
    pub red: Hsla,
    pub orange: Hsla,
    pub amber: Hsla,
    pub lime: Hsla,
    pub green: Hsla,
    pub teal: Hsla,
    pub cyan: Hsla,
    /// Directory icon.
    pub folder: Hsla,
}

impl UiColors {
    /// Returns `color` with opacity `alpha`, for shade backgrounds (badges, icon tiles).
    pub fn tint(color: Hsla, alpha: f32) -> Hsla {
        Hsla { a: alpha, ..color }
    }
}

/// How to draw a highlight scope.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyntaxStyle {
    pub color: Hsla,
    pub bold: bool,
    pub italic: bool,
}

/// Terminal colors: the defaults and the 16 ANSI colors programs pick from (`ls`, `git`, prompts).
/// The 256-color table and true colors are computed, not themed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerminalColors {
    pub foreground: Hsla,
    /// The background programs assume (inverse video, answers to color queries). The terminal
    /// itself is transparent: cells on the default background show the island.
    pub background: Hsla,
    pub cursor: Hsla,
    /// Black, red, green, yellow, blue, magenta, cyan, white, then the same eight bright.
    pub ansi: [Hsla; 16],
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub ui: UiColors,
    pub terminal: TerminalColors,
    /// Highlight scopes: names like tree-sitter captures (`keyword`, `function.method`). A general
    /// name covers the specific ones: `function` also applies to `function.method` if the latter
    /// has no entry of its own.
    pub syntax: Vec<(String, SyntaxStyle)>,
}

impl Global for Theme {}

impl Theme {
    pub fn get(cx: &App) -> &Theme {
        cx.global::<Theme>()
    }

    pub fn ui(cx: &App) -> UiColors {
        Self::get(cx).ui
    }

    /// Scope names in order, for `flux_syntax::HighlightMap`; the index in this list is the
    /// [`Highlight`].
    pub fn syntax_scopes(&self) -> Vec<&str> {
        self.syntax
            .iter()
            .map(|(scope, _)| scope.as_str())
            .collect()
    }

    pub fn syntax_style(&self, highlight: Highlight) -> Option<SyntaxStyle> {
        self.syntax.get(highlight.0).map(|(_, style)| *style)
    }

    /// The dark theme "Flux Night": a glass frame and islands in cool blue-violet neutrals; the
    /// accent is indigo. Code highlighting uses the GitHub Dark palette (Primer, "prettylights"):
    /// keywords and operators are red; functions are purple; types and constructors are orange;
    /// strings are light blue; constants, numbers, properties, and built-ins are blue; tags,
    /// regexes, and JSON keys are green; comments are gray. Plain variables and punctuation use the
    /// text color to avoid noise; parameters are orange (GitHub's `variable` color) to tell them
    /// apart from locals.
    pub fn flux_night() -> Self {
        const RED: u32 = 0xff7b72;
        const PURPLE: u32 = 0xd2a8ff;
        const ORANGE: u32 = 0xffa657;
        const BLUE: u32 = 0x79c0ff;
        const LIGHT_BLUE: u32 = 0xa5d6ff;
        const GREEN: u32 = 0x7ee787;
        const GRAY: u32 = 0x8b949e;
        const TEXT: u32 = 0xc9d1d9;

        let plain = |color: u32| SyntaxStyle {
            color: rgb(color).into(),
            bold: false,
            italic: false,
        };
        let bold = |color: u32| SyntaxStyle {
            bold: true,
            ..plain(color)
        };
        let syntax = [
            ("attribute", plain(BLUE)),
            ("boolean", plain(BLUE)),
            ("comment", plain(GRAY)),
            ("constant", plain(BLUE)),
            ("constructor", plain(ORANGE)),
            // Code inside `${…}` and f-strings is not a string.
            ("embedded", plain(TEXT)),
            ("escape", plain(BLUE)),
            ("function", plain(PURPLE)),
            ("keyword", plain(RED)),
            ("label", plain(ORANGE)),
            ("number", plain(BLUE)),
            ("operator", plain(RED)),
            ("property", plain(BLUE)),
            ("punctuation", plain(TEXT)),
            ("punctuation.special", plain(RED)),
            ("string", plain(LIGHT_BLUE)),
            ("string.escape", plain(BLUE)),
            ("string.special", plain(GREEN)),
            ("tag", plain(GREEN)),
            ("text.literal", plain(BLUE)),
            ("text.reference", plain(LIGHT_BLUE)),
            ("text.title", bold(BLUE)),
            ("text.uri", plain(LIGHT_BLUE)),
            ("type", plain(ORANGE)),
            ("type.builtin", plain(BLUE)),
            ("variable", plain(TEXT)),
            ("variable.builtin", plain(BLUE)),
            ("variable.parameter", plain(ORANGE)),
        ];

        // The shades are tuned for dark glass: similar lightness, different hue.
        const INDIGO: u32 = 0x8590ff;
        const AMBER: u32 = 0xffc560;
        // The terminal follows GitHub Dark too: its ANSI palette.
        let ansi = [
            0x484f58, 0xff7b72, 0x3fb950, 0xd29922, 0x58a6ff, 0xbc8cff, 0x39c5cf, 0xb1bac4,
            0x6e7681, 0xffa198, 0x56d364, 0xe3b341, 0x79c0ff, 0xd2a8ff, 0x56d4dd, 0xf0f6fc,
        ]
        .map(|color| rgb(color).into());
        let terminal = TerminalColors {
            foreground: rgb(TEXT).into(),
            background: rgb(0x0d1018).into(),
            cursor: rgb(0xaab2ff).into(),
            ansi,
        };
        Self {
            terminal,
            ui: UiColors {
                frame: rgba(0x07090fc7).into(),
                frame_glow: rgba(0x8590ff24).into(),
                island: rgba(0x0d1018e6).into(),
                island_border: rgba(0xffffff14).into(),
                sheen: rgba(0xffffff2e).into(),
                elevated: rgba(0x171b28fa).into(),
                elevated_border: rgba(0xffffff1f).into(),
                shadow: rgba(0x00000080).into(),
                divider: rgba(0xffffff12).into(),

                foreground: rgb(0xe6e9f2).into(),
                text_muted: rgb(0xa3abc3).into(),
                dim: rgb(0x6b7391).into(),
                text_disabled: rgb(0x4a516a).into(),

                accent: rgb(INDIGO).into(),
                accent_text: rgb(0xaab2ff).into(),
                accent_soft: rgba(0x8590ff2e).into(),
                hover: rgba(0xffffff0f).into(),
                pressed: rgba(0xffffff17).into(),
                list_selected: rgba(0x8590ff3d).into(),
                list_selected_inactive: rgba(0xffffff14).into(),
                input_background: rgba(0x00000052).into(),
                input_border: rgba(0xffffff17).into(),
                focus_border: rgba(0x8590ffd9).into(),
                focus_ring: rgba(0x8590ff3d).into(),
                drop_target: rgba(0x8590ff29).into(),
                keycap: rgba(0xffffff0f).into(),
                keycap_border: rgba(0xffffff1a).into(),

                success: rgb(0x4fd18b).into(),
                warning: rgb(AMBER).into(),
                error: rgb(0xff6b6b).into(),
                info: rgb(0x5aa9ff).into(),
                modified: rgb(0xffb35c).into(),

                current_line: rgba(0xffffff0a).into(),
                selection: rgba(0x8590ff4d).into(),
                cursor: rgb(0xaab2ff).into(),
                match_text: rgb(0xaab2ff).into(),
                search_match: rgba(0xffc56038).into(),
                search_match_active: rgba(0xffc5608f).into(),

                vcs_modified: rgb(0x6cb0ff).into(),
                vcs_added: rgb(0x5fd38d).into(),
                vcs_deleted: rgb(0x8a91ab).into(),
                vcs_renamed: rgb(0x3cd3c4).into(),
                vcs_untracked: rgb(0xe8806b).into(),
                vcs_conflict: rgb(0xff6b6b).into(),

                diff_added: rgb(0x3fb950).into(),
                diff_modified: rgb(0x5aa9ff).into(),
                diff_deleted: rgb(0x8b949e).into(),
                diff_added_bg: rgba(0x3fb9501f).into(),
                diff_modified_bg: rgba(0x5aa9ff1c).into(),
                diff_deleted_bg: rgba(0x8b949e1f).into(),
                diff_added_word: rgba(0x3fb95052).into(),
                diff_modified_word: rgba(0x5aa9ff4d).into(),
                diff_deleted_word: rgba(0x8b949e4d).into(),

                blue: rgb(0x5aa9ff).into(),
                indigo: rgb(INDIGO).into(),
                violet: rgb(0xb48cff).into(),
                pink: rgb(0xff7eb6).into(),
                red: rgb(0xff6b6b).into(),
                orange: rgb(0xff9c5b).into(),
                amber: rgb(AMBER).into(),
                lime: rgb(0xb8e06a).into(),
                green: rgb(0x4fd18b).into(),
                teal: rgb(0x3cd3c4).into(),
                cyan: rgb(0x5ccfff).into(),
                folder: rgb(0x7fa8ff).into(),
            },
            syntax: syntax
                .into_iter()
                .map(|(scope, style)| (scope.to_string(), style))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_syntax::{HighlightMap, languages};

    /// Every capture of every language resolves to a theme scope: its own or a more general one, by
    /// falling back along the dots. `none` (an internal one in markdown) is left uncolored.
    #[test]
    fn dark_theme_covers_every_capture() {
        let theme = Theme::flux_night();
        let scopes = theme.syntax_scopes();
        for language in languages() {
            let map = HighlightMap::new(language, &scopes);
            for (i, name) in language.capture_names().iter().enumerate() {
                let highlight = map.get(i as u32);
                if *name == "none" {
                    assert_eq!(highlight, None, "{}", language.name());
                } else {
                    assert!(highlight.is_some(), "{}: @{name}", language.name());
                }
            }
        }
    }

    #[test]
    fn scopes_are_unique_and_styles_line_up() {
        let theme = Theme::flux_night();
        let scopes = theme.syntax_scopes();
        let mut sorted = scopes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), scopes.len());
        let title = scopes.iter().position(|s| *s == "text.title").unwrap();
        assert!(theme.syntax_style(Highlight(title)).unwrap().bold);
        assert_eq!(theme.syntax_style(Highlight(scopes.len())), None);
    }
}
