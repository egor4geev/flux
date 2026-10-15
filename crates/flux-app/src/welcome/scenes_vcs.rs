//! The tour's pictures of version control and of Claude Code ([`super::scenes`]): the commit window
//! with the changes and the log's graph; the chat beside the code with an edit and "Done".
//!
//! Both are pure functions of their show's `progress` (0 — before the task's keys, 1 — after), as
//! every picture of the tour. Their steps are cut with [`phase`] (eased) and [`linear`] (typing);
//! the islands slide open by their width, their contents keep their own width and are uncovered as
//! a drawer would be, so nothing reflows while a show runs.

use std::time::Duration;

use gpui::{
    AnyElement, App, Div, FontWeight, Hsla, IntoElement, SharedString, div, linear_color_stop,
    linear_gradient, prelude::*, px,
};

use super::scenes::{self, CodeLine, SCENE_CODE, SCENE_LINE_HEIGHT, SCENE_TEXT, opaque, phase};
use crate::i18n::{tr, trf, trn};
use crate::icons::{self, IconName, icon};
use crate::settings::UiStyle;
use crate::theme::{self, Theme, UiColors};
use crate::ui;

/// The commit window's island, open: wide enough for a file, its folder and its counts.
const COMMIT_WIDTH: f32 = 214.;
/// The log under the editor: its header and five commits.
const LOG_HEIGHT: f32 = 150.;
const LOG_ROW: f32 = 24.;
/// The commit window's rows of changes.
const CHANGE_ROW: f32 = 22.;
/// The chat's island, open.
const CHAT_WIDTH: f32 = 292.;
/// The tab bar of the small window's editor.
const TABS_HEIGHT: f32 = 28.;
/// A lane of the log's graph: its distance from the previous one, the first one's from the edge.
const LANE_STEP: f32 = 12.;
const LANE_FIRST: f32 = 12.;

/// The project's `color.rs` once the short form is parsed: the change the commit takes, and
/// Claude's edit in the other picture.
const COLOR_RS: &[CodeLine] = &[
    &[
        (Some("keyword"), "impl "),
        (Some("type"), "Color"),
        (None, " {"),
    ],
    &[(
        Some("comment"),
        "    /// Parses `#rrggbb` and the short `#rgb`.",
    )],
    &[
        (Some("keyword"), "    pub fn "),
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
        (Some("keyword"), "        let "),
        (None, "digits = hex."),
        (Some("function"), "strip_prefix"),
        (None, "("),
        (Some("string"), "'#'"),
        (None, ")?;"),
    ],
    &[
        (Some("keyword"), "        let "),
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

/// `color.rs` before Claude's edit: the long form only.
const COLOR_RS_BEFORE: &[CodeLine] = &[
    &[
        (Some("keyword"), "impl "),
        (Some("type"), "Color"),
        (None, " {"),
    ],
    &[(Some("comment"), "    /// Parses `#rrggbb`.")],
    &[
        (Some("keyword"), "    pub fn "),
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
        (Some("keyword"), "        let "),
        (None, "digits = hex."),
        (Some("function"), "strip_prefix"),
        (None, "("),
        (Some("string"), "'#'"),
        (None, ")?;"),
    ],
    &[
        (Some("keyword"), "        let "),
        (None, "(r, g, b) = "),
        (Some("function"), "parse_long"),
        (None, "(digits)?;"),
    ],
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

/// The first line number of the code shown.
const FIRST_LINE: usize = 14;

/// The part of a show between `from` and `to`, at an even pace (typing): 0 before, 1 after.
fn linear(progress: f32, from: f32, to: f32) -> f32 {
    ((progress - from) / (to - from)).clamp(0., 1.)
}

/// The gap between the small window's islands: none in Flat, where panels meet at a line.
fn gap(cx: &App) -> f32 {
    if ui::style(cx) == UiStyle::Flat {
        0.
    } else {
        6.
    }
}

fn flat(cx: &App) -> bool {
    ui::style(cx) == UiStyle::Flat
}

// --- Version control ---

/// Version control: before — the changed file in the editor, the log under it; the show — ⌘K opens
/// the commit window, the message is typed, Commit takes the two checked files (the third stays: a
/// commit in parts) and the commit comes up at the top of the log, the branch one ahead.
pub fn git(progress: f32, cx: &App) -> AnyElement {
    let open = phase(progress, 0., 0.26);
    let typed = linear(progress, 0.28, 0.7);
    let pressed = phase(progress, 0.72, 0.8);
    let committed = phase(progress, 0.8, 1.);
    let gap = gap(cx);
    let commit = (open > 0.01).then(|| {
        commit_window(typed, pressed, committed, cx)
            .w(px(COMMIT_WIDTH * open))
            .mr(px(gap * open))
            .opacity(open)
    });
    let marks: Vec<(usize, Hsla)> = {
        let ui = Theme::ui(cx);
        vec![
            (4, ui.diff_modified),
            (5, ui.diff_modified),
            (6, ui.diff_added),
            (7, ui.diff_added),
        ]
    };
    let editor = editor_island(
        COLOR_RS,
        &marks,
        1. - committed,
        Some(6),
        committed < 0.5,
        cx,
    )
    .flex_1()
    .min_h_0();
    let log = log_island(committed, cx).h(px(LOG_HEIGHT)).flex_none();
    let content = div().flex_1().min_w_0().flex().children(commit).child(
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(gap))
            .child(editor)
            .child(log),
    );
    let branch = if committed > 0.5 { "main ↑1" } else { "main" };
    let launchpad = (open > 0.01).then_some(1);
    scenes::mini_window("palette", branch, launchpad, content, cx).into_any_element()
}

/// A changed file of the commit window: name, folder, counts, checked.
struct Change {
    name: &'static str,
    folder: &'static str,
    added: usize,
    deleted: usize,
    checked: bool,
}

const CHANGES: [Change; 3] = [
    Change {
        name: "color.rs",
        folder: "src",
        added: 12,
        deleted: 3,
        checked: true,
    },
    Change {
        name: "lib.rs",
        folder: "src",
        added: 1,
        deleted: 0,
        checked: true,
    },
    Change {
        name: "README.md",
        folder: "",
        added: 4,
        deleted: 1,
        checked: false,
    },
];

/// The commit window: its header with the branch, the changes, the message as it is typed (`typed`
/// of it), Commit (pressed by `pressed`) and, once `committed`, only the file left out.
fn commit_window(typed: f32, pressed: f32, committed: f32, cx: &App) -> Div {
    let ui = Theme::ui(cx);
    let done = committed > 0.5;
    let message = tr("Short #rgb form in from_hex");
    let shown_chars = (message.chars().count() as f32 * typed).round() as usize;
    let text: String = if done {
        String::new()
    } else {
        message.chars().take(shown_chars).collect()
    };
    let typing = typed > 0. && !done;
    let changes: Vec<&Change> = CHANGES
        .iter()
        .filter(|change| !(done && change.checked))
        .collect();
    let header = div()
        .h(px(26.))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(8.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(icon(IconName::Commit, ui.text_muted).size(px(13.)))
        .child(tr("Commit"))
        .child(div().flex_1())
        .child(branch_label("main", true, ui));
    let section = div()
        .px(px(10.))
        .pt(px(4.))
        .pb(px(2.))
        .child(ui::section_label(
            format!("{} · {}", tr("Changes"), changes.len()),
            ui,
        ));
    let rows = changes.into_iter().map(|change| change_row(change, ui));
    let field = div()
        .mx(px(8.))
        .h(px(54.))
        .flex_none()
        .px(px(8.))
        .py(px(6.))
        .rounded(px(6.))
        .bg(opaque(ui.input_background))
        .border_1()
        .border_color(if typing {
            ui.focus_border
        } else {
            ui.input_border
        })
        .when(text.is_empty(), |field| {
            field.text_color(ui.dim).child(tr("Commit Message"))
        })
        .when(!text.is_empty(), |field| {
            field.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .child(text.clone())
                    .when(typing, |line| {
                        line.child(div().ml(px(1.)).w(px(1.5)).h(px(13.)).bg(ui.accent_text))
                    }),
            )
        });
    // The footer as the commit window has it: Amend, then Commit (pressed by `pressed`) and Commit
    // and Push….
    let press = if done { 0. } else { pressed };
    let amend = div()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(8.))
        .pt(px(8.))
        .text_color(ui.text_muted)
        .child(
            div()
                .size(px(12.))
                .rounded(px(3.))
                .border_1()
                .border_color(ui.text_muted),
        )
        .child(tr("Amend"));
    let commit_button = div()
        .h(px(24.))
        .px(px(10.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .border_1()
        .border_color(UiColors::tint(ui.accent, 0.55))
        .bg(UiColors::tint(ui.accent, 0.24 + 0.2 * press))
        .text_color(ui.accent_text)
        .font_weight(FontWeight::MEDIUM)
        .child(tr("Commit"));
    let push_button = div()
        .h(px(24.))
        .px(px(9.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .border_1()
        .border_color(ui.input_border)
        .child(tr("Commit and Push…"));
    let content = div()
        .w(px(COMMIT_WIDTH))
        .h_full()
        .flex()
        .flex_col()
        .child(header)
        .child(section)
        .children(rows)
        .child(div().flex_1())
        .child(field)
        .child(amend)
        .child(
            div()
                .flex()
                .gap(px(8.))
                .px(px(8.))
                .pt(px(8.))
                .pb(px(8.))
                .whitespace_nowrap()
                .child(commit_button)
                .child(push_button),
        );
    scenes::island(cx)
        .flex_none()
        .h_full()
        .when(flat(cx), |island| {
            island.border_r_1().border_color(ui.island_border)
        })
        .child(content)
}

/// A row of the commit window's changes: the box, the file's icon and name in its version control
/// color, the folder, the counts.
fn change_row(change: &Change, ui: UiColors) -> Div {
    let checkbox = div()
        .size(px(12.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .when(change.checked, |box_| {
            box_.bg(ui.accent)
                .child(icon(IconName::Check, ui.on_accent).size(px(9.)))
        })
        .when(!change.checked, |box_| {
            box_.border_1().border_color(ui.text_muted)
        });
    div()
        .h(px(CHANGE_ROW))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(10.))
        .whitespace_nowrap()
        .child(checkbox)
        .child(icons::file_icon(change.name, &ui).render().size(px(12.)))
        .child(div().text_color(ui.vcs_modified).child(change.name))
        .child(div().text_color(ui.dim).child(change.folder))
        .child(div().flex_1())
        .child(
            div()
                .flex()
                .gap(px(4.))
                .text_size(px(SCENE_TEXT - 0.5))
                .child(
                    div()
                        .text_color(ui.diff_added)
                        .child(format!("+{}", change.added)),
                )
                .when(change.deleted > 0, |counts| {
                    counts.child(
                        div()
                            .text_color(ui.error)
                            .child(format!("−{}", change.deleted)),
                    )
                }),
        )
}

/// A branch's label, as the log draws them: the current branch in the accent, others green.
fn branch_label(name: &str, current: bool, ui: UiColors) -> Div {
    ref_label(
        IconName::Branch,
        name,
        if current { ui.accent } else { ui.green },
        current,
    )
}

/// A tag's label: amber, with the tag's icon.
fn tag_label(name: &str, ui: UiColors) -> Div {
    ref_label(IconName::Tag, name, ui.amber, false)
}

fn ref_label(glyph: IconName, name: &str, color: Hsla, bold: bool) -> Div {
    div()
        .flex_none()
        .h(px(16.))
        .px(px(5.))
        .flex()
        .items_center()
        .gap(px(3.))
        .rounded(px(4.))
        .bg(UiColors::tint(color, 0.16))
        .text_color(color)
        .text_size(px(SCENE_TEXT - 1.))
        .when(bold, |label| label.font_weight(FontWeight::SEMIBOLD))
        .child(icon(glyph, color).size(px(10.)))
        .child(name.to_string())
}

/// A commit of the log: its lane, the lanes passing by, its labels, message and time.
struct LogCommit {
    lane: usize,
    rails: &'static [usize],
    labels: &'static [(&'static str, bool)],
    message: LogMessage,
    time: (bool, &'static str),
}

/// A commit's message: a sentence of the picture (translated), or git's own words.
enum LogMessage {
    Translated(&'static str),
    Verbatim(&'static str),
}

/// The log's commits before the new one: a merge of a theme branch, the branch's commit, older work
/// and the first commit with its tag. `labels`: (name, is a tag); `time`: (today, clock). The new
/// commit pushes the last one out of the island.
const HISTORY: [LogCommit; 5] = [
    LogCommit {
        lane: 0,
        rails: &[0, 1],
        labels: &[("main", false)],
        message: LogMessage::Verbatim("Merge branch 'feature/theme'"),
        time: (true, "12:40"),
    },
    LogCommit {
        lane: 1,
        rails: &[0, 1],
        labels: &[("feature/theme", false)],
        message: LogMessage::Translated("Theme: warmer colors"),
        time: (true, "11:05"),
    },
    LogCommit {
        lane: 0,
        rails: &[0, 1],
        labels: &[],
        message: LogMessage::Translated("Palette: shades"),
        time: (false, "18:20"),
    },
    LogCommit {
        lane: 0,
        rails: &[0],
        labels: &[],
        message: LogMessage::Translated("Tests for from_hex"),
        time: (false, "16:02"),
    },
    LogCommit {
        lane: 0,
        rails: &[0],
        labels: &[("v0.1", true)],
        message: LogMessage::Translated("First commit"),
        time: (false, "10:12"),
    },
];

/// The commit the show makes.
const NEW_COMMIT: LogCommit = LogCommit {
    lane: 0,
    rails: &[0],
    labels: &[("main", false)],
    message: LogMessage::Translated("Short #rgb form in from_hex"),
    time: (true, "14:55"),
};

/// The log: its header, then the commits with the graph; `committed` brings the new commit in at
/// the top (the branch's label moves to it).
fn log_island(committed: f32, cx: &App) -> Div {
    let ui = Theme::ui(cx);
    let keys = ui::shortcut_for(&crate::git::ToggleGitWindow, cx);
    let header = div()
        .h(px(26.))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(icon(IconName::GitLog, ui.text_muted).size(px(13.)))
        .child(tr("Log"))
        .child(div().flex_1())
        .children(keys.map(|keys| ui::keys(&keys, ui)));
    let moved = committed > 0.5;
    let new_row = (committed > 0.01).then(|| {
        log_row(&NEW_COMMIT, true, ui)
            .h(px(LOG_ROW * committed))
            .opacity(committed)
            .bg(UiColors::tint(ui.accent, 0.1 * committed))
    });
    let rows = HISTORY.iter().enumerate().map(|(index, commit)| {
        // The branch's label leaves the merge once the new commit holds it.
        let labels = index == 0 && moved;
        log_row(commit, !labels, ui)
    });
    scenes::island(cx)
        .flex()
        .flex_col()
        .when(flat(cx), |island| {
            island.border_t_1().border_color(ui.island_border)
        })
        .child(header)
        .children(new_row)
        .children(rows)
}

/// A row of the log: the graph's rails and the commit's dot, its labels (`labels` — drawn), message
/// and time.
fn log_row(commit: &LogCommit, labels: bool, ui: UiColors) -> Div {
    let lane_x = |lane: usize| LANE_FIRST + lane as f32 * LANE_STEP;
    let lanes = ui.graph_lanes;
    let rails = commit.rails.iter().map(|lane| {
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(lane_x(*lane) - 1.))
            .w(px(2.))
            .bg(UiColors::tint(lanes[*lane % lanes.len()], 0.6))
    });
    let dot = div()
        .absolute()
        .top(px(LOG_ROW / 2. - 4.))
        .left(px(lane_x(commit.lane) - 4.))
        .size(px(8.))
        .rounded(px(4.))
        .bg(lanes[commit.lane % lanes.len()])
        .border_1()
        .border_color(opaque(ui.island));
    let message = match commit.message {
        LogMessage::Translated(text) => tr(text).to_string(),
        LogMessage::Verbatim(text) => text.to_string(),
    };
    let (today, clock) = commit.time;
    let time = if today {
        trf("Today {0}", &[&clock])
    } else {
        trf("Yesterday {0}", &[&clock])
    };
    let graph_width = LANE_FIRST * 2. + LANE_STEP;
    div()
        .relative()
        .h(px(LOG_ROW))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.))
        .pr(px(10.))
        .overflow_hidden()
        .whitespace_nowrap()
        .child(
            div()
                .relative()
                .w(px(graph_width))
                .h_full()
                .flex_none()
                .children(rails)
                .child(dot),
        )
        .when(labels, |row| {
            row.children(commit.labels.iter().map(|(name, tag)| {
                if *tag {
                    tag_label(name, ui)
                } else {
                    branch_label(name, *name == "main", ui)
                }
            }))
        })
        .child(div().flex_1().min_w_0().overflow_hidden().child(message))
        .child(
            div()
                .flex_none()
                .text_size(px(SCENE_TEXT - 1.))
                .text_color(ui.dim)
                .child(time),
        )
}

/// The small window's editor: the tab bar (`color.rs` and `lib.rs`, named in the color of a changed
/// file while `changed`) and the code with its change marks in the gutter (`marks`: line index and
/// color, at `marks_alpha`); `current` — the caret's line.
fn editor_island(
    lines: &[CodeLine],
    marks: &[(usize, Hsla)],
    marks_alpha: f32,
    current: Option<usize>,
    changed: bool,
    cx: &App,
) -> Div {
    let ui = Theme::ui(cx);
    let name_color = if changed {
        ui.vcs_modified
    } else {
        ui.foreground
    };
    let tab = |name: &'static str, active: bool| {
        div()
            .h(px(20.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(5.))
            .rounded(px(5.))
            .when(active, |tab| {
                tab.bg(ui.pressed).border_1().border_color(ui.island_border)
            })
            .child(icons::file_icon(name, &ui).render().size(px(11.)))
            .child(
                div()
                    .text_color(if active {
                        name_color
                    } else {
                        UiColors::tint(name_color, 0.7)
                    })
                    .child(name),
            )
    };
    let tabs = div()
        .h(px(TABS_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(4.))
        .px(px(6.))
        .border_b_1()
        .border_color(ui.divider)
        .child(tab("color.rs", true))
        .child(tab("lib.rs", false));
    let gutter = marks.iter().map(|(line, color)| {
        div()
            .absolute()
            .left(px(36.))
            .top(px(6. + *line as f32 * SCENE_LINE_HEIGHT + 2.))
            .w(px(3.))
            .h(px(SCENE_LINE_HEIGHT - 4.))
            .rounded(px(1.5))
            .bg(UiColors::tint(*color, color.a * marks_alpha))
    });
    scenes::island(cx).flex().flex_col().child(tabs).child(
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(scenes::code_lines(lines, FIRST_LINE, current, cx))
            .children(gutter),
    )
}

// --- Claude Code ---

/// Claude Code: before — the editor alone; the show — ⌘Esc opens the chat beside it: the message,
/// Claude reads the file, its edit lands in the editor too (marked as changed), and the turn ends
/// with "Done" — review and rollback.
pub fn claude(progress: f32, cx: &App) -> AnyElement {
    let open = phase(progress, 0., 0.24);
    let asked = phase(progress, 0.18, 0.36);
    let read = phase(progress, 0.38, 0.52);
    let edited = phase(progress, 0.54, 0.74);
    let done = phase(progress, 0.76, 0.94);
    let ui = Theme::ui(cx);
    let gap = gap(cx);
    let after = edited > 0.5;
    let (code, marks): (&[CodeLine], Vec<(usize, Hsla)>) = if after {
        (
            COLOR_RS,
            vec![
                (1, ui.diff_modified),
                (4, ui.diff_modified),
                (5, ui.diff_added),
                (6, ui.diff_added),
                (7, ui.diff_added),
                (8, ui.diff_added),
            ],
        )
    } else {
        (COLOR_RS_BEFORE, Vec::new())
    };
    let editor = editor_island(code, &marks, edited, Some(4), after, cx)
        .flex_1()
        .min_w_0();
    let chat = (open > 0.01).then(|| {
        chat_island(asked, read, edited, done, cx)
            .w(px(CHAT_WIDTH * open))
            .ml(px(gap * open))
            .opacity(open)
    });
    let content = div().flex_1().min_w_0().flex().child(editor).children(chat);
    scenes::mini_window("palette", "main", None, content, cx).into_any_element()
}

/// The chat: the session's tab, the turn as far as the show is (`asked`, `read`, `edited`, `done`),
/// the field for the next message.
fn chat_island(asked: f32, read: f32, edited: f32, done: f32, cx: &App) -> Div {
    let ui = Theme::ui(cx);
    let working = done < 0.5;
    let status =
        div()
            .size(px(6.))
            .rounded(px(3.))
            .bg(if working { ui.accent } else { ui.success });
    let tabs = div()
        .h(px(TABS_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(6.))
        .border_b_1()
        .border_color(ui.divider)
        .child(
            div()
                .h(px(20.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(5.))
                .bg(ui.pressed)
                .border_1()
                .border_color(ui.island_border)
                .whitespace_nowrap()
                .child(tr("Short #rgb in from_hex"))
                .child(status),
        )
        .child(div().flex_1())
        .child(icon(IconName::History, ui.text_muted).size(px(12.)));
    // A step of the turn comes in from a little below as it fades in.
    let step = |amount: f32, element: Div| {
        (amount > 0.01).then(|| element.opacity(amount).mt(px(6. * (1. - amount))))
    };
    let bubble = div().flex().justify_end().child(
        div()
            .max_w(px(210.))
            .px(px(10.))
            .pt(px(6.))
            .pb(px(7.))
            .rounded(px(12.))
            .rounded_br(px(4.))
            .bg(UiColors::tint(ui.accent, 0.17))
            .border_1()
            .border_color(UiColors::tint(ui.accent, 0.26))
            .child(tr("Make from_hex accept the short #rgb form too")),
    );
    let read_row = div()
        .flex()
        .items_center()
        .gap(px(7.))
        .whitespace_nowrap()
        .child(avatar(ui))
        .child(div().text_color(ui.text_muted).child("Read"))
        .child(
            div()
                .font_family(theme::code_font())
                .text_size(px(SCENE_TEXT - 0.5))
                .child("src/color.rs"),
        )
        .child(div().flex_1())
        .child(
            div()
                .text_size(px(SCENE_TEXT - 1.))
                .text_color(ui.dim)
                .child(trn(90, "{n} line", "{n} lines")),
        );
    let edit = div()
        .ml(px(AVATAR + 7.))
        .flex()
        .flex_col()
        .rounded(px(8.))
        .overflow_hidden()
        .border_1()
        .border_color(UiColors::tint(ui.foreground, 0.09))
        .bg(UiColors::tint(ui.foreground, 0.03))
        .child(
            div()
                .h(px(24.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(6.))
                .border_b_1()
                .border_color(ui.divider)
                .child(icon(IconName::Pencil, ui.text_muted).size(px(11.)))
                .child(
                    div()
                        .font_family(theme::code_font())
                        .text_size(px(SCENE_TEXT - 0.5))
                        .child("src/color.rs"),
                )
                .child(div().text_color(ui.diff_added).child("+3"))
                .child(div().text_color(ui.error).child("−1")),
        )
        .child(
            div()
                .py(px(3.))
                .font_family(theme::code_font())
                .text_size(px(SCENE_CODE - 1.5))
                .line_height(px(15.))
                .whitespace_nowrap()
                .overflow_hidden()
                .child(diff_line(
                    false,
                    &[
                        (Some("keyword"), "let "),
                        (None, "(r, g, b) = "),
                        (Some("function"), "parse_long"),
                        (None, "(d)?;"),
                    ],
                    cx,
                ))
                .child(diff_line(
                    true,
                    &[
                        (Some("keyword"), "let "),
                        (None, "(r, g, b) = "),
                        (Some("keyword"), "match "),
                        (None, "d."),
                        (Some("function"), "len"),
                        (None, "() {"),
                    ],
                    cx,
                ))
                .child(diff_line(
                    true,
                    &[
                        (None, "    "),
                        (Some("number"), "3"),
                        (None, " => "),
                        (Some("function"), "parse_short"),
                        (None, "(d)?,"),
                    ],
                    cx,
                )),
        );
    // "Done", as the turn's last card: the time, Review and Rollback, the files it changed.
    let duration = crate::claude_transcript::format_duration(Duration::from_secs(5));
    let done_card = div()
        .ml(px(AVATAR + 7.))
        .pb(px(5.))
        .flex()
        .flex_col()
        .rounded(px(8.))
        .border_1()
        .border_color(UiColors::tint(ui.foreground, 0.09))
        .bg(UiColors::tint(ui.foreground, 0.03))
        .whitespace_nowrap()
        .child(
            div()
                .h(px(28.))
                .px(px(9.))
                .flex()
                .items_center()
                .gap(px(6.))
                .child(icon(IconName::CheckCircle, ui.success).size(px(13.)))
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .child(trf("Done in {0}", &[&duration])),
                )
                .child(div().flex_1())
                .child(div().text_color(ui.accent_text).child(tr("Review")))
                .child(div().text_color(ui.accent_text).child(tr("Rollback"))),
        )
        .child(
            div()
                .px(px(10.))
                .pt(px(2.))
                .pb(px(2.))
                .child(ui::section_label(tr("Changed Files"), ui)),
        )
        .child(
            div()
                .h(px(20.))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(6.))
                .child(icons::file_icon("color.rs", &ui).render().size(px(11.)))
                .child(
                    div()
                        .font_family(theme::code_font())
                        .text_size(px(SCENE_TEXT - 0.5))
                        .child("src/color.rs"),
                )
                .child(div().text_color(ui.diff_added).child("+3"))
                .child(div().text_color(ui.error).child("−1")),
        );
    let feed = div()
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .p(px(10.))
        .flex()
        .flex_col()
        .gap(px(9.))
        .line_height(px(16.))
        .children(step(asked, bubble))
        .children(step(read, read_row))
        .children(step(edited, edit))
        .children(step(done, done_card));
    let composer = div()
        .mx(px(8.))
        .mb(px(8.))
        .h(px(32.))
        .flex_none()
        .px(px(10.))
        .flex()
        .items_center()
        .rounded(px(9.))
        .bg(opaque(ui.input_background))
        .border_1()
        .border_color(ui.input_border)
        .text_color(ui.dim)
        .whitespace_nowrap()
        .overflow_hidden()
        .child(tr("Ask Claude… @ for files, / for commands"));
    let content = div()
        .w(px(CHAT_WIDTH))
        .h_full()
        .flex()
        .flex_col()
        .child(tabs)
        .child(feed)
        .child(composer);
    scenes::island(cx)
        .flex_none()
        .h_full()
        .when(flat(cx), |island| {
            island.border_l_1().border_color(ui.island_border)
        })
        .child(content)
}

/// Claude's avatar in the small chat: its orange circle with the mark.
const AVATAR: f32 = 20.;

fn avatar(ui: UiColors) -> Div {
    let deeper = Hsla {
        l: (ui.claude.l - 0.08).max(0.),
        ..ui.claude
    };
    div()
        .size(px(AVATAR))
        .flex_none()
        .rounded(px(AVATAR / 2.))
        .flex()
        .items_center()
        .justify_center()
        .bg(linear_gradient(
            135.,
            linear_color_stop(ui.claude, 0.),
            linear_color_stop(deeper, 1.),
        ))
        .child(icon(IconName::Claude, ui.on_accent).size(px(11.)))
}

/// A line of the edit card's diff: added (`+`, the added background) or removed (`-`), its code in
/// the theme's colors.
fn diff_line(added: bool, pieces: CodeLine, cx: &App) -> Div {
    let theme = Theme::get(cx);
    let ui = theme.ui;
    let sign: SharedString = if added { "+ ".into() } else { "- ".into() };
    div()
        .px(px(8.))
        .flex()
        .bg(if added {
            ui.diff_added_bg
        } else {
            ui.diff_deleted_bg
        })
        .child(
            div()
                .flex_none()
                .text_color(if added { ui.diff_added } else { ui.error })
                .child(sign),
        )
        .children(pieces.iter().map(|(scope, text)| {
            let style = scope.and_then(|scope| theme.style_for(scope));
            div()
                .flex_none()
                .text_color(style.map_or(ui.foreground, |style| style.color))
                .child(text.to_string())
        }))
}

/// The pictures as functions of their show: every moment draws (no panics, no NaN sizes).
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_steps_of_a_show_are_in_order() {
        assert_eq!(linear(0., 0.28, 0.7), 0.);
        assert_eq!(linear(1., 0.28, 0.7), 1.);
        assert!((linear(0.49, 0.28, 0.7) - 0.5).abs() < 0.01);
        // The new commit comes after the message, which comes after the window opens.
        assert!(phase(0.27, 0., 0.26) > 0.99 && linear(0.27, 0.28, 0.7) == 0.);
        assert!(linear(0.71, 0.28, 0.7) == 1. && phase(0.79, 0.8, 1.) == 0.);
    }

    #[test]
    fn the_code_before_and_after_the_edit_differ_where_the_marks_are() {
        let text = |line: CodeLine| line.iter().map(|(_, text)| *text).collect::<String>();
        assert_eq!(text(COLOR_RS[0]), text(COLOR_RS_BEFORE[0]));
        assert_ne!(text(COLOR_RS[1]), text(COLOR_RS_BEFORE[1]));
        assert!(text(COLOR_RS[6]).contains("parse_short"));
    }
}
