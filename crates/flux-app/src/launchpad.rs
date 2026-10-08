//! The launchpad is the tool strip on the left of the window frame. An icon opens and closes the
//! window of its tool (an island or an overlay window); the open one is highlighted. A new tool is
//! a [`Tool`] variant with its icon, label, and action, plus an entry in [`Tool::ALL`].

use gpui::{Action, Context, IntoElement, Window, div, prelude::*, px};

use crate::i18n::tr;
use crate::icons::IconName;
use crate::theme::Theme;
use crate::ui;
use crate::workspace::Workspace;
use crate::{file_tree, project_search};

/// Width of the strip; the buttons are centered.
pub const WIDTH: f32 = 40.;
/// A tool button is larger than a regular icon button: it is the window's main navigation.
const BUTTON_SIZE: f32 = 32.;

/// A launchpad tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Project tree: an island on the left.
    Project,
    /// Project search: the "Find in Files" window over the islands.
    FindInFiles,
}

impl Tool {
    /// Order in the strip, top to bottom.
    pub const ALL: [Tool; 2] = [Tool::Project, Tool::FindInFiles];

    fn icon(self) -> IconName {
        match self {
            Tool::Project => IconName::Project,
            Tool::FindInFiles => IconName::FindInFiles,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tool::Project => tr("Project"),
            Tool::FindInFiles => tr("Find in Files"),
        }
    }

    /// The action that opens and closes the tool's window.
    fn action(self) -> Box<dyn Action> {
        match self {
            Tool::Project => Box::new(file_tree::ToggleOpen),
            Tool::FindInFiles => Box::new(project_search::Toggle),
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
        .children(Tool::ALL.into_iter().map(|tool| {
            let open = workspace.tool_open(tool, cx);
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
}
