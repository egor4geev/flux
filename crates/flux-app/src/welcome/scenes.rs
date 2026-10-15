//! The tour's pictures: the Flux window drawn small in the chosen theme, its look (Islands or
//! Flat) and the interface language, with the slide's subject on top — the search popup, a problem
//! and its fixes, the tool windows (here); version control and Claude Code
//! ([`super::scenes_vcs`]).
//!
//! A picture is a function of its show's `progress` (0 — before the keys are pressed, 1 — after):
//! the guide drives the show ([`super::tour::Tasks`]) and asks for frames while it runs, so a
//! picture has no state of its own and any moment of it can be drawn (and screenshot).
//!
//! The pieces of the window — its tree, an editor with tabs, a popup — are drawn by the helpers
//! here, so every picture is the same window; a picture puts its own popups over the window
//! ([`mini_window`] is positioned: `.child(popup.absolute()…)`), at the places the text of
//! [`COLOR_RS`] gives them ([`code_x`], [`code_y`]).

use gpui::{
    AnyElement, App, BoxShadow, Div, FontWeight, Hsla, IntoElement, div, point, prelude::*, px,
};

use super::{Tour, scenes_vcs};
use crate::i18n::{tr, trn};
use crate::icons::{IconName, file_icon, folder_icon, icon};
use crate::settings::UiStyle;
use crate::theme::{self, Theme, UiColors};
use crate::{code_actions, file_tree, git, project_search, terminal_panel, ui};

/// The size a picture is drawn at: the right side of a tour slide.
pub const SCENE_WIDTH: f32 = 560.;
pub const SCENE_HEIGHT: f32 = 420.;
/// The small window's text: the size of the interface and of the code in it.
pub const SCENE_TEXT: f32 = 11.;
pub const SCENE_CODE: f32 = 11.5;
pub const SCENE_LINE_HEIGHT: f32 = 19.;

/// The small window's title bar and the editor's tab bar.
pub const TITLE_HEIGHT: f32 = 30.;
pub const TABS_HEIGHT: f32 = 30.;
/// The launchpad's column, and where the content starts after it (with the gap of Islands).
pub const LAUNCHPAD_WIDTH: f32 = 30.;
/// The width of the line numbers' column in [`code_lines`].
pub const GUTTER_WIDTH: f32 = 34.;
/// The code font's advance at [`SCENE_CODE`]: the monospaced fonts Flux uses are 0.6 em wide.
pub const CODE_ADVANCE: f32 = SCENE_CODE * 0.6;
/// The padding above the first line of [`code_lines`].
const CODE_TOP: f32 = 6.;

/// A tour slide's picture at `progress` of its show.
pub fn scene(tour: Tour, progress: f32, cx: &App) -> AnyElement {
    match tour {
        Tour::Search => search(progress, cx),
        Tour::Code => problem(progress, cx),
        Tour::Git => scenes_vcs::git(progress, cx),
        Tour::Claude => scenes_vcs::claude(progress, cx),
        Tour::Windows => windows(progress, cx),
    }
}

// --- The sample project ---

/// The file the pictures show: `src/color.rs` of a small project ("palette"), its lines from 14.
pub const COLOR_RS: &[CodeLine] = &[
    &[
        (Some("keyword"), "impl "),
        (Some("type"), "Color"),
        (None, " {"),
    ],
    &[
        (None, "    "),
        (Some("comment"), "/// Parses `#rrggbb` and the short `#rgb`."),
    ],
    &[
        (None, "    "),
        (Some("keyword"), "pub fn "),
        (Some("function"), "from_hex"),
        (None, "(hex: &"),
        (Some("type"), "str"),
        (None, ") -> "),
        (Some("type"), "Option"),
        (None, "<"),
        (Some("type"), "Self"),
        (None, "> {"),
    ],
    &[
        (None, "        "),
        (Some("keyword"), "let "),
        (None, "digits = hex."),
        (Some("function"), "strip_prefix"),
        (None, "("),
        (Some("string"), "'#'"),
        (None, ")?;"),
    ],
    &[
        (None, "        "),
        (Some("keyword"), "let "),
        (None, "(r, g, b) = "),
        (Some("keyword"), "match "),
        (None, "digits."),
        (Some("function"), "len"),
        (None, "() {"),
    ],
    &[
        (None, "            "),
        (Some("number"), "6"),
        (None, " => "),
        (Some("function"), "parse_long"),
        (None, "(digits)?,"),
    ],
    &[
        (None, "            "),
        (Some("number"), "3"),
        (None, " => "),
        (Some("function"), "parse_short"),
        (None, "(digits)?,"),
    ],
    &[
        (None, "            _ => "),
        (Some("keyword"), "return "),
        (Some("type"), "None"),
        (None, ","),
    ],
    &[(None, "        };")],
    &[
        (None, "        "),
        (Some("type"), "Some"),
        (None, "("),
        (Some("type"), "Color"),
        (None, "::"),
        (Some("function"), "rgb"),
        (None, "(r, g, b))"),
    ],
    &[(None, "    }")],
    &[(None, "}")],
];
/// The number of [`COLOR_RS`]'s first line.
pub const COLOR_RS_FIRST: usize = 14;
/// The line of [`COLOR_RS`] with the problem: `parse_short` isn't defined (line 20).
const PROBLEM_LINE: usize = 6;
/// Where `parse_short` starts in that line, in characters.
const PROBLEM_COLUMN: usize = 17;
const PROBLEM_WORD: &str = "parse_short";

/// The project's tree as the pictures show it: `src` open, color.rs selected and changed (in the
/// color version control gives changed files).
fn project_rows(cx: &App) -> Vec<TreeRow<'static>> {
    let ui = Theme::ui(cx);
    vec![
        TreeRow::folder(0, "src", true),
        TreeRow::file(1, "color.rs").selected().colored(ui.vcs_modified),
        TreeRow::file(1, "lib.rs"),
        TreeRow::file(1, "theme.rs"),
        TreeRow::folder(0, "tests", false),
        TreeRow::file(0, "Cargo.toml"),
        TreeRow::file(0, "README.md"),
    ]
}

// --- Everything from the keyboard ---

/// The files of the project the search popup knows, with what ⌘P shows before anything is typed
/// (the recent ones, in this order).
const SEARCH_FILES: &[(&str, &str)] = &[
    ("color.rs", "src"),
    ("lib.rs", "src"),
    ("theme.rs", "src"),
    ("colors.toml", "assets/themes"),
    ("color_tests.rs", "tests"),
    ("Cargo.lock", ""),
    ("Cargo.toml", ""),
    ("README.md", ""),
];
/// The recent files: what the popup lists before anything is typed.
const RECENT_FILES: usize = 6;
/// What the picture types into the popup.
const QUERY: &str = "col";

/// Everything from the keyboard: before — the editor with the tree; the show — the window dims a
/// little, the ⌘P popup rises in with the recent files, "col" is typed letter by letter and the
/// list narrows to the files that match, color.rs first.
fn search(progress: f32, cx: &App) -> AnyElement {
    let ui = Theme::ui(cx);
    let appear = phase(progress, 0.0, 0.22);
    // A letter every 0.15 of the show after the popup is in.
    let typed = ((progress - 0.3) / 0.15).floor().clamp(-1., QUERY.len() as f32 - 1.) as isize + 1;
    let query = &QUERY[..typed as usize];
    let content = div()
        .flex_1()
        .flex()
        .gap(px(island_gap(cx)))
        .child(tree(&project_rows(cx), 150., cx))
        .child(editor_island(&["color.rs", "lib.rs"], 0, COLOR_RS, COLOR_RS_FIRST, Some(3), cx));
    let mut picture = mini_window_with("palette", "main", &[0], content, cx);
    if appear > 0. {
        picture = picture
            .child(
                div()
                    .absolute()
                    .top(px(TITLE_HEIGHT))
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .bg(UiColors::tint(opaque(ui.frame), 0.38 * appear)),
            )
            .child(
                search_popup(query, cx)
                    .absolute()
                    .left(px((SCENE_WIDTH - SEARCH_POPUP_WIDTH) / 2.))
                    .top(px(TITLE_HEIGHT + 12. + 10. * (1. - appear)))
                    .opacity(appear),
            );
    }
    picture.into_any_element()
}

const SEARCH_POPUP_WIDTH: f32 = 380.;

/// The ⌘P popup with `query` typed: the field, the matching files (the letters that match
/// highlighted, the first selected), the footer with the count and the keys.
fn search_popup(query: &str, cx: &App) -> Div {
    let ui = Theme::ui(cx);
    let matches: Vec<(&str, &str, Vec<usize>)> = if query.is_empty() {
        SEARCH_FILES[..RECENT_FILES]
            .iter()
            .map(|(name, dir)| (*name, *dir, Vec::new()))
            .collect()
    } else {
        SEARCH_FILES
            .iter()
            .filter_map(|(name, dir)| Some((*name, *dir, fuzzy(name, query)?)))
            .collect()
    };
    let count = matches.len();
    let field = div()
        .h(px(32.))
        .px(px(10.))
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(8.))
        .bg(opaque_over(ui.input_background, ui.elevated))
        .border_1()
        .border_color(ui.focus_border)
        .shadow(ring(ui))
        .child(icon(IconName::Search, ui.text_muted).size(px(13.)))
        .child(
            div()
                .flex()
                .items_center()
                .text_size(px(12.5))
                .child(query.to_string())
                .child(div().ml(px(1.)).w(px(1.5)).h(px(14.)).bg(ui.cursor)),
        )
        .child(div().flex_1());
    let rows = matches.iter().enumerate().map(|(index, (name, dir, marks))| {
        let selected = index == 0;
        div()
            .h(px(28.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(6.))
            .when(selected, |row| row.bg(ui.list_selected))
            .child(file_icon(name, &ui).render().size(px(14.)))
            .child(highlighted(name, marks, ui))
            .child(
                div()
                    .text_size(px(SCENE_TEXT))
                    .text_color(ui.dim)
                    .child(dir.to_string()),
            )
    });
    let footer = div()
        .mt_1()
        .pt(px(7.))
        .px(px(6.))
        .flex()
        .items_center()
        .gap_3()
        .border_t_1()
        .border_color(ui.divider)
        .text_size(px(SCENE_TEXT))
        .text_color(ui.dim)
        .child(format!(
            "{count} {}",
            trn(SEARCH_FILES.len(), "of {n} file", "of {n} files")
        ))
        .child(div().flex_1())
        .child(small_hint("↑↓", tr("navigate"), ui))
        .child(small_hint("↵", tr("open"), ui));
    popup(cx)
        .w(px(SEARCH_POPUP_WIDTH))
        .p(px(8.))
        .flex()
        .flex_col()
        .gap_1()
        .child(field)
        .child(div().mt_1().flex().flex_col().children(rows))
        .child(footer)
}

/// Where the letters of `query` are found in `name`, in order (a subsequence, case-insensitive),
/// as the file search matches: `None` — it doesn't match.
fn fuzzy(name: &str, query: &str) -> Option<Vec<usize>> {
    let mut found = Vec::new();
    let mut letters = query.chars().map(|c| c.to_ascii_lowercase()).peekable();
    for (index, c) in name.chars().enumerate() {
        match letters.peek() {
            Some(letter) if c.to_ascii_lowercase() == *letter => {
                found.push(index);
                letters.next();
            }
            Some(_) => {}
            None => break,
        }
    }
    letters.peek().is_none().then_some(found)
}

/// A name with the letters at `marks` in the accent, as the pickers highlight a match.
fn highlighted(name: &str, marks: &[usize], ui: UiColors) -> Div {
    let mut runs: Vec<(bool, String)> = Vec::new();
    for (index, c) in name.chars().enumerate() {
        let on = marks.contains(&index);
        match runs.last_mut() {
            Some((last_on, text)) if *last_on == on => text.push(c),
            _ => runs.push((on, c.to_string())),
        }
    }
    div()
        .flex()
        .flex_none()
        .text_size(px(12.5))
        .children(runs.into_iter().map(|(on, text)| {
            div()
                .when(on, |run| {
                    run.text_color(ui.accent_text)
                        .font_weight(FontWeight::SEMIBOLD)
                })
                .child(text)
        }))
}

/// A key and what it does, small, for the footers of the pictures' popups.
fn small_hint(keys: &str, label: &str, ui: UiColors) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(ui::keys(keys, ui))
        .child(label.to_string())
}

// --- The code understood ---

/// The code understood: before — `parse_short` underlined as a problem and its hover; the show —
/// ⌥↵: the hover goes and the fixes and actions at the caret rise in, the first one selected.
fn problem(progress: f32, cx: &App) -> AnyElement {
    let ui = Theme::ui(cx);
    let fixes = phase(progress, 0.08, 0.32);
    let hover = 1. - phase(progress, 0.0, 0.12);
    let content = editor_island_marked(
        &["color.rs", "lib.rs"],
        0,
        COLOR_RS,
        COLOR_RS_FIRST,
        Some(PROBLEM_LINE),
        Some((PROBLEM_LINE, PROBLEM_COLUMN, PROBLEM_WORD.len())),
        cx,
    );
    let content_left = content_left(cx);
    let word_x = content_left + 1. + code_x(PROBLEM_COLUMN);
    let line_y = TITLE_HEIGHT + 1. + TABS_HEIGHT + code_y(PROBLEM_LINE);
    let mut picture = mini_window_with("palette", "main", &[], content, cx);
    if hover > 0. {
        picture = picture.child(
            problem_hover(cx)
                .absolute()
                .left(px(word_x - 18.))
                .bottom(px(SCENE_HEIGHT - line_y + 4.))
                .opacity(hover),
        );
    }
    if fixes > 0. {
        // The language server's fixes come in its words (English); Claude's actions are Flux's.
        let actions: [Fix; 4] = [
            (
                IconName::Bulb,
                ui.amber,
                vec![("Create function ", false), (PROBLEM_WORD, true)],
            ),
            (
                IconName::Bulb,
                ui.amber,
                vec![("Import ", false), ("crate::hex::", true), (PROBLEM_WORD, true)],
            ),
            (IconName::Claude, ui.claude, vec![(tr("Fix with Claude"), false)]),
            (IconName::Claude, ui.claude, vec![(tr("Explain with Claude"), false)]),
        ];
        let rows = actions
            .into_iter()
            .enumerate()
            .map(|(index, (name, color, label))| {
                div()
                    .h(px(26.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(6.))
                    .text_size(px(12.))
                    .whitespace_nowrap()
                    .when(index == 0, |row| row.bg(ui.list_selected))
                    .child(icon(name, color).size(px(13.)))
                    .child(div().flex().children(label.into_iter().map(|(text, code)| {
                        div()
                            .when(code, |piece| {
                                piece
                                    .font_family(theme::code_font())
                                    .text_size(px(SCENE_CODE))
                            })
                            .child(text.replace(' ', "\u{a0}"))
                    })))
            });
        picture = picture.child(
            popup(cx)
                .absolute()
                .left(px(word_x - 8.))
                .top(px(line_y + SCENE_LINE_HEIGHT + 3. + 8. * (1. - fixes)))
                .w(px(270.))
                .p(px(5.))
                .flex()
                .flex_col()
                .children(rows)
                .opacity(fixes),
        );
    }
    picture.into_any_element()
}

/// A fix in the popup: its icon and color, and its label — pieces of text, `true` for code.
type Fix<'a> = (IconName, Hsla, Vec<(&'a str, bool)>);

/// The hover of the problem: the message, and the keys that fix it.
fn problem_hover(cx: &App) -> Div {
    let ui = Theme::ui(cx);
    let fixes_keys = ui::shortcut_for(&code_actions::ShowContextActions, cx);
    popup(cx)
        .w(px(300.))
        .px(px(10.))
        .py(px(8.))
        .flex()
        .flex_col()
        .gap(px(6.))
        .text_size(px(12.))
        .child(
            div()
                .flex()
                .gap_2()
                .child(icon(IconName::Error, ui.error).size(px(13.)).mt(px(1.)))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .child("cannot find function\u{a0}")
                        .child(
                            div()
                                .font_family(theme::code_font())
                                .text_size(px(11.5))
                                .child(PROBLEM_WORD),
                        )
                        .child("\u{a0}in this scope"),
                ),
        )
        .children(fixes_keys.map(|keys| {
            div()
                .pl(px(21.))
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(px(SCENE_TEXT))
                .text_color(ui.dim)
                .child(ui::keys(&keys, ui))
                .child(tr("Fixes and Actions"))
        }))
}

// --- The tool windows ---

/// The tool windows: before — the editor alone, the launchpad's icons with their keys beside them;
/// the show — the keys go, the project tree slides in and the terminal rises under the editor
/// with a passing `cargo test`.
fn windows(progress: f32, cx: &App) -> AnyElement {
    let ui = Theme::ui(cx);
    let labels = 1. - phase(progress, 0.0, 0.16);
    let tree_in = phase(progress, 0.1, 0.42);
    let terminal_in = phase(progress, 0.4, 0.72);
    let gap = island_gap(cx);
    let mut open = Vec::new();
    if tree_in > 0.5 {
        open.push(0);
    }
    if terminal_in > 0.5 {
        open.push(3);
    }
    const TREE_WIDTH: f32 = 140.;
    const TERMINAL_HEIGHT: f32 = 112.;
    let tree_width = TREE_WIDTH * tree_in;
    let content = div()
        .flex_1()
        .flex()
        .gap(px(if tree_width > 1. { gap } else { 0. }))
        .when(tree_width > 1., |row| {
            row.child(
                div()
                    .w(px(tree_width))
                    .flex_none()
                    .overflow_hidden()
                    .flex()
                    .child(tree(&project_rows(cx), TREE_WIDTH, cx)),
            )
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(if terminal_in > 0. { gap } else { 0. }))
                .child(editor_island(
                    &["color.rs", "lib.rs"],
                    0,
                    COLOR_RS,
                    COLOR_RS_FIRST,
                    None,
                    cx,
                ))
                .when(terminal_in > 0., |column| {
                    column.child(
                        div()
                            .h(px(TERMINAL_HEIGHT * terminal_in))
                            .flex_none()
                            .overflow_hidden()
                            .flex()
                            .child(terminal_island(TERMINAL_HEIGHT, cx)),
                    )
                }),
        );
    let mut picture = mini_window_with("palette", "main", &open, content, cx);
    if labels > 0. {
        let tools = [
            (ui::shortcut_for(&file_tree::ToggleOpen, cx), tr("Project")),
            (ui::shortcut_for(&git::ToggleCommitWindow, cx), tr("Commit")),
            (ui::shortcut_for(&project_search::Toggle, cx), tr("Find in Files")),
            (ui::shortcut_for(&terminal_panel::TogglePanel, cx), tr("Terminal")),
            (ui::shortcut_for(&git::ToggleGitWindow, cx), tr("Git")),
        ];
        for (index, (keys, name)) in tools.into_iter().enumerate() {
            let Some(keys) = keys else {
                continue;
            };
            let center = launchpad_center(index);
            picture = picture.child(
                div()
                    .absolute()
                    .left(px(LAUNCHPAD_WIDTH + 4. - 6. * (1. - labels)))
                    .top(px(center - 11.))
                    .h(px(22.))
                    .pl(px(3.))
                    .pr(px(8.))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .rounded(px(7.))
                    .bg(opaque(ui.elevated))
                    .border_1()
                    .border_color(ui.elevated_border)
                    .shadow(ui::popover_shadow(ui))
                    .text_size(px(SCENE_TEXT))
                    .child(ui::keys(&keys, ui))
                    .child(name)
                    .opacity(labels),
            );
        }
    }
    picture.into_any_element()
}

/// The terminal under the editor: its tab and a `cargo test` that passed, `height` tall.
fn terminal_island(height: f32, cx: &App) -> Div {
    let ui = Theme::ui(cx);
    let theme = Theme::get(cx);
    let terminal = theme.terminal;
    let line = |pieces: Vec<(Hsla, &str)>| {
        div()
            .flex()
            .whitespace_nowrap()
            .children(pieces.into_iter().map(|(color, text)| {
                div().text_color(color).child(text.to_string())
            }))
    };
    island(cx)
        .h(px(height))
        .flex_1()
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(26.))
                .flex_none()
                .px(px(6.))
                .flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .h(px(20.))
                        .px(px(7.))
                        .flex()
                        .items_center()
                        .gap_1()
                        .rounded(px(5.))
                        .bg(ui.pressed)
                        .child(icon(IconName::Terminal, ui.text_muted).size(px(11.)))
                        .child("zsh"),
                )
                .child(div().text_color(ui.dim).child("cargo test")),
        )
        .child(
            div()
                .px(px(10.))
                .flex()
                .flex_col()
                .font_family(theme::code_font())
                .text_size(px(SCENE_TEXT))
                .line_height(px(16.))
                .text_color(terminal.foreground)
                .child(line(vec![
                    (terminal.ansi[2], "~/palette"),
                    (terminal.foreground, " $ cargo test"),
                ]))
                .child(line(vec![(ui.dim, "running 24 tests")]))
                .child(line(vec![
                    (terminal.foreground, "test color::from_hex_short ... "),
                    (terminal.ansi[2], "ok"),
                ]))
                .child(line(vec![
                    (terminal.foreground, "test result: "),
                    (terminal.ansi[2], "ok"),
                    (terminal.foreground, ". 24 passed; 0 failed"),
                ])),
        )
}

// --- The window ---

/// The launchpad's tools, in its order: what `launchpad_on` counts.
pub const LAUNCHPAD: [IconName; 5] = [
    IconName::Project,
    IconName::Commit,
    IconName::FindInFiles,
    IconName::Terminal,
    IconName::GitLog,
];
/// A launchpad button and the gap between them.
const LAUNCHPAD_BUTTON: f32 = 22.;
const LAUNCHPAD_STEP: f32 = 26.;
const LAUNCHPAD_TOP: f32 = 2.;

/// The middle of the launchpad's button `index` from the top of the picture: where a picture
/// points at it.
pub fn launchpad_center(index: usize) -> f32 {
    1. + TITLE_HEIGHT + LAUNCHPAD_TOP + LAUNCHPAD_STEP * index as f32 + LAUNCHPAD_BUTTON / 2.
}

/// The Flux window drawn small in the theme's colors and the window's look: the title bar with the
/// traffic lights, the project and its branch; the launchpad on the left (`launchpad_on` — the open
/// tool, its place in [`LAUNCHPAD`]); `content` fills the rest (islands made with [`island`]).
pub fn mini_window(
    project: &str,
    branch: &str,
    launchpad_on: Option<usize>,
    content: impl IntoElement,
    cx: &App,
) -> Div {
    mini_window_with(project, branch, launchpad_on.as_slice(), content, cx)
}

/// [`mini_window`] with several tools open (the tree and the terminal).
pub fn mini_window_with(
    project: &str,
    branch: &str,
    open: &[usize],
    content: impl IntoElement,
    cx: &App,
) -> Div {
    let ui = Theme::ui(cx);
    let light = |color: u32| -> Hsla { gpui::rgb(color).into() };
    let lights = div()
        .flex()
        .gap(px(6.))
        .child(div().size(px(9.)).rounded(px(5.)).bg(light(0xff5f57)))
        .child(div().size(px(9.)).rounded(px(5.)).bg(light(0xfebc2e)))
        .child(div().size(px(9.)).rounded(px(5.)).bg(light(0x28c840)));
    let title = div()
        .h(px(TITLE_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .px(px(10.))
        .child(lights)
        .child(
            div()
                .ml_2()
                .flex()
                .items_center()
                .gap_1()
                .font_weight(FontWeight::SEMIBOLD)
                .child(icon(IconName::Folder, ui.folder).size(px(12.)))
                .child(project.to_string()),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .px(px(6.))
                .py(px(2.))
                .rounded(px(5.))
                .bg(UiColors::tint(ui.violet, 0.14))
                .text_color(ui.violet)
                .text_size(px(SCENE_TEXT - 1.))
                .child(icon(IconName::Branch, ui.violet).size(px(10.)))
                .child(branch.to_string()),
        );
    let launchpad = div()
        .w(px(LAUNCHPAD_WIDTH))
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(LAUNCHPAD_STEP - LAUNCHPAD_BUTTON))
        .pt(px(LAUNCHPAD_TOP))
        .children(LAUNCHPAD.iter().enumerate().map(|(index, name)| {
            let on = open.contains(&index);
            div()
                .size(px(LAUNCHPAD_BUTTON))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .when(on, |button| button.bg(ui.accent_soft))
                .child(
                    icon(*name, if on { ui.accent_text } else { ui.text_muted }).size(px(13.)),
                )
        }));
    let gap = island_gap(cx);
    div()
        .relative()
        .w(px(SCENE_WIDTH))
        .h(px(SCENE_HEIGHT))
        .flex()
        .flex_col()
        .rounded(px(10.))
        .overflow_hidden()
        .bg(opaque(ui.frame))
        .border_1()
        .border_color(ui.island_border)
        .shadow(ui::popover_shadow(ui))
        .font_family(theme::UI_FONT)
        .text_size(px(SCENE_TEXT))
        .text_color(ui.foreground)
        .child(title)
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .gap(px(gap))
                .pr(px(gap))
                .pb(px(gap))
                .child(launchpad)
                .child(div().flex_1().min_w_0().flex().gap(px(gap)).child(content)),
        )
}

/// The gap between the small window's islands: none in Flat, where panels meet at a line.
pub fn island_gap(cx: &App) -> f32 {
    match ui::style(cx) {
        UiStyle::Islands => 6.,
        UiStyle::Flat => 0.,
    }
}

/// Where the content of [`mini_window`] starts, from the picture's left edge (inside its border).
pub fn content_left(cx: &App) -> f32 {
    LAUNCHPAD_WIDTH + island_gap(cx)
}

/// An island of the small window: rounded with a border in Islands, a plain panel in Flat (a line
/// on its left divides it from the launchpad or the panel before it).
pub fn island(cx: &App) -> Div {
    let ui = Theme::ui(cx);
    match ui::style(cx) {
        UiStyle::Islands => div()
            .relative()
            .rounded(px(8.))
            .bg(opaque(ui.island))
            .border_1()
            .border_color(ui.island_border)
            .overflow_hidden(),
        UiStyle::Flat => div()
            .relative()
            .bg(opaque_over(ui.island, ui.frame))
            .border_l_1()
            .border_t_1()
            .border_color(ui.island_border)
            .overflow_hidden(),
    }
}

/// A row of [`tree`].
#[derive(Debug, Clone, Copy)]
pub struct TreeRow<'a> {
    pub depth: usize,
    pub name: &'a str,
    /// A folder, and whether it is open; `None` — a file.
    pub folder: Option<bool>,
    pub selected: bool,
    /// The name's color (a file changed in git); `None` — the text color.
    pub color: Option<Hsla>,
}

impl<'a> TreeRow<'a> {
    pub fn file(depth: usize, name: &'a str) -> Self {
        Self {
            depth,
            name,
            folder: None,
            selected: false,
            color: None,
        }
    }

    pub fn folder(depth: usize, name: &'a str, open: bool) -> Self {
        Self {
            folder: Some(open),
            ..Self::file(depth, name)
        }
    }

    pub fn selected(self) -> Self {
        Self {
            selected: true,
            ..self
        }
    }

    pub fn colored(self, color: Hsla) -> Self {
        Self {
            color: Some(color),
            ..self
        }
    }
}

/// The project's tree in an island `width` wide: the rows with their icons (the chosen set of file
/// icons), the selected one marked as the tree marks it without focus.
pub fn tree(rows: &[TreeRow], width: f32, cx: &App) -> Div {
    let ui = Theme::ui(cx);
    island(cx)
        .w(px(width))
        .flex_none()
        .py(px(6.))
        .px(px(4.))
        .flex()
        .flex_col()
        .children(rows.iter().map(|row| {
            let icon = match row.folder {
                Some(open) => folder_icon(open, &ui),
                None => file_icon(row.name, &ui),
            };
            div()
                .h(px(21.))
                .pl(px(5. + 11. * row.depth as f32))
                .flex()
                .items_center()
                .gap(px(5.))
                .rounded(px(5.))
                .whitespace_nowrap()
                .when(row.selected, |line| line.bg(ui.list_selected_inactive))
                .child(icon.render().size(px(12.)))
                .child(
                    div()
                        .text_color(row.color.unwrap_or(ui.foreground))
                        .child(row.name.to_string()),
                )
        }))
}

/// An editor: the tab bar with `tabs` (`active` one current) and the lines of code.
pub fn editor_island(
    tabs: &[&str],
    active: usize,
    lines: &[CodeLine],
    first: usize,
    current: Option<usize>,
    cx: &App,
) -> Div {
    island(cx)
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(tab_bar(tabs, active, cx))
        .child(code_lines(lines, first, current, cx))
}

/// [`editor_island`] with a problem underlined: `(line, column, length)` in characters.
fn editor_island_marked(
    tabs: &[&str],
    active: usize,
    lines: &[CodeLine],
    first: usize,
    current: Option<usize>,
    problem: Option<(usize, usize, usize)>,
    cx: &App,
) -> Div {
    island(cx)
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(tab_bar(tabs, active, cx))
        .child(code_lines_marked(lines, first, current, problem, cx))
}

/// The tab bar of an editor: the files' tabs, `active` one current.
pub fn tab_bar(tabs: &[&str], active: usize, cx: &App) -> Div {
    let ui = Theme::ui(cx);
    div()
        .h(px(TABS_HEIGHT))
        .flex_none()
        .px(px(6.))
        .flex()
        .items_center()
        .gap_1()
        .border_b_1()
        .border_color(ui.divider)
        .children(tabs.iter().enumerate().map(|(index, name)| {
            let on = index == active;
            div()
                .h(px(22.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(5.))
                .rounded(px(6.))
                .whitespace_nowrap()
                .when(on, |tab| tab.bg(ui.pressed))
                .text_color(if on { ui.foreground } else { ui.text_muted })
                .child(file_icon(name, &ui).render().size(px(11.)))
                .child(name.to_string())
        }))
}

/// A line of code: pieces with their highlight scope (`None` — plain text), colored by the theme.
pub type CodeLine<'a> = &'a [(Option<&'a str>, &'a str)];

/// Lines of code with their numbers from `first`, in the code font and the theme's colors;
/// `current` — the caret's line, with the current line's background.
pub fn code_lines(lines: &[CodeLine], first: usize, current: Option<usize>, cx: &App) -> Div {
    code_lines_marked(lines, first, current, None, cx)
}

/// [`code_lines`] with a problem underlined with a wavy line in the error color: `(line, column,
/// length)` in characters (within one piece of the line).
fn code_lines_marked(
    lines: &[CodeLine],
    first: usize,
    current: Option<usize>,
    problem: Option<(usize, usize, usize)>,
    cx: &App,
) -> Div {
    let theme = Theme::get(cx);
    let ui = theme.ui;
    div()
        .pt(px(CODE_TOP))
        .flex()
        .flex_col()
        .font_family(theme::code_font())
        .text_size(px(SCENE_CODE))
        .line_height(px(SCENE_LINE_HEIGHT))
        .children(lines.iter().enumerate().map(|(index, line)| {
            let mut column = 0;
            let pieces = line.iter().map(|(scope, text)| {
                let style = scope.and_then(|scope| theme.style_for(scope));
                let start = column;
                column += text.chars().count();
                let marked = problem
                    .is_some_and(|(at, from, _)| at == index && from >= start && from < column);
                div()
                    .flex_none()
                    .text_color(style.map_or(ui.foreground, |style| style.color))
                    .when(style.is_some_and(|style| style.bold), |piece| {
                        piece.font_weight(FontWeight::BOLD)
                    })
                    .when(style.is_some_and(|style| style.italic), |piece| piece.italic())
                    .when(marked, |piece| {
                        piece
                            .underline()
                            .text_decoration_wavy()
                            .text_decoration_1()
                            .text_decoration_color(ui.error)
                    })
                    .child(text.to_string())
            });
            div()
                .h(px(SCENE_LINE_HEIGHT))
                .flex()
                .items_center()
                .whitespace_nowrap()
                .when(current == Some(index), |line| line.bg(ui.current_line))
                .child(
                    div()
                        .w(px(GUTTER_WIDTH))
                        .pr(px(12.))
                        .flex()
                        .justify_end()
                        .flex_none()
                        .text_color(ui.dim)
                        .child((first + index).to_string()),
                )
                .children(pieces)
        }))
}

/// Where column `column` of [`code_lines`] starts, from the island's inner left edge.
pub fn code_x(column: usize) -> f32 {
    GUTTER_WIDTH + CODE_ADVANCE * column as f32
}

/// Where line `line` of [`code_lines`] starts, from the top of the code (under the tab bar).
pub fn code_y(line: usize) -> f32 {
    CODE_TOP + SCENE_LINE_HEIGHT * line as f32
}

/// A popup of the small window — a list at the caret, a search, a hover: the elevated surface
/// with its border and shadow.
pub fn popup(cx: &App) -> Div {
    let ui = Theme::ui(cx);
    div()
        .rounded(px(10.))
        .bg(opaque(ui.elevated))
        .border_1()
        .border_color(ui.elevated_border)
        .shadow(ui::popover_shadow(ui))
        .text_color(ui.foreground)
}

/// The focus ring of a field.
fn ring(ui: UiColors) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: ui.focus_ring,
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(2.),
    }]
}

/// A color without the glass's transparency: the picture sits on the guide's island, so it is
/// drawn opaque.
pub fn opaque(color: Hsla) -> Hsla {
    Hsla { a: 1., ..color }
}

/// `color` laid over `under` (both made opaque where the theme's glass lets the desktop through):
/// what a translucent token looks like on the surface it is drawn on.
pub fn opaque_over(color: Hsla, under: Hsla) -> Hsla {
    let top = color.to_rgb();
    let bottom = opaque(under).to_rgb();
    let mix = |a: f32, b: f32| a * top.a + b * (1. - top.a);
    gpui::Rgba {
        r: mix(top.r, bottom.r),
        g: mix(top.g, bottom.g),
        b: mix(top.b, bottom.b),
        a: 1.,
    }
    .into()
}

/// The part of a show between `from` and `to` (both 0–1), eased out: 0 before it, 1 after it.
pub fn phase(progress: f32, from: f32, to: f32) -> f32 {
    let t = ((progress - from) / (to - from)).clamp(0., 1.);
    1. - (1. - t).powi(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_query_matches_as_the_file_search_does() {
        assert_eq!(fuzzy("color.rs", "col"), Some(vec![0, 1, 2]));
        assert_eq!(fuzzy("Cargo.lock", "col"), Some(vec![0, 4, 6]));
        assert_eq!(fuzzy("lib.rs", "col"), None);
        // Five of the eight files match "col": the list the picture ends with.
        let matching = SEARCH_FILES
            .iter()
            .filter(|(name, _)| fuzzy(name, QUERY).is_some())
            .count();
        assert_eq!(matching, 5);
    }

    #[test]
    fn the_problem_word_is_where_the_picture_points() {
        let line: String = COLOR_RS[PROBLEM_LINE].iter().map(|(_, text)| *text).collect();
        let word: String = line
            .chars()
            .skip(PROBLEM_COLUMN)
            .take(PROBLEM_WORD.len())
            .collect();
        assert_eq!(word, PROBLEM_WORD);
    }

    #[test]
    fn phases_ease_from_zero_to_one() {
        assert_eq!(phase(0., 0.2, 0.6), 0.);
        assert_eq!(phase(0.2, 0.2, 0.6), 0.);
        assert_eq!(phase(0.6, 0.2, 0.6), 1.);
        assert_eq!(phase(1., 0.2, 0.6), 1.);
        assert!(phase(0.4, 0.2, 0.6) > 0.5);
    }
}
