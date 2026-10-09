//! The launchpad is the tool strip on the left of the window frame. An icon opens and closes the
//! window of its tool (an island or an overlay window); the open one is highlighted. A new tool is
//! a [`Tool`] variant with its icon, label, and action, plus an entry in [`Tool::ALL`]. Plugins'
//! tool windows (stage 8) come from the window's plugins and sit above Notifications: they live in
//! the same island on the right.

use gpui::{Action, Context, IntoElement, Window, div, prelude::*, px};

use crate::i18n::tr;
use crate::icons::IconName;
use crate::plugins::{ToggleToolWindow, ToolWindow};
use crate::theme::{Theme, UiColors};
use crate::ui;
use crate::workspace::Workspace;
use crate::{file_tree, git, notifications_panel, project_search, terminal_panel};

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
    /// The Notifications window: the island on the right. At the bottom of the strip, apart from
    /// the tools of the left and the bottom.
    Notifications,
}

impl Tool {
    /// The tools at the bottom of the strip, above Settings: the windows of the right island.
    pub const BOTTOM: [Tool; 1] = [Tool::Notifications];

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
            Tool::Notifications => IconName::Bell,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tool::Project => tr("Project"),
            Tool::Commit => tr("Commit"),
            Tool::FindInFiles => tr("Find in Files"),
            Tool::Terminal => tr("Terminal"),
            Tool::Git => tr("Git"),
            Tool::Notifications => tr("Notifications"),
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
            Tool::Notifications => Box::new(notifications_panel::Toggle),
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
        .children(
            Tool::ALL
                .into_iter()
                .map(|tool| tool_button(workspace, tool, window, cx)),
        )
        // Settings: at the bottom of the strip, apart from the tools; above them, the windows of the
        // right island: the plugins', then Notifications.
        .child(div().flex_1())
        .children(
            workspace
                .plugins
                .read(cx)
                .tool_windows()
                .iter()
                .map(|tool| plugin_tool_button(workspace, tool, window, cx))
                .collect::<Vec<_>>(),
        )
        .children(
            Tool::BOTTOM
                .into_iter()
                .map(|tool| tool_button(workspace, tool, window, cx)),
        )
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

/// A tool button: the icon, highlighted when the tool's window is open, a count in the corner.
fn tool_button(
    workspace: &Workspace,
    tool: Tool,
    window: &Window,
    cx: &Context<Workspace>,
) -> impl IntoElement + use<> {
    let ui = Theme::ui(cx);
    let open = workspace.tool_open(tool, cx);
    let badge = workspace.tool_badge(tool, cx);
    let alarm = workspace.tool_badge_alarm(tool, cx);
    let action = tool.action();
    let keys = ui::shortcut_for(action.as_ref(), window);
    div()
        .relative()
        .child(
            ui::toggle_button(("tool", tool as usize), tool.icon(), open, ui)
                .size(px(BUTTON_SIZE))
                .rounded(px(ui::RADIUS_MD))
                .tooltip(ui::tooltip(tool.label(), keys))
                .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx)),
        )
        // A count in the corner: changes waiting for a commit, unread notifications.
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
                .bg(if alarm { ui.error } else { ui.accent })
                .text_size(px(crate::theme::TEXT_XS - 2.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(ui.frame)
                .child(if count > 99 {
                    "99+".to_string()
                } else {
                    count.to_string()
                })
        }))
        .when(open, |button| button.child(open_marker(ui)))
}

/// A plugin's tool window: its icon from the manifest (a puzzle piece without one), its title and
/// keys in the tooltip.
fn plugin_tool_button(
    workspace: &Workspace,
    tool: &ToolWindow,
    window: &Window,
    cx: &Context<Workspace>,
) -> impl IntoElement + use<> {
    let ui = Theme::ui(cx);
    let open = workspace.plugin_tool_open(tool.key);
    let action = ToggleToolWindow {
        plugin: tool.plugin.clone(),
        window: tool.id.clone(),
    };
    let keys = ui::shortcut_for(&action, window);
    let path = tool
        .icon
        .clone()
        .unwrap_or_else(|| IconName::Puzzle.path().into());
    div()
        .relative()
        .child(
            ui::toggle_button_at(("plugin-tool", tool.key.0 as usize), path, open, ui)
                .size(px(BUTTON_SIZE))
                .rounded(px(ui::RADIUS_MD))
                .tooltip(ui::tooltip(tool.title.clone(), keys))
                .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx)),
        )
        .when(open, |button| button.child(open_marker(ui)))
}

/// Marker of the open window: at the edge of the strip, like an IDE tool tab.
fn open_marker(ui: UiColors) -> impl IntoElement {
    div()
        .absolute()
        .left(px(-(WIDTH - BUTTON_SIZE) / 2. - 1.))
        .top(px(BUTTON_SIZE / 2. - 7.))
        .w(px(3.))
        .h(px(14.))
        .rounded(px(2.))
        .bg(ui.accent)
}
