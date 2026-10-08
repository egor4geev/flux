//! Начальный экран — в окне без открытых документов: логотип, текущий проект, быстрые
//! действия с сочетаниями и недавние проекты.
//!
//! Действия отправляются в окно (`window.dispatch_action`) — те же, что у клавиш; недавний
//! проект открывается действием [`OpenProject`]. Состояния у экрана нет: всё, что он
//! показывает, передаёт Workspace ([`StartScreen`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{
    Action, AnyElement, BoxShadow, Context, Div, FontWeight, Hsla, SharedString, Window, div, img,
    linear_color_stop, linear_gradient, point, prelude::*, px,
};

use crate::icons::{self, IconName, icon};
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_LG, RADIUS_MD};
use crate::workspace::{self, OpenProject, Workspace, tilde};
use crate::{command_palette, file_finder, file_tree, project_search};

/// Ширина содержимого по центру острова; уже — во всю ширину с полями.
const CONTENT_WIDTH: f32 = 720.;
/// Столбцы «Start» и «Recent Projects» стоят рядом, пока каждому хватает этой ширины,
/// иначе встают друг под друга.
const COLUMN_WIDTH: f32 = 300.;
/// Сколько недавних проектов показывать.
const RECENT_SHOWN: usize = 6;
const ACTION_ROW_HEIGHT: f32 = 36.;
const RECENT_ROW_HEIGHT: f32 = 44.;
/// Плитка логотипа и плитки значков действий.
const LOGO_SIZE: f32 = 76.;
const TILE_SIZE: f32 = 26.;
/// Пути длиннее сокращаются посередине («~/…/flux-dev/flux»): многоточие gpui в строках
/// с гибкой шириной не срабатывает, а конец пути важнее начала.
const CARD_PATH_CHARS: usize = 64;
const RECENT_PATH_CHARS: usize = 44;

/// Что знает о себе окно для начального экрана.
pub struct StartScreen<'a> {
    pub root: Option<&'a Path>,
    /// Ветка git проекта.
    pub branch: Option<&'a str>,
    /// Недавние проекты, последние — первыми (текущий тоже среди них).
    pub recent: &'a [PathBuf],
    /// Сообщение окна: ошибки открытия файлов, «Project: ~/…».
    pub notice: Option<&'a SharedString>,
    /// Файлы из командной строки ещё читаются — экран не показываем.
    pub loading: bool,
}

/// Быстрое действие: подпись, значок, оттенок плитки и действие окна.
struct QuickAction {
    id: &'static str,
    label: &'static str,
    icon: IconName,
    hue: Hsla,
    action: Box<dyn Action>,
    /// Без корня проекта действие бессмысленно (поиск, дерево) — не показывается.
    needs_project: bool,
}

fn quick_actions(ui: &UiColors) -> Vec<QuickAction> {
    let action = |id, label, icon, hue, action: &dyn Action, needs_project| QuickAction {
        id,
        label,
        icon,
        hue,
        action: action.boxed_clone(),
        needs_project,
    };
    vec![
        action(
            "find-file",
            "Find File",
            IconName::Search,
            ui.blue,
            &file_finder::Toggle,
            true,
        ),
        action(
            "find-in-files",
            "Find in Files",
            IconName::FindInFiles,
            ui.amber,
            &project_search::Toggle,
            true,
        ),
        action(
            "command-palette",
            "Command Palette",
            IconName::Command,
            ui.violet,
            &command_palette::Toggle,
            false,
        ),
        action(
            "new-file",
            "New File",
            IconName::FilePlus,
            ui.green,
            &workspace::NewFile,
            false,
        ),
        action(
            "open",
            "Open…",
            IconName::FolderOpen,
            ui.cyan,
            &workspace::Open,
            false,
        ),
        action(
            "toggle-tree",
            "Toggle Project Tree",
            IconName::Sidebar,
            ui.pink,
            &file_tree::ToggleOpen,
            true,
        ),
    ]
}

pub fn render(screen: StartScreen, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let ui = Theme::ui(cx);
    if screen.loading {
        return div().size_full().into_any_element();
    }
    let actions: Vec<QuickAction> = quick_actions(&ui)
        .into_iter()
        .filter(|action| screen.root.is_some() || !action.needs_project)
        .collect();
    let shortcuts: Vec<Option<SharedString>> = actions
        .iter()
        .map(|action| ui::shortcut_for(action.action.as_ref(), window))
        .collect();
    request_frame_for_shortcuts(&shortcuts, window);

    let action_rows = actions
        .into_iter()
        .zip(shortcuts)
        .map(|(action, keys)| action_row(action, keys, ui).into_any_element());
    let recent: Vec<AnyElement> = screen
        .recent
        .iter()
        .take(RECENT_SHOWN)
        .enumerate()
        .map(|(index, path)| {
            let current = screen.root == Some(path.as_path());
            recent_row(index, path, current, ui).into_any_element()
        })
        .collect();
    let recent = if recent.is_empty() {
        vec![empty_recent(ui).into_any_element()]
    } else {
        recent
    };

    let content = div()
        .w_full()
        .max_w(px(CONTENT_WIDTH))
        .flex()
        .flex_col()
        .gap_7()
        .child(hero(ui))
        .children(screen.notice.map(|notice| notice_banner(notice, ui)))
        .child(match screen.root {
            Some(root) => project_card(root, screen.branch, ui).into_any_element(),
            None => open_folder_card(window, ui).into_any_element(),
        })
        .child(
            div()
                .w_full()
                .flex()
                .flex_wrap()
                .gap_x_8()
                .gap_y_6()
                .child(column("Start", action_rows, ui))
                .child(column("Recent Projects", recent, ui)),
        );
    // Содержимое — по центру; не помещается по высоте — прокручивается (поля `my_auto`
    // сжимаются до нуля).
    div()
        .id("start-screen")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .items_center()
        .px_10()
        .py_10()
        .font_family(theme::UI_FONT)
        .child(content.my_auto())
        .into_any_element()
}

/// Сочетания берутся из дерева прошлого кадра: в самом первом кадре окна их ещё нет.
/// Тогда — один лишний кадр, чтобы клавиши появились, не дожидаясь движения мыши.
fn request_frame_for_shortcuts(shortcuts: &[Option<SharedString>], window: &mut Window) {
    static REQUESTED: AtomicBool = AtomicBool::new(false);
    if shortcuts.iter().all(Option::is_none) && !REQUESTED.swap(true, Ordering::Relaxed) {
        window.request_animation_frame();
    }
}

/// Логотип на градиенте акцента, название и версия.
fn hero(ui: UiColors) -> Div {
    // Плитка как у иконки приложения: тёмное стекло, блик, цветной логотип и мягкое свечение.
    let logo = div()
        .relative()
        .flex_none()
        .size(px(LOGO_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(LOGO_SIZE * 0.25))
        .bg(linear_gradient(
            180.,
            linear_color_stop(ui.elevated, 0.),
            linear_color_stop(UiColors::tint(ui.island, 1.), 1.),
        ))
        .border_1()
        .border_color(ui.elevated_border)
        .shadow(vec![
            BoxShadow {
                color: UiColors::tint(ui.accent, 0.30),
                offset: point(px(0.), px(10.)),
                blur_radius: px(32.),
                spread_radius: px(-6.),
            },
            BoxShadow {
                color: UiColors::tint(ui.shadow, 0.3),
                offset: point(px(0.), px(2.)),
                blur_radius: px(6.),
                spread_radius: px(0.),
            },
        ])
        .child(ui::sheen(ui, LOGO_SIZE * 0.25))
        .child(img(icons::LOGO).size(px(LOGO_SIZE * 0.68)));
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_4()
        .child(logo)
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_size(px(theme::TEXT_DISPLAY))
                                .font_weight(FontWeight::BOLD)
                                .line_height(px(theme::TEXT_DISPLAY + 6.))
                                .text_color(ui.foreground)
                                .child("flux"),
                        )
                        .child(ui::badge(
                            concat!("v", env!("CARGO_PKG_VERSION")),
                            ui.accent_text,
                        )),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_MD))
                        .text_color(ui.text_muted)
                        .child("A fast, minimal code editor"),
                ),
        )
}

/// Подложка карточек: чуть светлее острова.
fn card(ui: UiColors) -> Div {
    div()
        .w_full()
        .rounded(px(RADIUS_LG))
        .bg(UiColors::tint(ui.foreground, 0.03))
        .border_1()
        .border_color(ui.island_border)
}

/// Текущий проект: значок, имя, путь и ветка.
fn project_card(root: &Path, branch: Option<&str>, ui: UiColors) -> Div {
    card(ui)
        .flex()
        .items_center()
        .gap_3()
        .p_3()
        .child(
            div()
                .flex_none()
                .size(px(40.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(10.))
                .bg(UiColors::tint(ui.folder, 0.14))
                .child(icon(IconName::Folder, ui.folder).size(px(20.))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .truncate()
                        .text_size(px(theme::TEXT_LG))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(ui.foreground)
                        .child(display_name(root)),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.text_muted)
                        .child(shorten_path(&tilde(root), CARD_PATH_CHARS)),
                ),
        )
        .children(branch.map(|branch| branch_chip(branch, ui)))
}

/// Ветка git: фиолетовый чип, как в шапке окна.
fn branch_chip(branch: &str, ui: UiColors) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .h(px(24.))
        .px_2()
        .rounded(px(12.))
        .bg(UiColors::tint(ui.violet, 0.12))
        .text_size(px(theme::TEXT_SM))
        .text_color(ui.violet)
        .child(icon(IconName::Branch, ui.violet).size(px(12.)))
        .child(branch.to_string())
}

/// Без проекта: та же карточка, что у проекта, — приглашение и заметная основная кнопка.
fn open_folder_card(window: &Window, ui: UiColors) -> Div {
    let keys = ui::shortcut_for(&workspace::Open, window);
    // Тёмный текст на светлом акценте читается лучше белого.
    let on_accent = UiColors::tint(ui.island, 1.);
    card(ui)
        .flex()
        .items_center()
        .gap_3()
        .p_3()
        .child(
            div()
                .flex_none()
                .size(px(40.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(10.))
                .bg(UiColors::tint(ui.folder, 0.14))
                .child(icon(IconName::FolderOpen, ui.folder).size(px(20.))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .truncate()
                        .text_size(px(theme::TEXT_LG))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(ui.foreground)
                        .child("No project open"),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.text_muted)
                        .child("Open a folder to browse, search and keep it in recent projects"),
                ),
        )
        .child(
            div()
                .id("open-folder")
                .flex_none()
                .h(px(32.))
                .px_3p5()
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(RADIUS_MD))
                .bg(linear_gradient(
                    135.,
                    linear_color_stop(ui.accent, 0.),
                    linear_color_stop(ui.violet, 1.),
                ))
                .shadow(vec![BoxShadow {
                    color: UiColors::tint(ui.accent, 0.3),
                    offset: point(px(0.), px(4.)),
                    blur_radius: px(14.),
                    spread_radius: px(-4.),
                }])
                .cursor_pointer()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(on_accent)
                .hover(|style| style.opacity(0.9))
                .active(|style| style.opacity(0.8))
                .tooltip(ui::tooltip("Open a folder or files", keys))
                .on_click(|_, window, cx| window.dispatch_action(workspace::Open.boxed_clone(), cx))
                .child(icon(IconName::FolderOpen, on_accent).size(px(14.)))
                .child("Open Folder…"),
        )
}

/// Столбец с подписью раздела.
fn column(title: &'static str, rows: impl IntoIterator<Item = AnyElement>, ui: UiColors) -> Div {
    div()
        .flex_grow()
        .flex_shrink()
        .flex_basis(px(COLUMN_WIDTH))
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(ui::section_label(title, ui).px_2().pb_1())
        .children(rows)
}

/// Строка быстрого действия: плитка значка, подпись, клавиши.
fn action_row(action: QuickAction, keys: Option<SharedString>, ui: UiColors) -> impl IntoElement {
    let QuickAction {
        id,
        label,
        icon: name,
        hue,
        action,
        ..
    } = action;
    div()
        .id(id)
        .h(px(ACTION_ROW_HEIGHT))
        .px_2()
        .flex()
        .items_center()
        .gap_3()
        .rounded(px(RADIUS_MD))
        .cursor_pointer()
        .hover(move |style| style.bg(ui.hover))
        .active(move |style| style.bg(ui.pressed))
        .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
        .child(tile(name, hue))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(ui.foreground)
                .child(label),
        )
        .children(keys.map(|keys| ui::keys(&keys, ui)))
}

/// Цветная плитка со значком: подложка и значок одного оттенка.
fn tile(name: IconName, hue: Hsla) -> Div {
    div()
        .flex_none()
        .size(px(TILE_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .bg(UiColors::tint(hue, 0.15))
        .child(icon(name, hue).size(px(14.)))
}

/// Недавний проект: имя и путь; текущий — с бейджем и без щелчка.
fn recent_row(index: usize, path: &Path, current: bool, ui: UiColors) -> impl IntoElement {
    let row = div()
        .id(("recent-project", index))
        .h(px(RECENT_ROW_HEIGHT))
        .px_2()
        .flex()
        .items_center()
        .gap_3()
        .rounded(px(RADIUS_MD))
        .child(tile(IconName::Folder, ui.folder))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(ui.foreground)
                        .child(display_name(path)),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .child(shorten_path(&tilde(path), RECENT_PATH_CHARS)),
                ),
        );
    if current {
        return row.child(ui::badge("current", ui.accent_text));
    }
    let path = path.to_path_buf();
    row.cursor_pointer()
        .hover(move |style| style.bg(ui.hover))
        .active(move |style| style.bg(ui.pressed))
        .on_click(move |_, window, cx| {
            window.dispatch_action(Box::new(OpenProject(path.clone())), cx)
        })
}

fn empty_recent(ui: UiColors) -> impl IntoElement {
    div()
        .px_2()
        .py_2()
        .flex()
        .items_center()
        .gap_2()
        .text_color(ui.dim)
        .child(icon(IconName::Clock, ui.dim).size(px(14.)))
        .child("Projects you open will appear here")
}

/// Сообщение окна: ошибка — красной плашкой, остальное — информационной.
fn notice_banner(notice: &SharedString, ui: UiColors) -> Div {
    let (name, color) = if looks_like_error(notice) {
        (IconName::Error, ui.error)
    } else {
        (IconName::Info, ui.info)
    };
    div()
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_2()
        .rounded(px(RADIUS_MD))
        .bg(UiColors::tint(color, 0.10))
        .border_1()
        .border_color(UiColors::tint(color, 0.28))
        .child(icon(name, color).size(px(15.)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(ui.foreground)
                .child(notice.clone()),
        )
}

/// Сообщения окна не делятся на уровни: ошибку узнаём по словам (Cannot open…).
fn looks_like_error(message: &str) -> bool {
    let message = message.to_lowercase();
    [
        "cannot",
        "error",
        "failed",
        "denied",
        "not found",
        "no such",
    ]
    .iter()
    .any(|word| message.contains(word))
}

/// Сокращает путь посередине до `max_chars` символов, по границам каталогов: начало
/// (`~` или первый каталог от корня) и столько последних компонентов, сколько влезет —
/// «/private/…/scratchpad/wt-start». Последний компонент длиннее предела — «…» и его конец.
fn shorten_path(path: &str, max_chars: usize) -> String {
    let len = |text: &str| text.chars().count();
    if len(path) <= max_chars {
        return path.to_string();
    }
    let parts: Vec<&str> = path.split('/').collect();
    let (head, rest) = match parts.as_slice() {
        ["", first, rest @ ..] => (format!("/{first}"), rest),
        [first, rest @ ..] => (first.to_string(), rest),
        [] => return path.to_string(),
    };
    // «head/…/» и хвост из последних компонентов.
    let mut budget = max_chars.saturating_sub(len(&head) + 3);
    let mut tail: Vec<&str> = Vec::new();
    for part in rest.iter().rev() {
        let cost = len(part) + usize::from(!tail.is_empty());
        if cost > budget {
            break;
        }
        budget -= cost;
        tail.push(part);
    }
    if tail.is_empty() {
        let last = parts.last().copied().unwrap_or(path);
        let keep = max_chars.saturating_sub(1);
        let skip = len(last).saturating_sub(keep);
        return format!("…{}", last.chars().skip(skip).collect::<String>());
    }
    tail.reverse();
    format!("{head}/…/{}", tail.join("/"))
}

/// Имя каталога; корень диска — путь целиком.
fn display_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| tilde(path), |name| name.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_told_from_information() {
        assert!(looks_like_error("Cannot open a.rs: Permission denied"));
        assert!(looks_like_error("Save failed"));
        assert!(!looks_like_error("Project: ~/dev/flux"));
        assert!(!looks_like_error(
            "Deleted files with unsaved changes stay open — save to restore them"
        ));
    }

    #[test]
    fn long_paths_are_shortened_in_the_middle() {
        assert_eq!(shorten_path("~/dev/flux", 44), "~/dev/flux");
        let scratch = "/private/tmp/claude-501/-Users-me-dev/fc0c1149-64b3/scratchpad/wt-start";
        let short = shorten_path(scratch, 32);
        assert_eq!(short, "/private/…/scratchpad/wt-start");
        assert!(short.chars().count() <= 32);
        assert_eq!(
            shorten_path("~/dev/personal/flux-dev/playground/tree-sandbox", 32),
            "~/…/playground/tree-sandbox"
        );
        // Последний компонент сам длиннее предела.
        assert_eq!(shorten_path("/a/abcdefghijklmnop", 8), "…jklmnop");
        // Кириллица — по символам, не по байтам.
        assert_eq!(
            shorten_path("/дом/проекты/очень-длинное-имя", 12),
            "…длинное-имя"
        );
    }

    #[test]
    fn project_names_come_from_the_last_component() {
        assert_eq!(display_name(Path::new("/Users/me/dev/flux")), "flux");
        assert_eq!(display_name(Path::new("/")), "/");
    }

    #[test]
    fn actions_needing_a_project_are_marked() {
        let ui = Theme::flux_night().ui;
        let without_project: Vec<&str> = quick_actions(&ui)
            .iter()
            .filter(|action| !action.needs_project)
            .map(|action| action.label)
            .collect();
        assert_eq!(without_project, ["Command Palette", "New File", "Open…"]);
    }
}
