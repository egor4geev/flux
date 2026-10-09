//! The launchpad is the tool strip on the left of the window frame. An icon opens and closes the
//! window of its tool (an island or an overlay window); the open one is highlighted. A new tool is
//! a [`Tool`] variant with its icon, label, and action, plus an entry in [`Tool::ALL`].

use gpui::{Action, Context, IntoElement, Window, div, prelude::*, px};

use crate::i18n::tr;
use crate::icons::IconName;
use crate::theme::Theme;
use crate::ui;
use crate::workspace::Workspace;
use crate::{file_tree, git, project_search, terminal_panel};

/// Width of the strip; the buttons are centered.
pub const WIDTH: f32 = 40.;
/// A tool button is larger than a regular icon button: it is the window's main navigation.
const BUTTON_SIZE: f32 = 32.;

/// A launchpad tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Project tree: an island on the left.
    Project,
    /// The commit window: the left island, in place of the tree.
    Commit,
    /// Project search: the "Find in Files" window over the islands.
    FindInFiles,
    /// Terminals: an island under the editor.
    Terminal,
    /// The Git window (the log): the island under the editor, in place of the terminals.
    Git,
}

impl Tool {
    /// Order in the strip, top to bottom.
    pub const ALL: [Tool; 5] = [
        Tool::Project,
        Tool::Commit,
        Tool::FindInFiles,
        Tool::Terminal,
        Tool::Git,
    ];

    fn icon(self) -> IconName {
        match self {
            Tool::Project => IconName::Project,
            Tool::Commit => IconName::Commit,
            Tool::FindInFiles => IconName::FindInFiles,
            Tool::Terminal => IconName::Terminal,
            Tool::Git => IconName::GitLog,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tool::Project => tr("Project"),
            Tool::Commit => tr("Commit"),
            Tool::FindInFiles => tr("Find in Files"),
            Tool::Terminal => tr("Terminal"),
            Tool::Git => tr("Git"),
        }
    }

    /// The action that opens and closes the tool's window.
    fn action(self) -> Box<dyn Action> {
        match self {
            Tool::Project => Box::new(file_tree::ToggleOpen),
            Tool::Commit => Box::new(git::ToggleCommitWindow),
            Tool::FindInFiles => Box::new(project_search::Toggle),
            Tool::Terminal => Box::new(terminal_panel::TogglePanel),
            Tool::Git => Box::new(git::ToggleGitWindow),
        }
    }
}

/// The tool strip: one button per tool; the open one gets an accent background and a marker on the
/// left.
pub fn render(
    workspace: &Workspace,
    window: &Window,
    cx: &Context<Workspace>,
) -> impl IntoElement + use<> {
    let ui = Theme::ui(cx);
    div()
        .flex_none()
        .w(px(WIDTH))
        .h_full()
        .flex()
        .flex_col()
        .items_center()
        .gap_1p5()
        .pb(px(ui::GAP))
        .children(Tool::ALL.into_iter().map(|tool| {
            let open = workspace.tool_open(tool, cx);
            let badge = workspace.tool_badge(tool, cx);
            let action = tool.action();
            let keys = ui::shortcut_for(action.as_ref(), window);
            div()
                .relative()
                .child(
                    ui::toggle_button(("tool", tool as usize), tool.icon(), open, ui)
                        .size(px(BUTTON_SIZE))
                        .rounded(px(ui::RADIUS_MD))
                        .tooltip(ui::tooltip(tool.label(), keys))
                        .on_click(move |_, window, cx| {
                            window.dispatch_action(action.boxed_clone(), cx)
                        }),
                )
                // A count in the corner: the changes waiting for a commit.
                .children(badge.map(|count| {
                    div()
                        .absolute()
                        .top(px(-3.))
                        .right(px(-5.))
                        .min_w(px(15.))
                        .h(px(15.))
                        .px(px(3.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(8.))
                        .bg(ui.accent)
                        .text_size(px(crate::theme::TEXT_XS - 2.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(ui.frame)
                        .child(if count > 99 {
                            "99+".to_string()
                        } else {
                            count.to_string()
                        })
                }))
                // Marker of the open window: at the edge of the strip, like an IDE tool tab.
                .when(open, |button| {
                    button.child(
                        div()
                            .absolute()
                            .left(px(-(WIDTH - BUTTON_SIZE) / 2. - 1.))
                            .top(px(BUTTON_SIZE / 2. - 7.))
                            .w(px(3.))
                            .h(px(14.))
                            .rounded(px(2.))
                            .bg(ui.accent),
                    )
                })
        }))
        // Settings: at the bottom of the strip, apart from the tools.
        .child(div().flex_1())
        .child({
            let open = crate::settings_view::is_open(workspace);
            let keys = ui::shortcut_for(&crate::settings_view::Toggle, window);
            ui::toggle_button("settings", IconName::Settings, open, ui)
                .size(px(BUTTON_SIZE))
                .rounded(px(ui::RADIUS_MD))
                .tooltip(ui::tooltip(tr("Settings"), keys))
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(crate::settings_view::Toggle), cx)
                })
        })
}
