//! Design system components (wiki: "Design System"): islands and popovers on glass, icon buttons
//! and toggles, shortcut keys, badges, section labels, hover tooltips. Colors are theme tokens
//! ([`UiColors`]); sizes are the constants below and gpui's spacing scale (4 px step: `p_1` = 4 px,
//! `p_2` = 8 px, `gap_3` = 12 px).

use gpui::{
    Action, AnyView, App, BoxShadow, Context, Div, ElementId, FocusHandle, FontWeight, Hsla, IntoElement,
    KeyBinding, Render, SharedString, Stateful, Window, div, linear_color_stop, linear_gradient,
    point, prelude::*, px,
};

use crate::command_palette::keystroke_label;
use crate::icons::{IconName, icon};
use crate::theme::{self, Theme, UiColors};

/// Corner radii: keys and badges · buttons, fields, rows · cards · islands · overlay windows. A
/// nested radius = the outer radius − the inset (island 12, a row with a 6 inset → 6).
pub const RADIUS_XS: f32 = 4.;
pub const RADIUS_SM: f32 = 6.;
pub const RADIUS_MD: f32 = 8.;
pub const RADIUS_LG: f32 = 12.;
pub const RADIUS_XL: f32 = 16.;

/// The gap between islands and from the islands to the window edge.
pub const GAP: f32 = 8.;
/// The window title bar (macOS traffic lights, project, buttons) and the status bar live on the
/// frame, with no island.
pub const TITLE_BAR_HEIGHT: f32 = 40.;
pub const STATUS_BAR_HEIGHT: f32 = 28.;
/// Icon button and toggle.
pub const ICON_BUTTON_SIZE: f32 = 26.;

/// The width of the left island. The project tree and the commit window take turns in it and share
/// it: dragging the edge of either resizes both, so switching between them (⌘1, ⌘0) doesn't move
/// the editor.
#[derive(Clone)]
pub struct LeftIslandWidth(std::rc::Rc<std::cell::Cell<f32>>);

impl LeftIslandWidth {
    pub const DEFAULT: f32 = 260.;
    /// The commit window's buttons («Commit», «Commit and Push…») need this much.
    pub const MIN: f32 = 220.;
    pub const MAX: f32 = 640.;

    pub fn new() -> Self {
        Self(std::rc::Rc::new(std::cell::Cell::new(Self::DEFAULT)))
    }

    pub fn get(&self) -> f32 {
        self.0.get()
    }

    /// Sets the width, kept within the limits.
    pub fn set(&self, width: f32) {
        self.0.set(width.clamp(Self::MIN, Self::MAX));
    }
}

impl Default for LeftIslandWidth {
    fn default() -> Self {
        Self::new()
    }
}

/// Island: a standalone panel on the window's glass frame (tree, editor). The corner rounding
/// doesn't clip children: their backgrounds must keep away from the edge at the corners.
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

/// Popover: menus, pickers, project search, tooltips. Denser than an island and with a deep shadow:
/// it is above everything else.
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

/// Glass highlight: a light line along the top edge that fades out toward the corners. `inset` is
/// the offset from the side edges (the panel's corner radius).
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

/// Window frame background: glass tint and a colored glow from the top-left corner.
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

/// Focus ring around a field: an accent border plus a soft glow.
pub fn focus_ring(ui: UiColors) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: ui.focus_ring,
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(3.),
    }]
}

/// Button hover group: the icon changes color when the mouse is over the button.
const BUTTON_GROUP: &str = "icon-button";

/// 26×26 icon button: a muted icon, a backdrop on hover. The action is the caller's `on_click`, the
/// tooltip is `tooltip(ui::tooltip(..))`.
pub fn icon_button(id: impl Into<ElementId>, name: IconName, ui: UiColors) -> Stateful<Div> {
    button_base(id, ui)
        .hover(move |style| style.bg(ui.hover))
        .child(
            icon(name, ui.text_muted)
                .size(px(15.))
                .group_hover(BUTTON_GROUP, move |style| style.text_color(ui.foreground)),
        )
}

/// Toggle (case, whole word, regex): when on, an accent backdrop and icon.
pub fn toggle_button(
    id: impl Into<ElementId>,
    name: IconName,
    on: bool,
    ui: UiColors,
) -> Stateful<Div> {
    if !on {
        return icon_button(id, name, ui);
    }
    // A gpui element's `hover` can be set only once (a second call panics), so the button base has
    // none: the regular and the enabled variants each set their own.
    button_base(id, ui)
        .bg(ui.accent_soft)
        .hover(move |style| style.bg(UiColors::tint(ui.accent, 0.26)))
        .child(icon(name, ui.accent_text).size(px(15.)))
}

/// A button with a text label (Update, Delete in Settings): quiet, with a border; `danger` — the
/// label in the error color.
pub fn text_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    danger: bool,
    ui: UiColors,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(ICON_BUTTON_SIZE))
        .px(px(10.))
        .flex()
        .items_center()
        .rounded(px(RADIUS_SM))
        .border_1()
        .border_color(ui.input_border)
        .text_size(px(theme::TEXT_SM))
        .text_color(if danger { ui.error } else { ui.foreground })
        .cursor_pointer()
        .hover(move |style| style.bg(ui.hover))
        .active(move |style| style.bg(ui.pressed))
        .child(label.into())
}

/// An on/off switch: a pill with a knob, accent when on.
pub fn switch(id: impl Into<ElementId>, on: bool, ui: UiColors) -> Stateful<Div> {
    const WIDTH: f32 = 34.;
    const HEIGHT: f32 = 20.;
    const KNOB: f32 = 16.;
    let inset = (HEIGHT - KNOB) / 2.;
    div()
        .id(id)
        .flex_none()
        .relative()
        .w(px(WIDTH))
        .h(px(HEIGHT))
        .rounded(px(HEIGHT / 2.))
        .bg(if on { ui.accent } else { ui.input_border })
        .cursor_pointer()
        .child(
            div()
                .absolute()
                .top(px(inset))
                .left(px(if on { WIDTH - KNOB - inset } else { inset }))
                .size(px(KNOB))
                .rounded(px(KNOB / 2.))
                .bg(ui.foreground),
        )
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

/// A checkbox: on, off, or partly on (a file with some of its changes in the commit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Checked,
    Partial,
    Unchecked,
}

impl CheckState {
    pub fn from_bool(on: bool) -> Self {
        if on { CheckState::Checked } else { CheckState::Unchecked }
    }
}

/// Size of a checkbox.
const CHECKBOX_SIZE: f32 = 14.;

/// A checkbox: empty, checked (accent with a check), or partly checked (accent with a dash).
pub fn checkbox(
    id: impl Into<gpui::ElementId>,
    state: CheckState,
    ui: UiColors,
) -> Stateful<Div> {
    let on = state != CheckState::Unchecked;
    div()
        .id(id)
        .flex_none()
        .size(px(CHECKBOX_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .border_1()
        .cursor_pointer()
        .when(on, |check| check.bg(ui.accent).border_color(ui.accent))
        .when(!on, move |check| {
            check
                .border_color(ui.text_muted)
                .hover(move |style| style.border_color(ui.foreground))
        })
        .children(match state {
            CheckState::Checked => Some(icon(IconName::Check, ui.foreground).size(px(11.))),
            CheckState::Partial => Some(icon(IconName::Minus, ui.foreground).size(px(11.))),
            CheckState::Unchecked => None,
        })
}

/// The main button of a window (Commit, Push): an accent-tinted pill; dimmed when unavailable.
pub fn primary_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
    ui: UiColors,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(ICON_BUTTON_SIZE))
        .px(px(12.))
        .flex()
        .items_center()
        .rounded(px(RADIUS_SM))
        .border_1()
        .border_color(UiColors::tint(ui.accent, 0.55))
        .bg(UiColors::tint(ui.accent, 0.24))
        .text_size(px(theme::TEXT_SM))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ui.accent_text)
        .when(enabled, move |button| {
            button
                .cursor_pointer()
                .hover(move |style| style.bg(UiColors::tint(ui.accent, 0.34)))
                .active(move |style| style.bg(UiColors::tint(ui.accent, 0.42)))
        })
        .when(!enabled, |button| button.opacity(0.5))
        .child(label.into())
}

/// Shortcut keys: "⇧⌘F" → ⇧ ⌘ F, "⌘K ⌘S" → two groups. Modifiers get one key each; the rest of a
/// group is a single key.
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

/// A single key: a small rounded square with a monospaced symbol.
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

/// A keystroke group → keys: modifiers ⌃⌥⇧⌘ one by one, the remainder as a single key.
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

/// The shortcut of an action's first binding in macOS symbols ("⇧⌘P"), for tooltips and buttons.
/// Bindings are looked up along the path from the focused element (previous frame).
pub fn shortcut_for(action: &dyn Action, window: &Window) -> Option<SharedString> {
    shortcut_label(&window.bindings_for_action(action))
}

/// The same, as if focus were in `focus`: a panel's buttons show its keys even when focus is
/// elsewhere (the tree header, the find bar buttons).
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

/// Badge: a number or a short label in `color` on a backdrop of the same hue.
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

/// Section label: small uppercase, semibold, muted.
pub fn section_label(text: impl Into<SharedString>, ui: UiColors) -> Div {
    div()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(ui.dim)
        .child(text.into().to_uppercase())
}

/// A horizontal divider inside an island or a panel.
pub fn divider(ui: UiColors) -> Div {
    div().flex_none().h(px(1.)).bg(ui.divider)
}

/// Popover footer: "↑↓ navigate · ↵ open · esc close".
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

/// Hover tooltip: text and, if there is one, a shortcut. For an element's `tooltip(..)`.
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
        // gpui puts the tooltip at the mouse point; the offset moves it out from under the cursor.
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
