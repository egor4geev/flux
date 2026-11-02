//! Design system components (wiki: "Design System"): islands and popovers on glass, icon buttons
//! and toggles, shortcut keys, badges, section labels, hover tooltips. Colors are theme tokens
//! ([`UiColors`]); sizes are the constants below and gpui's spacing scale (4 px step: `p_1` = 4 px,
//! `p_2` = 8 px, `gap_3` = 12 px).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{
    Action, AnyElement, AnyView, App, Bounds, BoxShadow, Context, Div, ElementId, FocusHandle,
    FontWeight, Hsla, IntoElement, KeyBinding, KeyContext, Keymap, Pixels, Point, Render,
    SharedString, Stateful, Window, WindowBackgroundAppearance, canvas, div, linear_color_stop,
    linear_gradient, point, prelude::*, px,
};

use crate::command_palette::keystroke_label;
use crate::icons::{IconName, icon};
use crate::keymap::{self, KeyOs};
use crate::motion::{self, TransitionExt};
use crate::settings::{self, UiStyle};
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
/// Dialogs and search windows open this far from the top of the window, under the title bar.
pub const DIALOG_TOP: f32 = TITLE_BAR_HEIGHT + 32.;
/// The margin a dialog keeps above the status bar; a dialog is never lower than the minimum.
const DIALOG_MARGIN: f32 = 16.;
const DIALOG_MIN_HEIGHT: f32 = 240.;

/// The height of a dialog under the title bar: its own, unless a small window — or a large interface
/// scale (part 10.4), which makes the window logically smaller — leaves less room above the status
/// bar.
pub fn dialog_height(preferred: f32, window: &Window) -> f32 {
    let room =
        f32::from(window.viewport_size().height) - DIALOG_TOP - STATUS_BAR_HEIGHT - DIALOG_MARGIN;
    preferred.min(room).max(DIALOG_MIN_HEIGHT)
}

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

/// The width of the island on the right, shared by its tool windows (Notifications; plugins' windows
/// later): switching between them doesn't move the editor.
#[derive(Clone)]
pub struct RightIslandWidth(std::rc::Rc<std::cell::Cell<f32>>);

impl RightIslandWidth {
    pub const DEFAULT: f32 = 340.;
    pub const MIN: f32 = 260.;
    pub const MAX: f32 = 720.;

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

impl Default for RightIslandWidth {
    fn default() -> Self {
        Self::new()
    }
}

/// The height of the island under the editor. The terminal panel and the Git window take turns in
/// it and share it: switching between them (⌥F12, ⌘9) doesn't move the editor.
#[derive(Clone)]
pub struct BottomIslandHeight(std::rc::Rc<std::cell::Cell<f32>>);

impl BottomIslandHeight {
    pub const DEFAULT: f32 = 300.;
    pub const MIN: f32 = 120.;
    /// The island takes at most this share of the window's height.
    pub const MAX_SHARE: f32 = 0.7;

    pub fn new() -> Self {
        Self(std::rc::Rc::new(std::cell::Cell::new(Self::DEFAULT)))
    }

    /// The height, within the limits for a window allowing `max` (it may have shrunk).
    pub fn get_within(&self, max: f32) -> f32 {
        self.0.get().min(max).max(Self::MIN)
    }

    /// Sets the height, kept within the limits.
    pub fn set_within(&self, height: f32, max: f32) {
        self.0.set(height.clamp(Self::MIN, max.max(Self::MIN)));
    }
}

impl Default for BottomIslandHeight {
    fn default() -> Self {
        Self::new()
    }
}

// --- The look of the window (Settings → Appearance → Interface, part 10.4) ---

/// The look of the window: Islands on glass, or Flat panels edge to edge.
pub fn style(cx: &App) -> UiStyle {
    settings::ui_style(cx)
}

/// Chooses the look of the window (the Interface page, Quick Switch): saved and applied at once to
/// every window — the theme's surfaces turn opaque or glass again ([`theme::apply`]), the windows'
/// backgrounds follow.
pub fn set_style(style: UiStyle, cx: &mut App) {
    if settings::ui_style(cx) == style {
        return;
    }
    settings::update_appearance(cx, |appearance| appearance.ui_style = style);
    theme::apply(cx);
    // After the event that chose it: the window it came from is being updated until then, and an
    // update of it now would be refused.
    cx.defer(|cx| {
        for window in cx.windows() {
            window
                .update(cx, |_, window, cx| apply_window_style(window, cx))
                .ok();
        }
    });
    cx.refresh_windows();
}

/// Applies the look of the window to a window Flux is opening, or to every window when the look
/// changes: the blurred desktop behind the glass of Islands, an opaque window for Flat.
pub fn apply_window_style(window: &mut Window, cx: &App) {
    window.set_background_appearance(match style(cx) {
        UiStyle::Islands => WindowBackgroundAppearance::Blurred,
        UiStyle::Flat => WindowBackgroundAppearance::Opaque,
    });
}

/// The gap between islands and from the islands to the window's edges: none in Flat, where the
/// panels meet at a line.
pub fn island_gap(style: UiStyle) -> f32 {
    match style {
        UiStyle::Islands => GAP,
        UiStyle::Flat => 0.,
    }
}

/// The thickness of the handle on an island's edge that resizes it (an accent line on hover).
pub const RESIZE_HANDLE: f32 = GAP;

/// How far from the edge of a panel's content the middle of its resize handle is: in the gap
/// between islands (past the island's 1 px border), or on the 1 px line between Flat panels. The
/// handle sits `resize_handle_center + RESIZE_HANDLE / 2` out; dragging puts the edge where the
/// middle of the handle is under the mouse, so the edge doesn't jump.
pub fn resize_handle_center(style: UiStyle) -> f32 {
    match style {
        UiStyle::Islands => 1. + RESIZE_HANDLE / 2.,
        UiStyle::Flat => 0.5,
    }
}

/// Island: a standalone panel on the window's glass frame (tree, editor). The corner rounding
/// doesn't clip children: their backgrounds must keep away from the edge at the corners. In Flat
/// it is a plain panel: no rounding, border, shadow or sheen — the window's layout draws the lines
/// between panels (`Workspace::render`), and the theme's island color is opaque then.
pub fn island(ui: UiColors, style: UiStyle) -> Div {
    match style {
        UiStyle::Islands => div()
            .relative()
            .rounded(px(RADIUS_LG))
            .bg(ui.island)
            .border_1()
            .border_color(ui.island_border)
            .shadow(island_shadow(ui))
            .child(sheen(ui, RADIUS_LG)),
        UiStyle::Flat => div().relative().bg(ui.island),
    }
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

/// The backdrop of a modal dialog: a light shade over the window that takes its clicks (the dialog
/// is modal, as in JetBrains IDEs). No blur and no animation: the dialog simply appears.
pub fn modal_backdrop(ui: UiColors) -> Div {
    div().occlude().bg(UiColors::tint(ui.shadow, 0.18))
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

/// How wide the focus ring is, and how wide it comes in before it closes around the field.
const RING_SPREAD: f32 = 3.;
const HALO_SPREAD: f32 = 10.;

/// The focus ring `t` of the way in (0 → 1, a spring overshooting a little). Coming in, it starts
/// wide, soft and clear and closes around the field, as macOS draws it; going away, it only fades.
pub fn focus_ring_at(ui: UiColors, t: f32, coming: bool) -> Vec<BoxShadow> {
    let (spread, blur) = if coming {
        // Past 1, the overshoot, it draws a hair tighter before it settles.
        (
            motion::lerp(RING_SPREAD, HALO_SPREAD, 1. - t),
            4. * (1. - t).max(0.),
        )
    } else {
        (RING_SPREAD, 0.)
    };
    vec![BoxShadow {
        color: ui.focus_ring.opacity(t),
        offset: point(px(0.), px(0.)),
        blur_radius: px(blur),
        spread_radius: px(spread.max(0.)),
    }]
}

/// A field's focus ring ([`focus_ring_at`]) that comes and goes: on focus the halo comes in wide and settles around
/// the field ([`motion::POP`]), on blur it fades ([`motion::FADE`]). `id` keeps it between frames
/// (unique among the field's siblings); a field that is drawn focused at once has its ring at once.
/// Not on the path of typing: it moves when the focus does.
pub fn focus_halo<E: Styled + IntoElement + 'static>(
    element: E,
    id: impl Into<ElementId>,
    focused: bool,
    ui: UiColors,
) -> motion::Transition<E, f32> {
    focus_transition(element, id, focused, false, ui)
}

/// [`focus_halo`] for a field whose border is [`UiColors::input_border`] at rest: the border turns
/// to [`UiColors::focus_border`] with the ring.
pub fn focus_field<E: Styled + IntoElement + 'static>(
    element: E,
    id: impl Into<ElementId>,
    focused: bool,
    ui: UiColors,
) -> motion::Transition<E, f32> {
    focus_transition(element, id, focused, true, ui)
}

fn focus_transition<E: Styled + IntoElement + 'static>(
    element: E,
    id: impl Into<ElementId>,
    focused: bool,
    border: bool,
    ui: UiColors,
) -> motion::Transition<E, f32> {
    let (target, curve) = if focused {
        (1., motion::POP)
    } else {
        (0., motion::FADE)
    };
    element.with_transition(id, target, curve, move |element, t| {
        let element = if border {
            element.border_color(motion::mix(ui.input_border, ui.focus_border, t))
        } else {
            element
        };
        if t <= 0. {
            element
        } else {
            element.shadow(focus_ring_at(ui, t, focused))
        }
    })
}

/// Button hover group: the icon changes color when the mouse is over the button.
const BUTTON_GROUP: &str = "icon-button";

/// 26×26 icon button: a muted icon, a backdrop on hover. The action is the caller's `on_click`, the
/// tooltip is `tooltip(ui::tooltip(..))`.
pub fn icon_button(id: impl Into<ElementId>, name: IconName, ui: UiColors) -> Stateful<Div> {
    button_base(id, ui)
        .hover(move |style| style.bg(ui.hover))
        .child(button_icon(icon(name, ui.text_muted), ui))
}

/// Toggle (case, whole word, regex): when on, an accent backdrop and icon. Turning on or off, the
/// backdrop and the icon's color crossfade ([`motion::FADE`]).
pub fn toggle_button(
    id: impl Into<ElementId>,
    name: IconName,
    on: bool,
    ui: UiColors,
) -> Stateful<Div> {
    toggle_base(id, on, ui).child(toggle_icon(icon(name, ui.text_muted), on, ui))
}

/// [`icon_button`] with an icon by its asset path: a plugin's SVG (stage 8).
pub fn icon_button_at(
    id: impl Into<ElementId>,
    path: impl Into<SharedString>,
    ui: UiColors,
) -> Stateful<Div> {
    button_base(id, ui)
        .hover(move |style| style.bg(ui.hover))
        .child(button_icon(crate::icons::icon_at(path, ui.text_muted), ui))
}

/// [`toggle_button`] with an icon by its asset path: a plugin's tool window in the launchpad.
pub fn toggle_button_at(
    id: impl Into<ElementId>,
    path: impl Into<SharedString>,
    on: bool,
    ui: UiColors,
) -> Stateful<Div> {
    toggle_base(id, on, ui).child(toggle_icon(
        crate::icons::icon_at(path, ui.text_muted),
        on,
        ui,
    ))
}

/// The icon of an icon button: muted, the foreground color while the mouse is over the button. Its
/// id lets the color fade with the button's backdrop.
fn button_icon(icon: gpui::Svg, ui: UiColors) -> Stateful<gpui::Svg> {
    icon.size(px(15.))
        .id("icon")
        .group_hover(BUTTON_GROUP, move |style| style.text_color(ui.foreground))
}

/// A toggle's button with its accent backdrop, faded in as much as the toggle is on.
fn toggle_base(id: impl Into<ElementId>, on: bool, ui: UiColors) -> Stateful<Div> {
    // A gpui element's `hover` can be set only once (a second call panics), so the button base has
    // none: the regular and the enabled variants each set their own. Over the backdrop of an
    // enabled toggle, the hover adds as much accent again as the backdrop has.
    let hover = if on {
        UiColors::tint(ui.accent, 0.12)
    } else {
        ui.hover
    };
    button_base(id, ui)
        .relative()
        .hover(move |style| style.bg(hover))
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded(px(RADIUS_SM))
                .with_transition(
                    "backdrop",
                    if on { 1. } else { 0. },
                    motion::FADE,
                    move |backdrop, t| backdrop.bg(ui.accent_soft.opacity(t)),
                ),
        )
}

/// A toggle's icon: accent when on, muted (the foreground under the mouse) when off.
fn toggle_icon(icon: gpui::Svg, on: bool, ui: UiColors) -> impl IntoElement {
    icon.size(px(15.))
        .id("icon")
        .when(!on, move |icon| {
            icon.group_hover(BUTTON_GROUP, move |style| style.text_color(ui.foreground))
        })
        .with_transition(
            "icon-color",
            if on { ui.accent_text } else { ui.text_muted },
            motion::FADE,
            |icon, color| icon.text_color(color),
        )
}

/// A button with a text label (Update, Delete in Settings): quiet, with a border; `danger` — the
/// label in the error color.
pub fn text_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    danger: bool,
    ui: UiColors,
) -> Stateful<Div> {
    text_button_frame(id, danger, ui).child(label.into())
}

/// A [`text_button`] without its label.
fn text_button_frame(id: impl Into<ElementId>, danger: bool, ui: UiColors) -> Stateful<Div> {
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
        .active(move |style| motion::press(style.bg(ui.pressed)))
}

/// How much an unavailable button is dimmed ([`available`]).
pub const UNAVAILABLE_OPACITY: f32 = 0.5;

/// A button — [`text_button`], [`icon_button`], [`primary_button`] — available or not: unavailable,
/// it is dimmed, takes no hover or press (no backdrop, no spring) and shows the arrow; the caller
/// adds `on_click` only when it is available. Dimming and coming back fade ([`motion::FADE`]), as a
/// button becomes unavailable while a task runs. The button stays buildable (`on_click`, `tooltip`,
/// styles); its id keys the fade.
pub fn available(button: Stateful<Div>, enabled: bool) -> motion::Transition<Stateful<Div>, f32> {
    let mut button = button;
    let id = ElementId::NamedChild(
        Box::new(
            button
                .interactivity()
                .element_id
                .clone()
                .unwrap_or_else(|| "button".into()),
        ),
        "available".into(),
    );
    if !enabled {
        button.interactivity().clear_interaction_styles();
        button = button.cursor_default();
    }
    let target = if enabled { 1. } else { UNAVAILABLE_OPACITY };
    button.with_transition(id, target, motion::FADE, |button, opacity| {
        button.opacity(opacity)
    })
}

/// How long a Copy button says "Copied" ([`copy_button`]).
pub const COPIED_FOR: std::time::Duration = std::time::Duration::from_millis(1500);

/// When a Copy button ([`copy_button`]) was last pressed, kept by its view.
#[derive(Clone, Copy, Debug, Default)]
pub struct Copied(Option<std::time::Instant>);

impl Copied {
    /// Copied now: the button says so for [`COPIED_FOR`], and the view is drawn again when it stops.
    pub fn now<V: 'static>(&mut self, cx: &mut Context<V>) {
        self.0 = Some(std::time::Instant::now());
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FOR).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
    }

    /// The button says "Copied".
    pub fn showing(&self) -> bool {
        self.0.is_some_and(|at| at.elapsed() < COPIED_FOR)
    }
}

/// A [`text_button`] that copies: "Copy", and for a moment after a press ([`Copied::now`]) a check
/// and "Copied" in the success color; the label crossfades ([`motion::FADE`]) both ways.
pub fn copy_button(id: impl Into<ElementId>, copied: &Copied, ui: UiColors) -> Stateful<Div> {
    let copied = copied.showing();
    let label = if copied {
        div()
            .flex()
            .items_center()
            .gap_1()
            .text_color(ui.success)
            .child(icon(IconName::Check, ui.success).size(px(12.)))
            .child(crate::i18n::tr("Copied"))
    } else {
        div().child(crate::i18n::tr("Copy"))
    };
    // Keyed by what it says: the new label fades in where the old one was.
    text_button_frame(id, false, ui)
        .child(motion::Appear::fade().apply(if copied { "copied" } else { "copy" }, label))
}

/// An on/off switch: a pill with a knob, accent when on. Flipped, the knob glides across with a
/// bounce ([`motion::BOUNCE`]), stretching on the way as a drop would and pressing into the end, and
/// the track's color follows it.
pub fn switch(id: impl Into<ElementId>, on: bool, ui: UiColors) -> Stateful<Div> {
    const WIDTH: f32 = 34.;
    const HEIGHT: f32 = 20.;
    const KNOB: f32 = 16.;
    /// How much wider the knob is halfway across.
    const STRETCH: f32 = 5.;
    let inset = (HEIGHT - KNOB) / 2.;
    let travel = WIDTH - KNOB - 2. * inset;
    div()
        .id(id)
        .flex_none()
        .relative()
        .w(px(WIDTH))
        .h(px(HEIGHT))
        .cursor_pointer()
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded(px(HEIGHT / 2.))
                .with_transition(
                    "switch",
                    if on { 1. } else { 0. },
                    motion::BOUNCE,
                    move |track, t| {
                        let along = t.clamp(0., 1.);
                        // Past either end (the spring's overshoot) the knob presses into the end and
                        // squishes instead of leaving the track.
                        let squish = travel * (t - along).abs();
                        let width = KNOB + STRETCH * (std::f32::consts::PI * along).sin() - squish;
                        let left = if t > 1. {
                            WIDTH - inset - width
                        } else if t < 0. {
                            inset
                        } else {
                            inset + KNOB / 2. + travel * along - width / 2.
                        };
                        track
                            .bg(motion::mix(ui.input_border, ui.accent, along))
                            .child(
                                div()
                                    .absolute()
                                    .top(px(inset))
                                    .left(px(left))
                                    .w(px(width))
                                    .h(px(KNOB))
                                    .rounded(px(KNOB / 2.))
                                    .bg(ui.on_accent)
                                    // The knob stands out of a light track too.
                                    .shadow(vec![BoxShadow {
                                        color: UiColors::tint(ui.shadow, 0.3),
                                        offset: point(px(0.), px(1.)),
                                        blur_radius: px(2.),
                                        spread_radius: px(0.),
                                    }]),
                            )
                    },
                ),
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
        .active(move |style| motion::press(style.bg(ui.pressed)))
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
        if on {
            CheckState::Checked
        } else {
            CheckState::Unchecked
        }
    }
}

/// A segmented control's gliding pill (Settings: the interface scale, the extra indent): where its
/// segments were drawn last, relative to the control, so the selected one's pill can glide from one
/// to another — segments may wrap and have widths of their own, so their places are measured, not
/// counted. A segment that moved asks for one more frame, and the pill follows. The control is
/// `relative()`; its children: [`Segments::measure_origin`], [`Segments::pill`], the segments, each
/// with a [`Segments::measure`] child and no backdrop of its own while the pill is there.
#[derive(Default)]
pub struct Segments {
    /// The control's padding box in the window, this frame (measured before its segments).
    origin: Cell<Point<Pixels>>,
    /// Each segment's place in the control.
    places: RefCell<Vec<Option<Bounds<Pixels>>>>,
}

impl Segments {
    /// The control's first child: measures where it is, before its segments.
    pub fn measure_origin(self: &Rc<Self>) -> impl IntoElement {
        let segments = self.clone();
        canvas(
            move |bounds, _, _| segments.origin.set(bounds.origin),
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    /// A child of each segment: measures its place, for the pill.
    pub fn measure(self: &Rc<Self>, index: usize) -> impl IntoElement {
        let segments = self.clone();
        canvas(
            move |bounds, window, _| {
                let place = Bounds {
                    origin: bounds.origin - segments.origin.get(),
                    size: bounds.size,
                };
                let mut places = segments.places.borrow_mut();
                if places.len() <= index {
                    places.resize(index + 1, None);
                }
                if places[index] != Some(place) {
                    places[index] = Some(place);
                    window.request_animation_frame();
                }
            },
            |_, _, _, _| {},
        )
        // Insets of its own: without them it would sit where the segment's content starts.
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    /// The pill under the selected segment, once its place is known: it glides to the next one
    /// chosen ([`motion::POP`]).
    pub fn pill(
        &self,
        selected: Option<usize>,
        id: &'static str,
        ui: UiColors,
    ) -> Option<AnyElement> {
        let place = (*self.places.borrow()).get(selected?).copied().flatten()?;
        let target = [
            f32::from(place.origin.x),
            f32::from(place.origin.y),
            f32::from(place.size.width),
            f32::from(place.size.height),
        ];
        Some(
            div()
                .absolute()
                .rounded(px(RADIUS_SM))
                .bg(ui.list_selected)
                .with_transition(id, target, motion::POP, |pill, [x, y, width, height]| {
                    pill.left(px(x)).top(px(y)).w(px(width)).h(px(height))
                })
                .into_any_element(),
        )
    }
}

/// Size of a checkbox.
const CHECKBOX_SIZE: f32 = 14.;

/// What a checkmark or a radio's dot grows from as it pops in.
const MARK_SCALE: f32 = 0.4;

/// A mark coming in pops with a bounce; going away, it is gone quicker.
fn mark_curve(on: bool) -> motion::Curve {
    if on { motion::BOUNCE } else { motion::FADE }
}

/// A checkbox: empty, checked (accent with a check), or partly checked (accent with a dash).
/// Checked, the accent floods in and the mark pops; unchecked, both fade. Between checked and
/// partly checked the accent stays and the marks swap: the new one pops in as the old one shrinks
/// away.
pub fn checkbox(id: impl Into<gpui::ElementId>, state: CheckState, ui: UiColors) -> Stateful<Div> {
    let on = state != CheckState::Unchecked;
    // How much of the fill, the check and the dash shows.
    let target = match state {
        CheckState::Checked => [1., 1., 0., 0.],
        CheckState::Partial => [1., 0., 1., 0.],
        CheckState::Unchecked => [0., 0., 0., 0.],
    };
    // A mark, `t` of the way in (past 1: the bounce's overshoot).
    let mark = move |name: IconName, t: f32| {
        (t > 0.).then(|| {
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .opacity(t.min(1.))
                .child(
                    icon(name, ui.on_accent)
                        .size(px(11.))
                        .transform_scale(motion::lerp(MARK_SCALE, 1., t)),
                )
        })
    };
    div()
        .id(id)
        .flex_none()
        .relative()
        .size(px(CHECKBOX_SIZE))
        .rounded(px(4.))
        .border_1()
        .cursor_pointer()
        .when(on, |check| check.border_color(ui.accent))
        .when(!on, move |check| {
            check
                .border_color(ui.text_muted)
                .hover(move |style| style.border_color(ui.foreground))
        })
        .child(
            // Over the border too: the fill is the whole box.
            div()
                .absolute()
                .top(px(-1.))
                .left(px(-1.))
                .size(px(CHECKBOX_SIZE))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .with_transition(
                    "mark",
                    target,
                    mark_curve(on),
                    move |fill, [t, check, dash, _]| {
                        if t <= 0. {
                            return fill;
                        }
                        fill.bg(ui.accent)
                            .opacity(t.min(1.))
                            .children(mark(IconName::Check, check))
                            .children(mark(IconName::Minus, dash))
                    },
                ),
        )
}

/// A radio button: a ring, with an accent dot when selected (Update Project's Merge / Rebase). The
/// dot pops in with a bounce.
pub fn radio(id: impl Into<gpui::ElementId>, selected: bool, ui: UiColors) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(CHECKBOX_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(CHECKBOX_SIZE / 2.))
        .border_1()
        .cursor_pointer()
        .when(selected, |radio| radio.border_color(ui.accent))
        .when(!selected, move |radio| {
            radio
                .border_color(ui.text_muted)
                .hover(move |style| style.border_color(ui.foreground))
        })
        .child(div().with_transition(
            "dot",
            if selected { 1. } else { 0. },
            mark_curve(selected),
            move |dot, t| {
                if t <= 0. {
                    return dot;
                }
                dot.size(px(6.))
                    .rounded(px(3.))
                    .bg(ui.accent)
                    .opacity(t.min(1.))
                    .transform_scale(motion::lerp(MARK_SCALE, 1., t))
            },
        ))
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
                .active(move |style| motion::press(style.bg(UiColors::tint(ui.accent, 0.42))))
        })
        .when(!enabled, |button| button.opacity(0.5))
        .child(label.into())
}

/// Shortcut keys: "⇧⌘F" → ⇧ ⌘ F, "⌘K ⌘S" → two groups. Modifiers get one key each; the rest of a
/// group is a single key. A label written in macOS symbols reads the system's way on Windows
/// ("Ctrl+Shift+F" → Ctrl Shift F, [`keymap::label`]).
pub fn keys(label: &str, ui: UiColors) -> Div {
    let os = KeyOs::current();
    let label = keymap::label(os, label);
    div().flex().flex_none().items_center().gap_1p5().children(
        label
            .split(' ')
            .filter(|group| !group.is_empty())
            .map(|group| {
                div()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .children(split_keys(os, group).into_iter().map(|key| keycap(key, ui)))
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

/// A keystroke group → keys: modifiers ⌃⌥⇧⌘ one by one, the remainder as a single key; on Windows
/// the parts between `+` ("Ctrl+Shift+F"), a `+` key included ("Ctrl++").
fn split_keys(os: KeyOs, group: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut rest = group;
    if os == KeyOs::Windows {
        while let Some((modifier, tail)) = rest.split_once('+') {
            if tail.is_empty() || !["Ctrl", "Alt", "Shift", "Win", "Fn"].contains(&modifier) {
                break;
            }
            keys.push(modifier.to_string());
            rest = tail;
        }
        if !rest.is_empty() {
            keys.push(rest.to_string());
        }
        return keys;
    }
    while let Some(c) = rest.chars().next().filter(|c| "⌃⌥⇧⌘".contains(*c)) {
        keys.push(c.to_string());
        rest = &rest[c.len_utf8()..];
    }
    if !rest.is_empty() {
        keys.push(rest.to_string());
    }
    keys
}

/// The shortcut of an action in the system's notation ("⇧⌘P", "Ctrl+Shift+P"), for tooltips and buttons: its first binding
/// that works at the window's level, else its first binding in the key context it has (a panel's
/// own keys). It doesn't depend on the focus: gpui's `Window::bindings_for_action` goes by the key
/// contexts of whatever the last frame painted last — a dialog, a tooltip, a popup — and the labels
/// came and went with them (part 10.1).
pub fn shortcut_for(action: &dyn Action, cx: &App) -> Option<SharedString> {
    action_binding(action, cx).map(|binding| shortcut_label(&binding))
}

/// The same, as if focus were in `focus`: a panel's buttons show its keys even when focus is
/// elsewhere (the tree header, the find bar buttons). Until the panel is drawn (its first frame),
/// as [`shortcut_for`].
pub fn shortcut_in(
    action: &dyn Action,
    focus: &FocusHandle,
    window: &Window,
    cx: &App,
) -> Option<SharedString> {
    match window.bindings_for_action_in(action, focus).first() {
        Some(binding) => Some(shortcut_label(binding)),
        None => shortcut_for(action, cx),
    }
}

/// The binding an action's label shows ([`shortcut_for`]).
pub fn action_binding(action: &dyn Action, cx: &App) -> Option<KeyBinding> {
    label_binding(&cx.key_bindings().borrow(), action)
}

/// The first binding of `action` that the window's own key context (`Workspace`) reaches and
/// nothing there shadows, else the first binding it has in any context.
fn label_binding(keymap: &Keymap, action: &dyn Action) -> Option<KeyBinding> {
    let mut window_level = KeyContext::default();
    window_level.add("Workspace");
    let contexts = [window_level];
    let mut first = None;
    for binding in keymap.bindings_for_action(action) {
        let (found, _) = keymap.bindings_for_input(binding.keystrokes(), &contexts);
        if found
            .first()
            .is_some_and(|found| found.action().partial_eq(action))
        {
            return Some(binding.clone());
        }
        first.get_or_insert_with(|| binding.clone());
    }
    first
}

/// A binding's keys in the system's notation ([`keystroke_label`]), a chord's keystrokes apart.
pub fn shortcut_label(binding: &KeyBinding) -> SharedString {
    binding
        .keystrokes()
        .iter()
        .map(|k| keystroke_label(k.modifiers(), k.key()))
        .collect::<Vec<_>>()
        .join(" ")
        .into()
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
    move |window, cx| {
        let tooltip = Tooltip {
            text: text.clone(),
            keys: keys.clone(),
            handoff: window.tooltip_handoff(),
        };
        cx.new(|_| tooltip).into()
    }
}

/// A tooltip grows from this scale: less than a popover, it is small and quick.
const TOOLTIP_SCALE: f32 = 0.94;

struct Tooltip {
    text: SharedString,
    keys: Option<SharedString>,
    /// Shown at once after another tooltip (moving along a toolbar): it only fades in.
    handoff: bool,
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        // gpui puts the tooltip at the mouse point; the offset moves it out from under the cursor.
        // It pops out of the corner by the pointer, once: a tooltip is made anew each time it shows.
        // Right after another one it shows at once and only fades in (gpui: `tooltip_handoff`).
        let appear = if self.handoff {
            motion::Appear::fade()
        } else {
            motion::Appear::pop((0., 0.)).scale(TOOLTIP_SCALE)
        };
        div().pl(px(10.)).pt(px(18.)).child(
            appear.apply(
                "tooltip",
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
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_split_into_keys() {
        let mac = |group| split_keys(KeyOs::Mac, group);
        assert_eq!(mac("⇧⌘F"), ["⇧", "⌘", "F"]);
        assert_eq!(mac("⌘PgUp"), ["⌘", "PgUp"]);
        assert_eq!(mac("F2"), ["F2"]);
        assert_eq!(mac("⌥⌘"), ["⌥", "⌘"]);
        let windows = |group| split_keys(KeyOs::Windows, group);
        assert_eq!(windows("Ctrl+Shift+F"), ["Ctrl", "Shift", "F"]);
        assert_eq!(windows("Ctrl+PgUp"), ["Ctrl", "PgUp"]);
        assert_eq!(windows("F2"), ["F2"]);
        assert_eq!(windows("Ctrl++"), ["Ctrl", "+"]);
        assert_eq!(windows("Shift+↑↓"), ["Shift", "↑↓"]);
        assert_eq!(windows("+"), ["+"]);
    }

    gpui::actions!(
        label_test,
        [
            Commit,
            ClearTerminal,
            MentionSelection,
            HideApp,
            NewTab,
            Unbound
        ]
    );

    /// The label's binding doesn't depend on where the focus is: the window's own key first (a
    /// context-dependent key goes to the window's action), else the action's binding wherever it
    /// works.
    #[test]
    fn labels_take_the_window_level_binding_then_any() {
        let keymap = Keymap::new(vec![
            KeyBinding::new("cmd-k", Commit, Some("Workspace")),
            KeyBinding::new("cmd-k", ClearTerminal, Some("Terminal")),
            KeyBinding::new("alt-cmd-k", MentionSelection, Some("Editor")),
            KeyBinding::new("cmd-h", HideApp, None),
            KeyBinding::new("cmd-t", NewTab, Some("Terminal")),
            KeyBinding::new("ctrl-t", NewTab, Some("Workspace")),
        ]);
        let label = |action: &dyn Action| {
            label_binding(&keymap, action).map(|binding| shortcut_label(&binding).to_string())
        };
        // The keys as they are bound, in the system's labels.
        let on = |mac, windows| match crate::keymap::KeyOs::current() {
            crate::keymap::KeyOs::Mac => Some(mac),
            crate::keymap::KeyOs::Windows => Some(windows),
        };
        assert_eq!(label(&Commit).as_deref(), on("⌘K", "Win+K"));
        assert_eq!(label(&ClearTerminal).as_deref(), on("⌘K", "Win+K"));
        assert_eq!(label(&MentionSelection).as_deref(), on("⌥⌘K", "Alt+Win+K"));
        assert_eq!(label(&HideApp).as_deref(), on("⌘H", "Win+H"));
        // Bound in a panel first, at the window's level after: the window's key is shown.
        assert_eq!(label(&NewTab).as_deref(), on("⌃T", "Ctrl+T"));
        assert_eq!(label(&Unbound), None);
    }

    /// Islands keep their resize handles in the gap, as before part 10.4 (1 px border + the gap,
    /// the edge under the middle); Flat panels have no gap, and the handle straddles their line.
    #[test]
    fn resize_handles_sit_in_the_gap_or_on_the_line() {
        assert_eq!(island_gap(UiStyle::Islands), GAP);
        assert_eq!(island_gap(UiStyle::Flat), 0.);
        let islands = resize_handle_center(UiStyle::Islands);
        assert_eq!(islands, 1. + GAP / 2.);
        assert_eq!(islands + RESIZE_HANDLE / 2., 1. + GAP);
        let flat = resize_handle_center(UiStyle::Flat);
        assert!(flat - RESIZE_HANDLE / 2. < 0. && flat + RESIZE_HANDLE / 2. > 1.);
    }
}
