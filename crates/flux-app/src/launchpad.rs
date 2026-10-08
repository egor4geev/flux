//! Лаунчпад — полоса инструментов слева на рамке окна. Значок открывает и закрывает окно
//! своего инструмента (остров или окно поверх); открытое — подсвечено. Новый инструмент —
//! вариант [`Tool`] с его значком, подписью и действием и строка в [`Tool::ALL`].

use gpui::{Action, Context, IntoElement, Window, div, prelude::*, px};

use crate::icons::IconName;
use crate::theme::Theme;
use crate::ui;
use crate::workspace::Workspace;
use crate::{file_tree, project_search};

/// Ширина полосы; кнопки — по центру.
pub const WIDTH: f32 = 40.;
/// Кнопка инструмента крупнее обычной кнопки-значка: это главная навигация окна.
const BUTTON_SIZE: f32 = 32.;

/// Инструмент лаунчпада.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Дерево проекта — остров слева.
    Project,
    /// Поиск по проекту — окно «Find in Files» поверх островов.
    FindInFiles,
}

impl Tool {
    /// Порядок в полосе сверху вниз.
    pub const ALL: [Tool; 2] = [Tool::Project, Tool::FindInFiles];

    fn icon(self) -> IconName {
        match self {
            Tool::Project => IconName::Project,
            Tool::FindInFiles => IconName::FindInFiles,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tool::Project => "Project",
            Tool::FindInFiles => "Find in Files",
        }
    }

    /// Действие, которое открывает и закрывает окно инструмента.
    fn action(self) -> Box<dyn Action> {
        match self {
            Tool::Project => Box::new(file_tree::ToggleOpen),
            Tool::FindInFiles => Box::new(project_search::Toggle),
        }
    }
}

/// Полоса инструментов: кнопка на инструмент, у открытого — акцентная подложка и метка слева.
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
                // Метка открытого окна — у края полосы, как у вкладки инструмента в IDE.
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
