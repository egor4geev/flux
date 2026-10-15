//! The keys to remember (the tour's last slide, and Help → Keyboard Shortcuts): four groups — Find,
//! Code, Version Control, Windows — read from the real keymap, so they are the keys Flux answers
//! to; below them, the palette that shows every command with its keys.
//!
//! The commands are named as the palette and the launchpad name them, so a key learned here is
//! found under the same name there. A command without a key (a keymap that lost it) isn't listed.

use gpui::{Action, AnyElement, App, Context, Div, FontWeight, Window, div, prelude::*, px};

use super::{WelcomeGuide, heading};
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::picker::highlighted_text;
use crate::theme::{self, Theme, UiColors};
use crate::ui;

/// The column of a row's keys: the widest (⇧⌘F, ⌘Esc) fit, the labels start on one line.
const KEYS_WIDTH: f32 = 62.;
const ROW_HEIGHT: f32 = 30.;
/// The gap between the keys and the label.
const KEYS_GAP: f32 = 10.;
/// The palette in the picture under the groups.
const PALETTE_WIDTH: f32 = 540.;
const PALETTE_ROW: f32 = 30.;

/// A group of keys: its title, the width of its column, its commands (the action, as the palette
/// names it).
struct Group {
    title: &'static str,
    width: f32,
    keys: Vec<(Box<dyn Action>, &'static str)>,
}

/// The groups, in the order of the tour: find, the code, version control, the windows. The widths
/// make room for the longest Russian label of each column, and the four fit side by side at 100 %.
fn groups() -> Vec<Group> {
    vec![
        Group {
            title: "Find",
            width: 232.,
            keys: vec![
                (Box::new(crate::file_finder::Toggle), "Find File"),
                (Box::new(crate::project_search::Toggle), "Find in Files"),
                (Box::new(crate::command_palette::Toggle), "Command Palette"),
                (Box::new(crate::quick_switch::Toggle), "Quick Switch"),
            ],
        },
        Group {
            title: "Code",
            width: 232.,
            keys: vec![
                (
                    Box::new(crate::navigation::GoToDefinition),
                    "Go To Definition",
                ),
                (Box::new(crate::navigation::FindUsages), "Find Usages"),
                (Box::new(crate::navigation::RenameSymbol), "Rename"),
                (
                    Box::new(crate::code_actions::ShowContextActions),
                    "Context Actions",
                ),
            ],
        },
        Group {
            title: "Version Control",
            width: 190.,
            keys: vec![
                (Box::new(crate::git::Commit), "Commit"),
                (Box::new(crate::git::Push), "Push"),
                (Box::new(crate::git::UpdateProject), "Update Project"),
                (Box::new(crate::git::Branches), "Branches"),
            ],
        },
        Group {
            title: "Windows",
            width: 190.,
            keys: vec![
                (Box::new(crate::file_tree::ToggleOpen), "Project"),
                (Box::new(crate::terminal_panel::TogglePanel), "Terminal"),
                (Box::new(crate::git::ToggleGitWindow), "Git"),
                (Box::new(crate::claude::ToggleClaude), "Claude Code"),
            ],
        },
    ]
}

pub fn render(
    _guide: &mut WelcomeGuide,
    _window: &mut Window,
    cx: &mut Context<WelcomeGuide>,
) -> AnyElement {
    let ui = Theme::ui(cx);
    let palette = ui::shortcut_for(&crate::command_palette::Toggle, cx)
        .map(|keys| keys.to_string())
        .unwrap_or_default();
    let lead = trf(
        "Everything else is in the command palette ({0}): every command there shows its keys. This page opens again from Help → Keyboard Shortcuts.",
        &[&palette],
    );
    let groups = div()
        .mt(px(26.))
        .flex()
        .flex_wrap()
        .gap_x(px(16.))
        .gap_y(px(18.))
        .children(
            groups()
                .into_iter()
                .map(|group| render_group(group, ui, cx)),
        );
    div()
        .id("welcome-shortcuts")
        .size_full()
        .overflow_y_scroll()
        .px(px(36.))
        .pt(px(28.))
        .pb(px(24.))
        .flex()
        .flex_col()
        .child(heading(
            tr("Tour · Keys"),
            tr("Keys Worth Remembering"),
            lead,
            ui,
        ))
        .child(groups)
        .child(div().flex_1().min_h(px(24.)))
        .child(div().flex().justify_center().child(palette_picture(ui, cx)))
        .into_any_element()
}

/// A group: its title over the labels, then a row per command that has keys — the keys right
/// aligned in their column, the command's name.
fn render_group(group: Group, ui: UiColors, cx: &App) -> Div {
    let rows = group.keys.into_iter().filter_map(|(action, label)| {
        let keys = ui::shortcut_for(action.as_ref(), cx)?;
        Some(
            div()
                .h(px(ROW_HEIGHT))
                .flex()
                .items_center()
                .gap(px(KEYS_GAP))
                .child(
                    div()
                        .w(px(KEYS_WIDTH))
                        .flex_none()
                        .flex()
                        .justify_end()
                        .child(ui::keys(&keys, ui)),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_size(px(theme::TEXT_MD))
                        .whitespace_nowrap()
                        .child(tr(label)),
                ),
        )
    });
    div()
        .w(px(group.width))
        .flex()
        .flex_col()
        .child(
            div()
                .pl(px(KEYS_WIDTH + KEYS_GAP))
                .pb(px(6.))
                .child(ui::section_label(tr(group.title), ui)),
        )
        .children(rows)
}

/// The palette as a picture: "comm" typed, three commands found — their section, their name with
/// the letters that matched, their keys — the first selected. Drawn as the palette draws itself, so
/// the slide shows where every other key is.
fn palette_picture(ui: UiColors, cx: &App) -> Div {
    let query = tr("comm");
    let commands: [(Box<dyn Action>, &str); 3] = [
        (Box::new(crate::git::Commit), "Commit"),
        (
            Box::new(crate::commit_panel::CommitAndPush),
            "Commit And Push",
        ),
        (
            Box::new(crate::git::ToggleCommitWindow),
            "Toggle Commit Window",
        ),
    ];
    let rows = commands
        .into_iter()
        .enumerate()
        .map(|(index, (action, title))| {
            let title = tr(title);
            let positions = match_positions(title, query);
            let keys = ui::shortcut_for(action.as_ref(), cx);
            div()
                .h(px(PALETTE_ROW))
                .px(px(10.))
                .flex()
                .items_center()
                .gap_3()
                .rounded(px(ui::RADIUS_MD))
                .when(index == 0, |row| row.bg(ui.list_selected))
                .child(
                    div()
                        .flex_none()
                        .h(px(20.))
                        .px_1p5()
                        .flex()
                        .items_center()
                        .rounded(px(ui::RADIUS_SM))
                        .bg(UiColors::tint(ui.indigo, 0.15))
                        .text_size(px(theme::TEXT_XS))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(ui.indigo)
                        .child(tr("Git")),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(highlighted_text(title, &positions, ui.match_text)),
                )
                .children(keys.map(|keys| ui::keys(&keys, ui)))
        });
    let field = div()
        .h(px(34.))
        .px(px(10.))
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(ui::RADIUS_MD))
        .bg(ui.input_background)
        .border_1()
        .border_color(ui.focus_border)
        .child(icon(IconName::Command, ui.text_muted).size(px(14.)))
        .child(query)
        .child(div().ml(px(-6.)).w(px(1.5)).h(px(15.)).bg(ui.accent_text));
    ui::popover(ui)
        .w(px(PALETTE_WIDTH))
        .flex_none()
        .p_2()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(field)
        .child(div().flex().flex_col().gap_0p5().children(rows))
}

/// Where `query` is in `text`, as the palette marks a match: the characters of its first
/// occurrence, case aside (the picture's query is a plain fragment of the names).
fn match_positions(text: &str, query: &str) -> Vec<usize> {
    let lower = |text: &str| -> Vec<char> {
        text.chars()
            .map(|c| c.to_lowercase().next().unwrap_or(c))
            .collect()
    };
    let (text, query) = (lower(text), lower(query));
    if query.is_empty() || query.len() > text.len() {
        return Vec::new();
    }
    (0..=text.len() - query.len())
        .find(|start| text[*start..*start + query.len()] == query[..])
        .map(|start| (start..start + query.len()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_match_marks_the_fragment_wherever_it_is() {
        assert_eq!(match_positions("Commit", "comm"), vec![0, 1, 2, 3]);
        assert_eq!(
            match_positions("Toggle Commit Window", "comm"),
            vec![7, 8, 9, 10]
        );
        assert_eq!(match_positions("Окно коммита", "комм"), vec![5, 6, 7, 8]);
        assert_eq!(match_positions("Push", "comm"), Vec::<usize>::new());
        assert_eq!(match_positions("Co", "comm"), Vec::<usize>::new());
    }

    #[test]
    fn every_group_has_four_commands() {
        for group in groups() {
            assert_eq!(group.keys.len(), 4, "{}", group.title);
        }
    }
}
