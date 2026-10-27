//! The launchpad is the tool strip on the left of the window frame. An icon opens and closes the
//! window of its tool (an island or an overlay window); the open one is highlighted. A new tool is
//! a [`Tool`] variant with its icon, label, and action, plus an entry in [`Tool::ALL`]. Plugins'
//! tool windows (stage 8) come from the window's plugins and sit above Notifications: they live in
//! the same island on the right.
//!
//! Claude's icon is a toggle of its own ([`ToggleClaude`]): the console shows in the left island or,
//! when shown, goes, whatever has the focus; ⌘Esc, which its tooltip shows, goes by the focus
//! (part 10.1, «Пульт Claude»).

use gpui::{Action, Context, ElementId, IntoElement, actions, div, prelude::*, px};

use crate::i18n::tr;
use crate::icons::IconName;
use crate::motion::{self, TransitionExt};
use crate::plugins::{ToggleToolWindow, ToolWindow};
use crate::settings::UiStyle;
use crate::theme::{Theme, UiColors};
use crate::ui;
use crate::workspace::Workspace;
use crate::{file_tree, git, notifications_panel, project_search, terminal_panel};

actions!(
    launchpad,
    [
        /// The Claude icon: shows the console, or hides it when shown, whatever has the focus
        /// (⌘Esc, `claude::ToggleClaude`, goes by the focus). Not in the command palette.
        ToggleClaude,
    ]
);

/// Width of the strip; the buttons are centered.
pub const WIDTH: f32 = 40.;
/// A tool button is larger than a regular icon button: it is the window's main navigation.
const BUTTON_SIZE: f32 = 32.;
/// Flat: the space between the line under the title bar and the first button.
const FLAT_TOP_INSET: f32 = 4.;

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
    /// The Claude console: the left island, in place of the tree (part «Пульт Claude»).
    Claude,
    /// The Notifications window: the island on the right. At the bottom of the strip, apart from
    /// the tools of the left and the bottom.
    Notifications,
}

impl Tool {
    /// The tools at the bottom of the strip, above Settings: the windows of the right island.
    pub const BOTTOM: [Tool; 1] = [Tool::Notifications];

    /// Order in the strip, top to bottom.
    pub const ALL: [Tool; 6] = [
        Tool::Project,
        Tool::Commit,
        Tool::Claude,
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
            Tool::Claude => IconName::Claude,
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
            Tool::Claude => tr("Claude"),
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
            Tool::Claude => Box::new(ToggleClaude),
            Tool::Notifications => Box::new(notifications_panel::Toggle),
        }
    }

    /// The action whose keys the tooltip shows: the icon's own, except Claude's, whose icon has no
    /// keys — ⌘Esc brings Claude as well.
    fn keys_action(self) -> Box<dyn Action> {
        match self {
            Tool::Claude => Box::new(crate::claude::ToggleClaude),
            tool => tool.action(),
        }
    }
}

/// The tool strip: one button per tool; the open one gets an accent background and a marker on the
/// left.
pub fn render(workspace: &Workspace, cx: &Context<Workspace>) -> impl IntoElement + use<> {
    let ui = Theme::ui(cx);
    // Flat (part 10.4): a line divides the strip from the panels, and the first button keeps away
    // from the line under the title bar.
    let flat = ui::style(cx) == UiStyle::Flat;
    div()
        .flex_none()
        .w(px(WIDTH))
        .h_full()
        .flex()
        .flex_col()
        .items_center()
        .gap_1p5()
        .pb(px(ui::GAP))
        .when(flat, |strip| {
            strip
                .pt(px(FLAT_TOP_INSET))
                .border_r_1()
                .border_color(ui.island_border)
        })
        .children(
            Tool::ALL
                .into_iter()
                .filter(|tool| workspace.tool_visible(*tool, cx))
                .map(|tool| tool_button(workspace, tool, cx)),
        )
        // Settings: at the bottom of the strip, apart from the tools; above them, the windows of the
        // right island: the plugins', then Notifications. A plugin's window among the editor's tabs
        // has no icon here: it opens from the palette, its keys, the plugin.
        .child(div().flex_1())
        .children(
            workspace
                .plugins
                .read(cx)
                .tool_windows()
                .iter()
                .filter(|tool| !tool.in_editor())
                .map(|tool| plugin_tool_button(workspace, tool, cx))
                .collect::<Vec<_>>(),
        )
        .children(
            Tool::BOTTOM
                .into_iter()
                .filter(|tool| workspace.tool_visible(*tool, cx))
                .map(|tool| tool_button(workspace, tool, cx)),
        )
        .child({
            let open = crate::settings_view::is_open(workspace);
            let keys = ui::shortcut_for(&crate::settings_view::Toggle, cx);
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
    cx: &Context<Workspace>,
) -> impl IntoElement + use<> {
    let ui = Theme::ui(cx);
    let open = workspace.tool_open(tool, cx);
    let badge = workspace.tool_badge(tool, cx);
    let alarm = workspace.tool_badge_alarm(tool, cx);
    let dot = workspace.tool_dot(tool, cx);
    let action = tool.action();
    let keys = ui::shortcut_for(tool.keys_action().as_ref(), cx);
    div()
        .relative()
        .child(pad(
            ("tool-pad", tool as usize),
            open,
            ui,
            ui::toggle_button(("tool", tool as usize), tool.icon(), open, ui)
                .size(px(BUTTON_SIZE))
                .rounded(px(ui::RADIUS_MD))
                .tooltip(ui::tooltip(tool.label(), keys))
                .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx)),
        ))
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
        // A dot in the corner: Claude working or waiting for the user.
        .children(dot.filter(|_| badge.is_none()).map(|color| {
            div()
                .absolute()
                .top(px(2.))
                .right(px(2.))
                .size(px(8.))
                .rounded(px(4.))
                .bg(color)
                .border_1()
                .border_color(ui.frame)
        }))
        .child(open_marker(
            ("tool-marker", tool as usize),
            open,
            ui::style(cx) == UiStyle::Flat,
            ui,
        ))
}

/// A plugin's tool window: its icon from the manifest (a puzzle piece without one), its title and
/// keys in the tooltip.
fn plugin_tool_button(
    workspace: &Workspace,
    tool: &ToolWindow,
    cx: &Context<Workspace>,
) -> impl IntoElement + use<> {
    let ui = Theme::ui(cx);
    let open = workspace.plugin_tool_open(tool.key);
    let action = ToggleToolWindow {
        plugin: tool.plugin.clone(),
        window: tool.id.clone(),
    };
    let keys = ui::shortcut_for(&action, cx);
    let path = tool
        .icon
        .clone()
        .unwrap_or_else(|| IconName::Puzzle.path().into());
    div()
        .relative()
        .child(pad(
            ("plugin-tool-pad", tool.key.0 as usize),
            open,
            ui,
            ui::toggle_button_at(("plugin-tool", tool.key.0 as usize), path, open, ui)
                .size(px(BUTTON_SIZE))
                .rounded(px(ui::RADIUS_MD))
                .tooltip(ui::tooltip(tool.title.clone(), keys))
                .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx)),
        ))
        .child(open_marker(
            ("plugin-tool-marker", tool.key.0 as usize),
            open,
            ui::style(cx) == UiStyle::Flat,
            ui,
        ))
}

/// A tool button whose accent pad fades in as its window opens and out as it closes (`motion`).
fn pad(
    id: impl Into<ElementId>,
    open: bool,
    ui: UiColors,
    button: gpui::Stateful<gpui::Div>,
) -> impl IntoElement {
    let target = if open { 1. } else { 0. };
    button.with_transition(id, target, motion::FADE, move |button, shown| {
        button.bg(motion::mix(
            gpui::transparent_black(),
            ui.accent_soft,
            shown,
        ))
    })
}

/// Marker of the open window: at the edge of the strip, like an IDE tool tab. The strip of Islands
/// keeps 2 px from the window's edge; a Flat one starts at the edge and gives 1 px to its line. It
/// grows in with a bounce as the window opens and shrinks away as it closes (`motion`).
fn open_marker(id: impl Into<ElementId>, open: bool, flat: bool, ui: UiColors) -> impl IntoElement {
    let left = if flat {
        -(WIDTH - 1. - BUTTON_SIZE) / 2.
    } else {
        -(WIDTH - BUTTON_SIZE) / 2. - 1.
    };
    let (target, curve) = if open {
        (1., motion::BOUNCE)
    } else {
        (0., motion::EXIT)
    };
    div()
        .absolute()
        .left(px(left))
        .top(px(BUTTON_SIZE / 2. - 7.))
        .w(px(3.))
        .h(px(14.))
        .rounded(px(2.))
        .bg(ui.accent)
        .with_transition(id, target, curve, |marker, grown| {
            marker
                .opacity((grown * 3.).clamp(0., 1.))
                .transform_scale(grown.max(0.))
                .transform_origin(0.5, 0.5)
        })
}
