//! Motion (2026-10-28): how Flux moves. The author chose a lively character, as Liquid Glass:
//! springs with a light overshoot, popovers growing out of the place they came from, buttons that
//! spring back when let go, cards sliding in, switches gliding. The rules (wiki: Design Tokens →
//! Движение):
//!
//! - **Nothing waits for an animation.** An element is interactive and focused from its first
//!   frame; one that has closed takes no input while it fades out ([`inert`]).
//! - **Nothing animates on the path of input**: typing, the caret, scrolling by hand, a selection
//!   moved with the keyboard. Jumps (go to definition, the next match) do glide.
//! - **Nothing animates while idle.** Every animation ends; the repeating pulses tick at 30 fps
//!   (the gpui patch), so a still window draws no frames.
//! - **Off is off**: Settings → Appearance → Interface → Animations, or macOS's "Reduce motion" —
//!   things simply appear, as before.
//!
//! The pieces:
//! - curves ([`Curve`]) and the tokens of the design system ([`POP`], [`BOUNCE`], [`SMOOTH`]…);
//! - [`Appear`] — an element coming into view (popover, card, row), keyed by an id;
//! - [`TransitionExt::with_transition`] — a value gliding to its new target whenever it changes (a
//!   switch's knob, a tab's highlight, a panel's width), as SwiftUI's implicit animations;
//! - [`progress`] / [`Leaving`] — animations driven by a model's own timestamps: what appears
//!   in a list that recycles its rows, what has left and fades out;
//! - the gpui patch — `transform_scale` / `transform_origin` / `transform_translate` on any
//!   element (paint only: layout and the mouse keep the real bounds), hover and press styles that
//!   fade ([`apply`]), `Window::with_inert`.

use std::cell::Cell;
use std::f32::consts::TAU;
use std::panic::Location;
use std::time::{Duration, Instant};

use gpui::{
    AnimationExt, AnyElement, App, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    InteractionTransitions, IntoElement, LayoutId, Pixels, Styled, Window, px,
};

// --- On or off ---

thread_local! {
    /// Settings → Appearance → Interface → Animations.
    static SETTING: Cell<bool> = const { Cell::new(true) };
    /// macOS's "Reduce motion", and when it was last asked.
    static REDUCE: Cell<Option<(Instant, bool)>> = const { Cell::new(None) };
}

/// How often the system's "Reduce motion" is asked again.
const REDUCE_RECHECK: Duration = Duration::from_secs(2);

/// Scenarios (feature `scenario`): `FLUX_SLOW_MOTION=<factor>` stretches every animation, so a
/// shot taken half a second after its step catches the middle of a transition.
#[cfg(feature = "scenario")]
fn slow_motion() -> f32 {
    static FACTOR: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *FACTOR.get_or_init(|| {
        std::env::var("FLUX_SLOW_MOTION")
            .ok()
            .and_then(|factor| factor.parse::<f32>().ok())
            .filter(|factor| factor.is_finite() && *factor > 0.)
            .unwrap_or(1.)
    })
}

#[cfg(not(feature = "scenario"))]
fn slow_motion() -> f32 {
    1.
}

/// Animations are on: the setting, and macOS's "Reduce motion" off. Cheap — call it in render.
pub fn enabled() -> bool {
    SETTING.with(Cell::get) && !reduce_motion()
}

/// macOS's "Reduce motion" (System Settings → Accessibility → Display), asked at most every two
/// seconds; a change turns the hover fades off or on at once.
pub fn reduce_motion() -> bool {
    let now = Instant::now();
    if let Some((asked, reduce)) = REDUCE.with(Cell::get)
        && now.duration_since(asked) < REDUCE_RECHECK
    {
        return reduce;
    }
    let reduce = system_reduce_motion();
    let changed = REDUCE
        .with(Cell::get)
        .is_some_and(|(_, before)| before != reduce);
    REDUCE.with(|cell| cell.set(Some((now, reduce))));
    if changed {
        push_interactions();
    }
    reduce
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // `msg_send!` of objc 0.2 checks a `cargo-clippy` feature.
fn system_reduce_motion() -> bool {
    use objc::runtime::{BOOL, NO, Object};
    use objc::{class, msg_send, sel, sel_impl};
    // SAFETY: AppKit's shared workspace and a BOOL property, on the main thread.
    unsafe {
        let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        let reduce: BOOL = msg_send![workspace, accessibilityDisplayShouldReduceMotion];
        reduce != NO
    }
}

#[cfg(not(target_os = "macos"))]
fn system_reduce_motion() -> bool {
    false
}

/// Applies the setting (at startup and when it changes): the hover and press fades of every
/// element follow it.
pub fn apply(cx: &App) {
    SETTING.with(|cell| cell.set(crate::settings::animations(cx)));
    push_interactions();
}

/// Hands gpui the fades of hover and press styles, or none.
fn push_interactions() {
    let on = SETTING.with(Cell::get) && !REDUCE.with(Cell::get).is_some_and(|(_, reduce)| reduce);
    let slow = slow_motion();
    gpui::set_interaction_transitions(on.then_some(InteractionTransitions {
        hover_in: HOVER_IN.mul_f32(slow),
        hover_out: HOVER_OUT.mul_f32(slow),
        press_in: PRESS_IN.mul_f32(slow),
        press_out: RELEASE.duration(),
        easing: ease_out_cubic,
        release_easing: release,
    }));
}

/// The release of a pressed element: [`RELEASE`] as a plain function, for gpui.
fn release(t: f32) -> f32 {
    RELEASE.ease(t)
}

// --- Tokens ---

/// Popovers, menus, pickers and dialogs appearing: quick, with a hair of overshoot.
pub const POP: Curve = Curve::spring(0.34, 0.8);
/// What comes in from outside and settles — notification cards, badges, a switch's knob, a
/// checkmark: a visible, friendly bounce.
pub const BOUNCE: Curve = Curve::spring(0.42, 0.66);
/// Layout — panels opening and closing, tabs moving apart, a highlight gliding to its new place,
/// rows unfolding: no overshoot.
pub const SMOOTH: Curve = Curve::spring(0.38, 1.0);
/// A pressed element let go: it springs back past its size and settles.
pub const RELEASE: Curve = Curve::spring(0.3, 0.55);
/// Scrolling to a place (go to definition, the next match, Page Down): quick to start, soft to
/// stop.
pub const SCROLL: Curve = Curve::eased(Duration::from_millis(240), Easing::OutQuint);
/// Content changing in place: a crossfade.
pub const FADE: Curve = Curve::eased(Duration::from_millis(160), Easing::OutCubic);
/// Leaving: quicker than coming.
pub const EXIT: Curve = Curve::eased(Duration::from_millis(130), Easing::OutCubic);

/// Into a hover style, and back out of it (slower, so a row the mouse left fades behind it).
pub const HOVER_IN: Duration = Duration::from_millis(70);
pub const HOVER_OUT: Duration = Duration::from_millis(220);
/// Into a press style: the feedback of a mouse down, almost at once.
pub const PRESS_IN: Duration = Duration::from_millis(60);

/// A popover or a menu grows out of its anchor from this scale.
pub const POP_SCALE: f32 = 0.9;
/// A dialog or a picker in the middle of the window grows from this scale.
pub const DIALOG_SCALE: f32 = 0.95;
/// A button held down shrinks to this ([`press`]).
pub const PRESS_SCALE: f32 = 0.95;
/// What leaves shrinks to this as it fades.
pub const EXIT_SCALE: f32 = 0.97;
/// How far a card or a panel slides in from its edge.
pub const SLIDE: f32 = 24.;

/// The press of an icon button, a chip, a primary button: shrinks while held, springs back when let
/// go — for an element's `active` style (`.active(motion::press)`; with `.hover`, set it once).
pub fn press(style: gpui::StyleRefinement) -> gpui::StyleRefinement {
    style.transform_scale(PRESS_SCALE)
}

// --- Curves ---

/// How a value moves over time: a spring (as SwiftUI's — its response, the time of one swing, and
/// its damping, 1.0 without overshoot) or an easing over a duration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Curve {
    Spring { response: f32, damping: f32 },
    Ease { duration: Duration, easing: Easing },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Easing {
    Linear,
    OutCubic,
    OutQuint,
    InOutCubic,
}

/// A spring counts as settled when it is this close to its target.
const SETTLED: f32 = 0.002;

impl Curve {
    pub const fn spring(response: f32, damping: f32) -> Self {
        Curve::Spring { response, damping }
    }

    pub const fn eased(duration: Duration, easing: Easing) -> Self {
        Curve::Ease { duration, easing }
    }

    /// How long the curve runs: a spring until it has settled.
    pub fn duration(self) -> Duration {
        self.natural_duration().mul_f32(slow_motion())
    }

    fn natural_duration(self) -> Duration {
        match self {
            Curve::Spring { response, damping } => {
                let omega = TAU / response.max(0.01);
                let seconds = if damping >= 1. {
                    // (1 + ωt)·e^(−ωt) falls under the threshold at ωt ≈ 8.4.
                    8.4 / omega
                } else {
                    -(SETTLED.ln()) / (damping.max(0.05) * omega)
                };
                Duration::from_secs_f32(seconds)
            }
            Curve::Ease { duration, .. } => duration,
        }
    }

    /// The value at `t`, the share of [`Curve::duration`] gone (0 → 1): from 0 to 1, a spring
    /// overshooting on the way.
    pub fn ease(self, t: f32) -> f32 {
        if t <= 0. {
            return 0.;
        }
        if t >= 1. {
            return 1.;
        }
        match self {
            Curve::Spring { response, damping } => {
                let time = t * self.natural_duration().as_secs_f32();
                let omega = TAU / response.max(0.01);
                if damping >= 1. {
                    1. - (-omega * time).exp() * (1. + omega * time)
                } else {
                    let damped = omega * (1. - damping * damping).sqrt();
                    let decay = (-damping * omega * time).exp();
                    1. - decay
                        * ((damped * time).cos() + damping * omega / damped * (damped * time).sin())
                }
            }
            Curve::Ease { easing, .. } => match easing {
                Easing::Linear => t,
                Easing::OutCubic => ease_out_cubic(t),
                Easing::OutQuint => 1. - (1. - t).powi(5),
                Easing::InOutCubic => {
                    if t < 0.5 {
                        4. * t * t * t
                    } else {
                        1. - (-2. * t + 2.).powi(3) / 2.
                    }
                }
            },
        }
    }

    /// The value `elapsed` after the start, and whether the curve is done.
    pub fn at(self, elapsed: Duration) -> (f32, bool) {
        let duration = self.duration();
        if !enabled() || duration.is_zero() || elapsed >= duration {
            return (1., true);
        }
        (
            self.ease(elapsed.as_secs_f32() / duration.as_secs_f32()),
            false,
        )
    }
}

fn ease_out_cubic(t: f32) -> f32 {
    1. - (1. - t).powi(3)
}

/// `a` → `b` at `t` (which may overshoot 0..1).
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

// --- Driven by a model's timestamps ---

/// How far an animation that started at `since` has come, 0 → 1 (1 when done or off). For things
/// a model keeps the time of — a card that came in, a folder just opened — so a list that
/// recycles its rows doesn't replay it.
pub fn progress(since: Instant, curve: Curve) -> f32 {
    curve.at(since.elapsed()).0
}

/// [`progress`], asking for the next frame while the animation runs — call it in render.
pub fn animate(since: Instant, curve: Curve, window: &Window) -> f32 {
    let (value, done) = curve.at(since.elapsed());
    if !done {
        window.request_animation_frame();
    }
    value
}

/// Whether an animation that started at `since` still runs.
pub fn running(since: Instant, curve: Curve) -> bool {
    !curve.at(since.elapsed()).1
}

/// Something that has left — a closed popover, a dismissed card — kept for its exit: drawn, fading
/// out, taking no input ([`inert`]), until [`Leaving::done`].
pub struct Leaving<T> {
    pub value: T,
    pub since: Instant,
}

impl<T> Leaving<T> {
    pub fn new(value: T) -> Self {
        Self {
            value,
            since: Instant::now(),
        }
    }

    /// How far the exit has come, 0 → 1, asking for the next frame until it ends.
    pub fn progress(&self, window: &Window) -> f32 {
        animate(self.since, EXIT, window)
    }

    /// The exit has ended (or animations are off): drop it.
    pub fn done(&self) -> bool {
        !running(self.since, EXIT)
    }
}

/// `element` as something leaving at `progress` (0 → 1): fading out and shrinking a little
/// toward `origin` (fractions of its size).
pub fn leaving_style<E: Styled>(element: E, progress: f32, origin: (f32, f32)) -> E {
    element
        .opacity(1. - progress)
        .transform_scale(lerp(1., EXIT_SCALE, progress))
        .transform_origin(origin.0, origin.1)
}

/// `element` drawn but taking no mouse: hover, clicks and the wheel go to whatever lies beneath.
pub fn inert(element: impl IntoElement) -> Inert {
    Inert {
        element: Some(element.into_any_element()),
    }
}

pub struct Inert {
    element: Option<AnyElement>,
}

impl IntoElement for Inert {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Inert {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, AnyElement) {
        let mut element = self.element.take().expect("laid out once");
        let layout_id = element.request_layout(window, cx);
        (layout_id, element)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: gpui::Bounds<Pixels>,
        element: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_inert(true, |window| element.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: gpui::Bounds<Pixels>,
        element: &mut AnyElement,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

// --- Appearing ---

/// An element coming into view, once — the first frame its id is drawn (an id drawn again after a
/// frame without it starts over: key it by the thing shown, e.g. the popover's entity). It fades
/// in, grows from a scale around its origin and slides from an offset, on a curve, after a delay.
/// Off: it simply appears.
///
/// Inside a list that recycles its rows (`uniform_list`, `list`), a row scrolled back into view
/// would appear again — drive those from the model's timestamps instead ([`progress`]).
#[derive(Clone, Copy, Debug)]
pub struct Appear {
    curve: Curve,
    delay: Duration,
    scale: f32,
    origin: (f32, f32),
    offset: (f32, f32),
    /// The share of the curve the fade takes: it ends before the movement does.
    fade: f32,
}

impl Appear {
    /// A popover or a menu: grows out of its anchor (`origin`, e.g. `(0., 0.)` under a click at its
    /// top left corner).
    pub fn pop(origin: (f32, f32)) -> Self {
        Self {
            curve: POP,
            delay: Duration::ZERO,
            scale: POP_SCALE,
            origin,
            offset: (0., 0.),
            fade: 0.45,
        }
    }

    /// A dialog or a picker under the title bar: grows a little from its top middle, dropping in
    /// by a few pixels.
    pub fn dialog() -> Self {
        Self {
            curve: POP,
            delay: Duration::ZERO,
            scale: DIALOG_SCALE,
            origin: (0.5, 0.),
            offset: (0., -8.),
            fade: 0.45,
        }
    }

    /// Slides in from an offset (`(SLIDE, 0.)` — from the right) as it fades in.
    pub fn slide(dx: f32, dy: f32) -> Self {
        Self {
            curve: BOUNCE,
            delay: Duration::ZERO,
            scale: 1.,
            origin: (0.5, 0.5),
            offset: (dx, dy),
            fade: 0.4,
        }
    }

    /// Only fades in.
    pub fn fade() -> Self {
        Self {
            curve: FADE,
            delay: Duration::ZERO,
            scale: 1.,
            origin: (0.5, 0.5),
            offset: (0., 0.),
            fade: 1.,
        }
    }

    pub fn curve(mut self, curve: Curve) -> Self {
        self.curve = curve;
        self
    }

    /// Starts after `delay` — for a staggered list (`motion::stagger`).
    pub fn delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    pub fn scale(mut self, scale: f32) -> Self {
        self.scale = scale;
        self
    }

    pub fn origin(mut self, x: f32, y: f32) -> Self {
        self.origin = (x, y);
        self
    }

    /// The look of the element at `value` (the curve's value, 0 → 1).
    pub fn style<E: Styled>(&self, element: E, value: f32) -> E {
        let opacity = (value / self.fade.max(0.01)).clamp(0., 1.);
        let mut element = element.opacity(opacity);
        if self.scale != 1. {
            element = element
                .transform_scale(lerp(self.scale, 1., value))
                .transform_origin(self.origin.0, self.origin.1);
        }
        if self.offset != (0., 0.) {
            let rest = 1. - value;
            element =
                element.transform_translate(px(self.offset.0 * rest), px(self.offset.1 * rest));
        }
        element
    }

    /// `element`, appearing.
    pub fn apply<E>(self, id: impl Into<ElementId>, element: E) -> AnyElement
    where
        E: Styled + IntoElement + 'static,
    {
        if !enabled() {
            return element.into_any_element();
        }
        let duration = self.curve.duration();
        let delay = self.delay.mul_f32(slow_motion());
        let total = delay + duration;
        let (delay, length) = (delay.as_secs_f32(), duration.as_secs_f32());
        element
            .with_animation(id, gpui::Animation::new(total), move |element, t| {
                let elapsed = t * (delay + length) - delay;
                let value = if t >= 1. {
                    1.
                } else if elapsed <= 0. {
                    0.
                } else {
                    self.curve.ease(elapsed / length)
                };
                self.style(element, value)
            })
            .into_any_element()
    }
}

/// The delay of the `index`th item of a list appearing one after another, `step` apart, at most
/// `cap` items' worth (a long list mustn't keep the last rows waiting).
pub fn stagger(index: usize, step: Duration, cap: usize) -> Duration {
    step * index.min(cap) as u32
}

// --- Gliding to a target ---

/// A value a [`Transition`] can glide: numbers, pairs, quads, colors.
pub trait Animatable: Copy + PartialEq + 'static {
    fn lerp(from: Self, to: Self, t: f32) -> Self;
}

impl Animatable for f32 {
    fn lerp(from: Self, to: Self, t: f32) -> Self {
        lerp(from, to, t)
    }
}

impl Animatable for (f32, f32) {
    fn lerp(from: Self, to: Self, t: f32) -> Self {
        (lerp(from.0, to.0, t), lerp(from.1, to.1, t))
    }
}

impl Animatable for [f32; 4] {
    fn lerp(from: Self, to: Self, t: f32) -> Self {
        std::array::from_fn(|i| lerp(from[i], to[i], t))
    }
}

impl Animatable for Hsla {
    /// Mixed as straight-alpha RGBA; a transparent end fades only the other one's alpha.
    fn lerp(from: Self, to: Self, t: f32) -> Self {
        mix(from, to, t)
    }
}

/// `from` and `to` mixed at `t` (0..1): straight alpha, a transparent end taking the other's color.
pub fn mix(from: Hsla, to: Hsla, t: f32) -> Hsla {
    let t = t.clamp(0., 1.);
    if t == 0. {
        return from;
    }
    if t == 1. {
        return to;
    }
    let (mut a, mut b) = (from.to_rgb(), to.to_rgb());
    if a.a == 0. {
        a = gpui::Rgba { a: 0., ..b };
    } else if b.a == 0. {
        b = gpui::Rgba { a: 0., ..a };
    }
    let alpha = lerp(a.a, b.a, t);
    let channel = |x: f32, y: f32| {
        if alpha <= 0. {
            lerp(x, y, t)
        } else {
            ((x * a.a + (y * b.a - x * a.a) * t) / alpha).clamp(0., 1.)
        }
    };
    Hsla::from(gpui::Rgba {
        r: channel(a.r, b.r),
        g: channel(a.g, b.g),
        b: channel(a.b, b.b),
        a: alpha,
    })
}

/// Glides a value to its target whenever the target changes — an implicit animation. The first
/// frame shows the target (or glides from [`TransitionExt::with_transition_from`]'s start);
/// a new target mid-way glides on from where the value is. Off: the value is the target.
pub trait TransitionExt: IntoElement + Sized + 'static {
    /// `animator` builds the element at the current value; `id` keeps the value between frames
    /// (unique among its siblings — a stateful parent's id scopes it).
    fn with_transition<T: Animatable>(
        self,
        id: impl Into<ElementId>,
        target: T,
        curve: Curve,
        animator: impl Fn(Self, T) -> Self + 'static,
    ) -> Transition<Self, T> {
        Transition {
            id: id.into(),
            element: Some(self),
            target,
            initial: None,
            curve,
            animator: Box::new(animator),
        }
    }

    /// [`TransitionExt::with_transition`] that glides from `initial` the first time it is drawn.
    fn with_transition_from<T: Animatable>(
        self,
        id: impl Into<ElementId>,
        initial: T,
        target: T,
        curve: Curve,
        animator: impl Fn(Self, T) -> Self + 'static,
    ) -> Transition<Self, T> {
        Transition {
            id: id.into(),
            element: Some(self),
            target,
            initial: Some(initial),
            curve,
            animator: Box::new(animator),
        }
    }
}

impl<E: IntoElement + 'static> TransitionExt for E {}

pub struct Transition<E, T: Animatable> {
    id: ElementId,
    element: Option<E>,
    target: T,
    initial: Option<T>,
    curve: Curve,
    animator: Box<dyn Fn(E, T) -> E>,
}

struct TransitionState<T> {
    from: T,
    to: T,
    since: Instant,
}

impl<T: Animatable> TransitionState<T> {
    fn value(&self, curve: Curve) -> (T, bool) {
        if self.from == self.to {
            return (self.to, true);
        }
        let (t, done) = curve.at(self.since.elapsed());
        if done {
            (self.to, true)
        } else {
            (T::lerp(self.from, self.to, t), false)
        }
    }
}

impl<E: IntoElement + 'static, T: Animatable> IntoElement for Transition<E, T> {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl<E: IntoElement + 'static, T: Animatable> Element for Transition<E, T> {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, AnyElement) {
        let (target, initial, curve) = (self.target, self.initial, self.curve);
        let value = window.with_element_state(
            global_id.expect("a transition has an id"),
            |state: Option<TransitionState<T>>, window| {
                let mut state = state.unwrap_or_else(|| TransitionState {
                    from: initial.unwrap_or(target),
                    to: target,
                    since: Instant::now(),
                });
                if state.to != target {
                    let (current, _) = state.value(curve);
                    state.from = current;
                    state.to = target;
                    state.since = Instant::now();
                }
                let (value, done) = state.value(curve);
                if !done {
                    window.request_animation_frame();
                }
                (value, state)
            },
        );
        let element = self.element.take().expect("laid out once");
        let mut element = (self.animator)(element, value).into_any_element();
        let layout_id = element.request_layout(window, cx);
        (layout_id, element)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: gpui::Bounds<Pixels>,
        element: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: gpui::Bounds<Pixels>,
        element: &mut AnyElement,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

// --- overlays ---

impl Appear {
    /// [`Appear::apply`] growing out of a window point instead of a fraction of the element's size:
    /// a context menu out of its click, a popover out of the symbol or the line it is about.
    /// Wherever the window's edges shifted or flipped the element, it grows out of that point. Put
    /// it inside the `anchored`: a `deferred` element paints later, outside whatever is around it.
    pub fn apply_from<E>(
        self,
        id: impl Into<ElementId>,
        anchor: gpui::Point<Pixels>,
        element: E,
    ) -> AnyElement
    where
        E: Styled + IntoElement + 'static,
    {
        if !enabled() {
            return element.into_any_element();
        }
        FromPoint {
            id: Some(id.into()),
            anchor,
            element: Some(element),
            look: Look::Appear(self),
        }
        .into_any_element()
    }
}

/// [`leaving_style`] shrinking toward a window point — the one it grew out of
/// ([`Appear::apply_from`]). Inside the `anchored` too, in [`inert`].
pub fn leaving_from<E>(element: E, progress: f32, anchor: gpui::Point<Pixels>) -> AnyElement
where
    E: Styled + IntoElement + 'static,
{
    FromPoint {
        id: None,
        anchor,
        element: Some(element),
        look: Look::Leaving(progress),
    }
    .into_any_element()
}

enum Look {
    Appear(Appear),
    Leaving(f32),
}

/// The element of [`Appear::apply_from`] and [`leaving_from`]: the fade (and an appearing offset)
/// on the element's own style, the scale around the window point when it paints.
pub struct FromPoint<E> {
    id: Option<ElementId>,
    anchor: gpui::Point<Pixels>,
    element: Option<E>,
    look: Look,
}

impl<E: Styled + IntoElement + 'static> IntoElement for FromPoint<E> {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl<E: Styled + IntoElement + 'static> Element for FromPoint<E> {
    /// The element, and its scale.
    type RequestLayoutState = (AnyElement, f32);
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        self.id.clone()
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let element = self.element.take().expect("laid out once");
        let (element, scale) = match self.look {
            Look::Leaving(progress) => (
                element.opacity(1. - progress),
                lerp(1., EXIT_SCALE, progress),
            ),
            Look::Appear(appear) => {
                // When it was first drawn: the state lives while the element is drawn each frame.
                let since = match global_id {
                    Some(global_id) => {
                        window.with_element_state(global_id, |since: Option<Instant>, _| {
                            let since = since.unwrap_or_else(Instant::now);
                            (since, since)
                        })
                    }
                    None => Instant::now(),
                };
                let delay = appear.delay.mul_f32(slow_motion());
                let (value, done) = match since.elapsed().checked_sub(delay) {
                    Some(elapsed) => appear.curve.at(elapsed),
                    None => (0., false),
                };
                if !done {
                    window.request_animation_frame();
                }
                // The fade and the offset as `Appear::style` has them; the scale is painted here.
                let look = Appear {
                    scale: 1.,
                    ..appear
                };
                (look.style(element, value), lerp(appear.scale, 1., value))
            }
        };
        let mut element = element.into_any_element();
        let layout_id = element.request_layout(window, cx);
        (layout_id, (element, scale))
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: gpui::Bounds<Pixels>,
        (element, _): &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: gpui::Bounds<Pixels>,
        (element, scale): &mut Self::RequestLayoutState,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_element_scale(Some(*scale), self.anchor, |window| {
            element.paint(window, cx)
        });
    }
}

// --- lists ---

/// How far the rows of a folder drop into place as they unfold.
pub const UNFOLD_DROP: f32 = 6.;
/// The rows of an unfolding folder come one after another, this far apart, at most
/// [`UNFOLD_CAP`] rows' worth.
const UNFOLD_STEP: Duration = Duration::from_millis(16);
const UNFOLD_CAP: usize = 12;
/// A folder read in the background unfolds when its rows come; later than this, they just appear.
const UNFOLD_WAIT: Duration = Duration::from_millis(1500);

/// A tree's folders opening and closing, kept by the tree's model: a tree is a `uniform_list`, its
/// rows are recycled and can't keep it themselves. The chevron of a folder the user toggled turns
/// ([`SMOOTH`]); the rows a folder the user opened shows unfold — they fade in and drop a few
/// pixels into place, one after another. Keyed by what names a row across refreshes (a path, a
/// row key). What the program opens (a reveal, Expand All) doesn't come here: it simply happens.
pub struct Disclosure<K> {
    turns: Vec<Turn<K>>,
    unfold: Option<Unfold<K>>,
}

struct Turn<K> {
    key: K,
    since: Instant,
    /// How open the chevron was when it started turning (0 closed, 1 open).
    from: f32,
}

struct Unfold<K> {
    key: K,
    opened: Instant,
    /// When the folder's rows came into view, and where they are — `None` until they have (a
    /// folder read in the background shows its rows later).
    rows: Option<(Instant, std::ops::Range<usize>)>,
}

impl<K> Default for Disclosure<K> {
    fn default() -> Self {
        Self {
            turns: Vec::new(),
            unfold: None,
        }
    }
}

impl<K: PartialEq + Clone> Disclosure<K> {
    /// The user opened (`open`) or closed the folder `key`: its chevron turns from where it is now;
    /// an opened folder's rows unfold once the rows show them ([`Disclosure::rows_changed`]).
    pub fn toggled(&mut self, key: K, open: bool) {
        if !enabled() {
            self.turns.clear();
            self.unfold = None;
            return;
        }
        let from = self.openness(&key, !open);
        self.turns
            .retain(|turn| turn.key != key && running(turn.since, SMOOTH));
        let now = Instant::now();
        self.turns.push(Turn {
            key: key.clone(),
            since: now,
            from,
        });
        if open {
            self.unfold = Some(Unfold {
                key,
                opened: now,
                rows: None,
            });
        } else if self.unfold.as_ref().is_some_and(|unfold| unfold.key == key) {
            self.unfold = None;
        }
    }

    /// The rows were rebuilt: `children` says where the rows under a folder are now (indices into
    /// the rows; empty while it shows none — still being read), `None` when it is closed or gone.
    pub fn rows_changed(&mut self, children: impl FnOnce(&K) -> Option<std::ops::Range<usize>>) {
        self.turns.retain(|turn| running(turn.since, SMOOTH));
        let Some(unfold) = &mut self.unfold else {
            return;
        };
        let waiting = unfold.opened.elapsed() < UNFOLD_WAIT.mul_f32(slow_motion());
        match (children(&unfold.key), &mut unfold.rows) {
            (Some(range), Some((_, rows))) if !range.is_empty() => *rows = range,
            (Some(range), None) if !range.is_empty() && waiting => {
                unfold.rows = Some((Instant::now(), range))
            }
            (Some(_), None) if waiting => {}
            _ => self.unfold = None,
        }
        if let Some(Unfold {
            rows: Some((since, range)),
            ..
        }) = &self.unfold
            && !running(
                *since + stagger(range.len(), UNFOLD_STEP, UNFOLD_CAP),
                SMOOTH,
            )
        {
            self.unfold = None;
        }
    }

    /// How open the chevron of folder `key` is, 0 → 1 (`open` is the folder's state), asking for
    /// the next frame while it turns.
    pub fn chevron(&self, key: &K, open: bool, window: &Window) -> f32 {
        let target = if open { 1. } else { 0. };
        match self.turns.iter().find(|turn| turn.key == *key) {
            Some(turn) => lerp(turn.from, target, animate(turn.since, SMOOTH, window)),
            None => target,
        }
    }

    /// How far the row at `index` has unfolded, 0 → 1 (1 for the rows that don't unfold), asking
    /// for the next frame while it does — style the row with [`unfold_style`].
    pub fn unfold(&self, index: usize, window: &Window) -> f32 {
        match &self.unfold {
            Some(Unfold {
                rows: Some((since, range)),
                ..
            }) if range.contains(&index) => {
                let delay = stagger(index - range.start, UNFOLD_STEP, UNFOLD_CAP);
                animate(*since + delay.mul_f32(slow_motion()), SMOOTH, window)
            }
            _ => 1.,
        }
    }

    /// The chevron's openness now, without asking for frames (the start of a new turn).
    fn openness(&self, key: &K, open: bool) -> f32 {
        let target = if open { 1. } else { 0. };
        match self.turns.iter().find(|turn| turn.key == *key) {
            Some(turn) => lerp(turn.from, target, progress(turn.since, SMOOTH)),
            None => target,
        }
    }
}

/// A row at `value` of its unfolding (0 → 1, [`Disclosure::unfold`]): fading in quickly and
/// dropping into place. In place, the row is left as it is.
pub fn unfold_style<E: Styled>(element: E, value: f32) -> E {
    if value >= 1. {
        return element;
    }
    element
        .opacity((value * 1.6).min(1.))
        .transform_translate(px(0.), px(-UNFOLD_DROP * (1. - value)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_start_at_zero_and_end_at_one() {
        for curve in [POP, BOUNCE, SMOOTH, RELEASE, SCROLL, FADE, EXIT] {
            assert_eq!(curve.ease(0.), 0.);
            assert_eq!(curve.ease(1.), 1.);
            // Close to the end, a spring has settled.
            assert!((curve.ease(0.999) - 1.).abs() < 0.01, "{curve:?}");
        }
    }

    #[test]
    fn springs_overshoot_only_when_underdamped() {
        let peak = |curve: Curve| {
            (1..100)
                .map(|i| curve.ease(i as f32 / 100.))
                .fold(0., f32::max)
        };
        assert!(peak(BOUNCE) > 1.03, "a bounce shows");
        assert!(
            peak(POP) > 1.0 && peak(POP) < 1.03,
            "a pop overshoots by a hair"
        );
        assert!(peak(SMOOTH) <= 1.0, "layout never overshoots");
    }

    #[test]
    fn spring_durations_stay_short() {
        for curve in [POP, BOUNCE, SMOOTH, RELEASE] {
            let ms = curve.duration().as_millis();
            assert!((250..=700).contains(&ms), "{curve:?}: {ms} ms");
        }
    }

    #[test]
    fn colors_mix_through_transparent_by_alpha() {
        let white = Hsla::white().opacity(0.06);
        let mid = mix(Hsla::transparent_black(), white, 0.5);
        assert!((mid.a - 0.03).abs() < 0.001);
        // The color is the visible end's, not a gray halfway to black.
        assert!(mid.l > 0.99);
    }

    #[test]
    fn staggering_is_capped() {
        let step = Duration::from_millis(30);
        assert_eq!(stagger(2, step, 8), Duration::from_millis(60));
        assert_eq!(stagger(40, step, 8), Duration::from_millis(240));
    }
}
