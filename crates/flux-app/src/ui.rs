//! Компоненты дизайн-системы (вики: «Design System»): острова и всплывающие панели на
//! стекле, кнопки-значки и переключатели, клавиши сочетаний, бейджи, подписи разделов,
//! подсказки при наведении. Цвета — токены темы ([`UiColors`]), размеры — константы ниже
//! и шкала отступов gpui (шаг 4 px: `p_1` = 4 px, `p_2` = 8 px, `gap_3` = 12 px).

use gpui::{
    Action, AnyView, App, BoxShadow, Context, Div, ElementId, FocusHandle, Hsla, IntoElement,
    KeyBinding, Render, SharedString, Stateful, Window, div, linear_color_stop, linear_gradient,
    point, prelude::*, px,
};

use crate::command_palette::keystroke_label;
use crate::icons::{IconName, icon};
use crate::theme::{self, Theme, UiColors};

/// Скругления: клавиши и бейджи · кнопки, поля, строки · карточки · острова · окна поверх.
/// Вложенное скругление = внешнее − отступ (остров 12, строка с отступом 6 → 6).
pub const RADIUS_XS: f32 = 4.;
pub const RADIUS_SM: f32 = 6.;
pub const RADIUS_MD: f32 = 8.;
pub const RADIUS_LG: f32 = 12.;
pub const RADIUS_XL: f32 = 16.;

/// Зазор между островами и от островов до края окна.
pub const GAP: f32 = 8.;
/// Шапка окна (светофор macOS, проект, кнопки) и статус-бар — на рамке, без острова.
pub const TITLE_BAR_HEIGHT: f32 = 40.;
pub const STATUS_BAR_HEIGHT: f32 = 28.;
/// Кнопка-значок и переключатель.
pub const ICON_BUTTON_SIZE: f32 = 26.;

/// Остров: самостоятельная панель на стеклянной рамке окна (дерево, редактор).
/// Скругление не обрезает детей: их фон у углов должен отступать от края.
pub fn island(ui: UiColors) -> Div {
    div()
        .relative()
        .rounded(px(RADIUS_LG))
        .bg(ui.island)
        .border_1()
        .border_color(ui.island_border)
        .shadow(island_shadow(ui))
        .child(sheen(ui, RADIUS_LG))
}

/// Всплывающая панель: меню, списки выбора, поиск по проекту, подсказки. Плотнее острова
/// и с глубокой тенью — она над всем остальным.
pub fn popover(ui: UiColors) -> Div {
    div()
        .relative()
        .rounded(px(RADIUS_XL))
        .bg(ui.elevated)
        .border_1()
        .border_color(ui.elevated_border)
        .shadow(popover_shadow(ui))
        .font_family(theme::UI_FONT)
        .text_size(px(theme::TEXT_MD))
        .text_color(ui.foreground)
        .child(sheen(ui, RADIUS_XL))
}

/// Блик стекла: светлая линия по верхнему краю, гаснущая к углам. `inset` — отступ от
/// боковых краёв (скругление панели).
pub fn sheen(ui: UiColors, inset: f32) -> impl IntoElement {
    let clear = UiColors::tint(ui.sheen, 0.);
    div()
        .absolute()
        .top_0()
        .left(px(inset))
        .right(px(inset))
        .h(px(1.))
        .flex()
        .child(div().flex_1().h_full().bg(linear_gradient(
            90.,
            linear_color_stop(clear, 0.),
            linear_color_stop(ui.sheen, 1.),
        )))
        .child(div().flex_1().h_full().bg(linear_gradient(
            90.,
            linear_color_stop(ui.sheen, 0.),
            linear_color_stop(clear, 1.),
        )))
}

/// Фон рамки окна: тонировка стекла и цветной отсвет из верхнего левого угла.
pub fn frame_glow(ui: UiColors) -> impl IntoElement {
    div().absolute().inset_0().bg(linear_gradient(
        135.,
        linear_color_stop(ui.frame_glow, 0.),
        linear_color_stop(UiColors::tint(ui.frame_glow, 0.), 0.55),
    ))
}

pub fn island_shadow(ui: UiColors) -> Vec<BoxShadow> {
    vec![
        BoxShadow {
            color: UiColors::tint(ui.shadow, 0.25),
            offset: point(px(0.), px(1.)),
            blur_radius: px(2.),
            spread_radius: px(0.),
        },
        BoxShadow {
            color: UiColors::tint(ui.shadow, 0.22),
            offset: point(px(0.), px(8.)),
            blur_radius: px(24.),
            spread_radius: px(-6.),
        },
    ]
}

pub fn popover_shadow(ui: UiColors) -> Vec<BoxShadow> {
    vec![
        BoxShadow {
            color: UiColors::tint(ui.shadow, 0.35),
            offset: point(px(0.), px(2.)),
            blur_radius: px(6.),
            spread_radius: px(0.),
        },
        BoxShadow {
            color: ui.shadow,
            offset: point(px(0.), px(18.)),
            blur_radius: px(48.),
            spread_radius: px(-8.),
        },
    ]
}

/// Кольцо фокуса вокруг поля: акцентная рамка плюс мягкое свечение.
pub fn focus_ring(ui: UiColors) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: ui.focus_ring,
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(3.),
    }]
}

/// Группа наведения кнопок: значок меняет цвет, когда мышь над кнопкой.
const BUTTON_GROUP: &str = "icon-button";

/// Кнопка-значок 26×26: приглушённый значок, подложка при наведении. Действие — `on_click`
/// у вызывающего, подсказка — `tooltip(ui::tooltip(..))`.
pub fn icon_button(id: impl Into<ElementId>, name: IconName, ui: UiColors) -> Stateful<Div> {
    button_base(id, ui)
        .hover(move |style| style.bg(ui.hover))
        .child(
            icon(name, ui.text_muted)
                .size(px(15.))
                .group_hover(BUTTON_GROUP, move |style| style.text_color(ui.foreground)),
        )
}

/// Переключатель (регистр, целое слово, regex): включённый — акцентная подложка и значок.
pub fn toggle_button(
    id: impl Into<ElementId>,
    name: IconName,
    on: bool,
    ui: UiColors,
) -> Stateful<Div> {
    if !on {
        return icon_button(id, name, ui);
    }
    // `hover` у элемента gpui задаётся один раз (повторный вызов — паника), поэтому у основы
    // кнопки его нет: обычная и включённая задают свой.
    button_base(id, ui)
        .bg(ui.accent_soft)
        .hover(move |style| style.bg(UiColors::tint(ui.accent, 0.26)))
        .child(icon(name, ui.accent_text).size(px(15.)))
}

fn button_base(id: impl Into<ElementId>, ui: UiColors) -> Stateful<Div> {
    div()
        .id(id)
        .group(BUTTON_GROUP)
        .flex_none()
        .size(px(ICON_BUTTON_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(RADIUS_SM))
        .cursor_pointer()
        .active(move |style| style.bg(ui.pressed))
}

/// Клавиши сочетания: «⇧⌘F» → ⇧ ⌘ F, «⌘K ⌘S» → две группы. Модификаторы — по одной
/// клавише, остальное в группе — одна клавиша.
pub fn keys(label: &str, ui: UiColors) -> Div {
    div().flex().flex_none().items_center().gap_1p5().children(
        label
            .split(' ')
            .filter(|group| !group.is_empty())
            .map(|group| {
                div()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .children(split_keys(group).into_iter().map(|key| keycap(key, ui)))
            }),
    )
}

/// Одна клавиша: квадратик со скруглением, моноширинный значок.
pub fn keycap(key: impl Into<SharedString>, ui: UiColors) -> Div {
    div()
        .flex_none()
        .min_w(px(18.))
        .h(px(18.))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(RADIUS_XS))
        .bg(ui.keycap)
        .border_1()
        .border_color(ui.keycap_border)
        .font_family(theme::UI_FONT)
        .text_size(px(theme::TEXT_XS))
        .text_color(ui.text_muted)
        .child(key.into())
}

/// Группа нажатия → клавиши: модификаторы ⌃⌥⇧⌘ по одному, остаток — одной клавишей.
fn split_keys(group: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut rest = group;
    while let Some(c) = rest.chars().next().filter(|c| "⌃⌥⇧⌘".contains(*c)) {
        keys.push(c.to_string());
        rest = &rest[c.len_utf8()..];
    }
    if !rest.is_empty() {
        keys.push(rest.to_string());
    }
    keys
}

/// Сочетание первой привязки действия значками macOS («⇧⌘P») — для подсказок и кнопок.
/// Привязки — по пути от элемента в фокусе (прошлый кадр).
pub fn shortcut_for(action: &dyn Action, window: &Window) -> Option<SharedString> {
    shortcut_label(&window.bindings_for_action(action))
}

/// То же, как если бы фокус был в `focus`: кнопки панели показывают её клавиши, даже когда
/// фокус в другом месте (шапка дерева, кнопки строки поиска).
pub fn shortcut_in(
    action: &dyn Action,
    focus: &FocusHandle,
    window: &Window,
) -> Option<SharedString> {
    shortcut_label(&window.bindings_for_action_in(action, focus))
}

fn shortcut_label(bindings: &[KeyBinding]) -> Option<SharedString> {
    let binding = bindings.first()?;
    let label = binding
        .keystrokes()
        .iter()
        .map(|k| keystroke_label(k.modifiers(), k.key()))
        .collect::<Vec<_>>()
        .join(" ");
    Some(label.into())
}

/// Бейдж: число или короткая метка цветом `color` на подложке того же оттенка.
pub fn badge(text: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .flex_none()
        .h(px(18.))
        .px_1p5()
        .flex()
        .items_center()
        .rounded(px(9.))
        .bg(UiColors::tint(color, 0.16))
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(color)
        .child(text.into())
}

/// Подпись раздела: мелкие прописные, полужирные, приглушённые.
pub fn section_label(text: impl Into<SharedString>, ui: UiColors) -> Div {
    div()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(ui.dim)
        .child(text.into().to_uppercase())
}

/// Горизонтальный разделитель внутри острова или панели.
pub fn divider(ui: UiColors) -> Div {
    div().flex_none().h(px(1.)).bg(ui.divider)
}

/// Подвал всплывающей панели: «↑↓ navigate · ↵ open · esc close».
pub fn hint_bar(hints: &[(&str, &str)], ui: UiColors) -> Div {
    div()
        .flex()
        .items_center()
        .gap_4()
        .text_size(px(theme::TEXT_SM))
        .children(hints.iter().map(|(shortcut, label)| {
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(keys(shortcut, ui))
                .child(div().text_color(ui.dim).child(label.to_string()))
        }))
}

/// Подсказка при наведении: текст и, если есть, сочетание. Для `tooltip(..)` у элемента.
pub fn tooltip(
    text: impl Into<SharedString>,
    keys: Option<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| {
        let tooltip = Tooltip {
            text: text.clone(),
            keys: keys.clone(),
        };
        cx.new(|_| tooltip).into()
    }
}

struct Tooltip {
    text: SharedString,
    keys: Option<SharedString>,
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        // gpui ставит подсказку в точку мыши — отступ уводит её из-под курсора.
        div().pl(px(10.)).pt(px(18.)).child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded(px(RADIUS_SM))
                .bg(ui.elevated)
                .border_1()
                .border_color(ui.elevated_border)
                .shadow(popover_shadow(ui))
                .font_family(theme::UI_FONT)
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.foreground)
                .child(self.text.clone())
                .children(self.keys.clone().map(|shortcut| keys(&shortcut, ui))),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_split_into_keys() {
        assert_eq!(split_keys("⇧⌘F"), ["⇧", "⌘", "F"]);
        assert_eq!(split_keys("⌘PgUp"), ["⌘", "PgUp"]);
        assert_eq!(split_keys("F2"), ["F2"]);
        assert_eq!(split_keys("⌥⌘"), ["⌥", "⌘"]);
    }
}
