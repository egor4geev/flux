//! Notifications: cards in the bottom right corner of the window, above the status bar — the
//! balloons of JetBrains IDEs. An operation's result ("Checked out feature/x"), with actions as
//! links ("Restore", "Resolve…", "Details"): an action is a gpui action dispatched on the window,
//! so whoever handles it (the workspace, mostly) needs no reference to the card.
//!
//! The cards show what the window's [`NotificationCenter`](crate::notification_center) decides:
//! every card is a journal entry with the same id. How long a card stays is its [`CardLife`] — a
//! few seconds (longer with actions), or until closed (errors, a task in progress, sticky groups).
//! At most [`MAX_SHOWN`] are shown: a new card pushes the oldest out. Closing a card with × or
//! running one of its actions tells the center ([`CardEvent`]): the entry becomes read; a card that
//! times out leaves it unread.
//!
//! Motion: a card slides in from the right with a bounce ([`motion::BOUNCE`]); one that goes slides
//! out to the right and fades, drawn without input until its exit ends ([`motion::Leaving`] — the
//! cards keep it meanwhile); the rest of the stack glides to its new place ([`Glide`]). A card
//! changed in place fades its new icon and text in ([`motion::FADE`]; a success's check bounces),
//! the done part of its bar glides to the new fraction ([`motion::SMOOTH`]); an unknown amount of
//! work sweeps across the bar while it goes on. The close button fades in under the pointer.

use std::panic::Location;
use std::time::{Duration, Instant};

use gpui::{
    Action, Animation, AnimationExt, AnyElement, App, Bounds, ClickEvent, Context, Div, ElementId,
    Entity, EventEmitter, FontWeight, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    LayoutId, Pixels, Render, SharedString, Stateful, Task, Window, div, point, prelude::*, px,
    relative,
};

use crate::icons::{IconName, icon};
use crate::motion::{self, Leaving, TransitionExt};
use crate::notification_center::{NotificationGroup, NotificationId};
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, GAP, RADIUS_LG, RADIUS_SM, STATUS_BAR_HEIGHT};

/// Width of a card.
pub const WIDTH: f32 = 360.;
/// How many cards are shown at most.
const MAX_SHOWN: usize = 4;
/// How long a card without actions stays, and one with actions.
pub const SHORT: Duration = Duration::from_secs(6);
pub const LONG: Duration = Duration::from_secs(14);
/// Height of the progress bar of a task in progress.
const PROGRESS_HEIGHT: f32 = 4.;
/// How far a card that goes slides to the right as it fades.
const EXIT_SLIDE: f32 = 2. * motion::SLIDE;
/// A card glides to its new place in the stack only when it has moved by more than this (layout
/// rounding is not a move).
const GLIDE_MIN: f32 = 0.5;
/// The icon of a card that has become a success grows from this scale, with a bounce.
const SUCCESS_SCALE: f32 = 0.5;
/// A card's text that changes again within this long of its last change is a live one (a counter,
/// the file being read): it just changes, without fading in anew each time.
const LIVE_TEXT: Duration = Duration::from_millis(600);
/// An unknown amount of work: a segment of this share of the bar sweeps across it, again and again.
const SWEEP_WIDTH: f32 = 0.35;
const SWEEP: motion::Curve =
    motion::Curve::eased(Duration::from_millis(1300), motion::Easing::InOutCubic);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    Info,
    Success,
    Warning,
    Error,
}

#[allow(dead_code)] // The plugin API (stage 8) and the progress of background tasks use it.
/// A background task's progress: unknown ("Indexing…") or a fraction of the work done.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Progress {
    Indeterminate,
    /// 0.0 to 1.0.
    Fraction(f32),
}

/// A notification: a title, an optional body, links that dispatch actions.
pub struct Notification {
    pub kind: NotificationKind,
    pub title: SharedString,
    pub body: Option<SharedString>,
    pub actions: Vec<(SharedString, Box<dyn Action>)>,
    /// Stays until closed (errors are sticky by default; a warning about something the window
    /// shows anyway — conflicts in their dialog — goes away like the rest).
    pub sticky: bool,
    /// Who tells: the group's display setting decides where the notification shows.
    pub group: NotificationGroup,
    /// A task in progress: its card stays until the progress is cleared (`None`).
    pub progress: Option<Progress>,
}

impl Notification {
    pub fn new(kind: NotificationKind, title: impl Into<SharedString>) -> Self {
        Self {
            kind,
            title: title.into(),
            body: None,
            actions: Vec::new(),
            sticky: kind == NotificationKind::Error,
            group: NotificationGroup::General,
            progress: None,
        }
    }

    pub fn info(title: impl Into<SharedString>) -> Self {
        Self::new(NotificationKind::Info, title)
    }

    pub fn success(title: impl Into<SharedString>) -> Self {
        Self::new(NotificationKind::Success, title)
    }

    pub fn warning(title: impl Into<SharedString>) -> Self {
        Self::new(NotificationKind::Warning, title)
    }

    pub fn error(title: impl Into<SharedString>) -> Self {
        Self::new(NotificationKind::Error, title)
    }

    /// A line or two under the title (plain text; it wraps).
    pub fn body(mut self, body: impl Into<SharedString>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// A link at the bottom of the card: dispatches `action` and closes the card.
    pub fn action(mut self, label: impl Into<SharedString>, action: impl Action) -> Self {
        self.actions.push((label.into(), Box::new(action)));
        self
    }

    pub fn group(mut self, group: NotificationGroup) -> Self {
        self.group = group;
        self
    }

    #[allow(dead_code)] // The plugin API (stage 8) and the progress of background tasks use it.
    /// A task in progress (see [`Progress`]).
    pub fn progress(mut self, progress: Progress) -> Self {
        self.progress = Some(progress);
        self
    }

    /// Goes away by itself.
    pub fn transient(mut self) -> Self {
        self.sticky = false;
        self
    }

    /// Whether one of the card's links runs an action named so ("git::ResolveConflicts").
    pub fn offers(&self, names: &[&str]) -> bool {
        self.actions
            .iter()
            .any(|(_, action)| names.contains(&action.name()))
    }
}

impl Clone for Notification {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind,
            title: self.title.clone(),
            body: self.body.clone(),
            actions: self
                .actions
                .iter()
                .map(|(label, action)| (label.clone(), action.boxed_clone()))
                .collect(),
            sticky: self.sticky,
            group: self.group.clone(),
            progress: self.progress,
        }
    }
}

impl std::fmt::Debug for Notification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notification")
            .field("kind", &self.kind)
            .field("title", &self.title)
            .field("body", &self.body)
            .field("group", &self.group)
            .field("progress", &self.progress)
            .field(
                "actions",
                &self
                    .actions
                    .iter()
                    .map(|(label, action)| (label, action.name()))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// How long a card stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardLife {
    /// Closes by itself after this long.
    Timed(Duration),
    /// Stays until closed (or until its notification changes its life).
    Sticky,
}

/// What the user did to a card; the center marks the entry read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardEvent {
    /// Closed with ×.
    Closed(NotificationId),
    /// One of its actions was run (the card closed).
    ActionRun(NotificationId),
}

struct Shown {
    id: NotificationId,
    notification: Notification,
    /// The group's name over the title (a plugin's cards say whose they are).
    source: Option<SharedString>,
    life: CardLife,
    _timer: Option<Task<()>>,
    /// The card has been on screen: one pushed out before it ever was (a burst of notifications)
    /// has nothing to slide out.
    drawn: bool,
    /// When the card last changed in place: its kind (the icon fades in anew), its text (fades in
    /// anew, unless it is a live one — [`LIVE_TEXT`]).
    kind_changed: Option<Instant>,
    text_changed: Option<Instant>,
    /// When its text last changed, faded or not.
    text_at: Option<Instant>,
}

impl Shown {
    /// The card is about to show `next` in place: what changes fades in anew.
    fn note_changes(&mut self, next: &Notification) {
        let now = Instant::now();
        if next.kind != self.notification.kind {
            self.kind_changed = Some(now);
        }
        if next.title != self.notification.title || next.body != self.notification.body {
            let live = self
                .text_at
                .is_some_and(|at| now.duration_since(at) < LIVE_TEXT);
            self.text_changed = (!live).then_some(now);
            self.text_at = Some(now);
        }
    }
}

/// A place in the stack: a card on screen, or one that has gone and slides out.
enum Slot {
    Shown(Shown),
    Leaving(Leaving<Shown>),
}

impl Slot {
    /// The card on screen, not one on its way out.
    fn shown(&self) -> Option<&Shown> {
        match self {
            Slot::Shown(shown) => Some(shown),
            Slot::Leaving(_) => None,
        }
    }

    fn id(&self) -> NotificationId {
        match self {
            Slot::Shown(shown) => shown.id,
            Slot::Leaving(leaving) => leaving.value.id,
        }
    }
}

/// The cards of a window, newest at the bottom; the ones that have gone stay in their place until
/// their exit ends.
#[derive(Default)]
pub struct Notifications {
    slots: Vec<Slot>,
}

impl EventEmitter<CardEvent> for Notifications {}

impl Notifications {
    pub fn new() -> Self {
        Self::default()
    }

    /// Shows the card of journal entry `id`, or replaces it in place if it is shown.
    pub fn show(
        &mut self,
        id: NotificationId,
        notification: Notification,
        source: Option<SharedString>,
        life: CardLife,
        cx: &mut Context<Self>,
    ) {
        let index = self
            .slots
            .iter()
            .position(|slot| slot.shown().is_some_and(|shown| shown.id == id));
        if let Some(Slot::Shown(shown)) = index.map(|index| &mut self.slots[index]) {
            let restart = shown.life != life;
            shown.note_changes(&notification);
            shown.notification = notification;
            shown.source = source;
            if restart {
                shown.life = life;
                shown._timer = Self::timer(id, life, cx);
            }
        } else {
            // Shown again before its exit ended: it comes back in as a new card.
            self.slots.retain(|slot| slot.id() != id);
            self.slots.push(Slot::Shown(Shown {
                id,
                notification,
                source,
                life,
                _timer: Self::timer(id, life, cx),
                drawn: false,
                kind_changed: None,
                text_changed: None,
                text_at: None,
            }));
            if self.live() > MAX_SHOWN
                && let Some(oldest) = self.slots.iter().position(|slot| slot.shown().is_some())
            {
                self.leave(oldest);
            }
        }
        cx.notify();
    }

    /// How many cards are on screen (not counting the ones on their way out).
    fn live(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.shown().is_some())
            .count()
    }

    /// The card on screen at `index` goes: it slides out in its place (with animations off, it
    /// just goes).
    fn leave(&mut self, index: usize) {
        if let Slot::Shown(mut shown) = self.slots.remove(index)
            && shown.drawn
            && motion::enabled()
        {
            shown._timer = None;
            self.slots.insert(index, Slot::Leaving(Leaving::new(shown)));
        }
    }

    /// The cards on screen `go` says yes to leave; whether any did.
    fn leave_where(&mut self, go: impl Fn(&Shown) -> bool) -> bool {
        let mut any = false;
        let mut index = 0;
        while index < self.slots.len() {
            let before = self.slots.len();
            if self.slots[index].shown().is_some_and(&go) {
                self.leave(index);
                any = true;
            }
            // A card removed outright (animations off, never drawn): the next one takes its index.
            if self.slots.len() == before {
                index += 1;
            }
        }
        any
    }

    fn timer(id: NotificationId, life: CardLife, cx: &mut Context<Self>) -> Option<Task<()>> {
        let CardLife::Timed(delay) = life else {
            return None;
        };
        Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |this, cx| this.dismiss(id, cx)).ok();
        }))
    }

    /// Whether the card of entry `id` is on screen.
    pub fn is_shown(&self, id: NotificationId) -> bool {
        self.slots
            .iter()
            .any(|slot| slot.shown().is_some_and(|shown| shown.id == id))
    }

    /// Closes the card of entry `id` (the entry stays in the journal).
    pub fn dismiss(&mut self, id: NotificationId, cx: &mut Context<Self>) {
        if self.leave_where(|shown| shown.id == id) {
            cx.notify();
        }
    }

    /// Keeps only the cards `keep` says yes to (stale ones go: "Continue Rebase" after the rebase
    /// is over).
    pub fn retain(&mut self, keep: impl Fn(&Notification) -> bool, cx: &mut Context<Self>) {
        if self.leave_where(|shown| !keep(&shown.notification)) {
            cx.notify();
        }
    }

    /// Closes every card (Do Not Disturb turned on).
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if self.leave_where(|_| true) {
            cx.notify();
        }
    }

    /// Nothing to draw: no card on screen, none on its way out.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    fn render_card(&self, shown: &Shown, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let ui = Theme::ui(cx);
        let notification = &shown.notification;
        let id = shown.id;
        let (glyph, color) = kind_icon(notification.kind, ui);
        // Changed in place: the new icon and the new text fade in; a check of a task that has
        // succeeded grows in with a bounce.
        let fade_in = |since: Option<Instant>| {
            since.map_or(1., |since| motion::animate(since, motion::FADE, window))
        };
        let (icon_in, text_in) = (fade_in(shown.kind_changed), fade_in(shown.text_changed));
        let pop = shown
            .kind_changed
            .filter(|since| {
                notification.kind == NotificationKind::Success
                    && motion::running(*since, motion::BOUNCE)
            })
            .map(|since| motion::animate(since, motion::BOUNCE, window));
        let actions: Vec<_> = notification
            .actions
            .iter()
            .enumerate()
            .map(|(index, (label, action))| {
                let action = action.boxed_clone();
                div()
                    .id(("notification-action", id as usize * 16 + index))
                    .px_1()
                    .py_0p5()
                    .rounded(px(RADIUS_SM))
                    .cursor_pointer()
                    .text_color(ui.accent_text)
                    .hover(move |style| style.bg(ui.hover))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.dismiss(id, cx);
                        cx.emit(CardEvent::ActionRun(id));
                        window.dispatch_action(action.boxed_clone(), cx);
                    }))
                    .child(label.clone())
            })
            .collect();
        let group = format!("notification-{id}");
        div()
            .id(("notification", id as usize))
            .group(group.clone())
            .relative()
            .w(px(WIDTH))
            .p_3()
            .flex()
            .gap_2p5()
            .rounded(px(RADIUS_LG))
            .bg(ui.elevated)
            .border_1()
            .border_color(ui.elevated_border)
            .shadow(ui::popover_shadow(ui))
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_MD))
            .text_color(ui.foreground)
            // Clicks on a card stay on it (the editor under it doesn't take focus).
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(ui::sheen(ui, RADIUS_LG))
            .child(
                div()
                    .flex_none()
                    .pt(px(1.))
                    .when(icon_in < 1., |icon| icon.opacity(icon_in))
                    .when_some(pop, |icon, value| {
                        icon.transform_scale(motion::lerp(SUCCESS_SCALE, 1., value))
                            .transform_origin(0.5, 0.5)
                    })
                    .child(icon(glyph, color).size(px(16.))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(shown.source.clone().map(|source| {
                        div()
                            .pr_5()
                            .text_size(px(theme::TEXT_XS))
                            .text_color(ui.dim)
                            .truncate()
                            .child(source)
                    }))
                    .child(
                        div()
                            .pr_5()
                            .font_weight(FontWeight::SEMIBOLD)
                            .when(text_in < 1., |title| title.opacity(text_in))
                            .child(notification.title.clone()),
                    )
                    .children(notification.body.clone().map(|body| {
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.text_muted)
                            .when(text_in < 1., |body| body.opacity(text_in))
                            .child(body)
                    }))
                    .children(notification.progress.map(|progress| {
                        // The card's id scopes the fraction's glide.
                        bar(progress, Some("notification-progress".into()), ui)
                    }))
                    .when(!actions.is_empty(), |column| {
                        column.child(
                            div()
                                .pt_1()
                                .flex()
                                .flex_wrap()
                                .gap_2()
                                .ml(px(-4.))
                                .text_size(px(theme::TEXT_SM))
                                .children(actions),
                        )
                    }),
            )
            // Fades in under the pointer (an id keeps the fade); invisible, it still closes the
            // card.
            .child(
                div()
                    .id("notification-close-reveal")
                    .absolute()
                    .top(px(6.))
                    .right(px(6.))
                    .opacity(0.)
                    .group_hover(group, |style| style.opacity(1.))
                    .child(
                        ui::icon_button(("notification-close", id as usize), IconName::Close, ui)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.dismiss(id, cx);
                                cx.emit(CardEvent::Closed(id));
                            })),
                    ),
            )
    }
}

/// A thin bar: the done part in the accent color; an unknown amount is a segment sweeping across
/// the bar, again and again while it is drawn (with animations off, the whole bar, dimmer).
pub fn progress_bar(progress: Progress, ui: UiColors) -> impl IntoElement + use<> {
    bar(progress, None, ui)
}

/// [`progress_bar`]; with `glide`, the done part glides to each new fraction ([`motion::SMOOTH`]),
/// `glide` keeping the fraction between frames (unique among the bar's siblings).
fn bar(progress: Progress, glide: Option<ElementId>, ui: UiColors) -> Div {
    let track = div()
        .mt_1()
        .w_full()
        .h(px(PROGRESS_HEIGHT))
        .rounded(px(PROGRESS_HEIGHT / 2.))
        .overflow_hidden()
        .bg(ui.input_border);
    let part = || div().h_full().rounded(px(PROGRESS_HEIGHT / 2.));
    match progress {
        // A repeating animation: it ticks on the shared 30 fps grid, and only while the bar is
        // drawn — while the work goes on.
        Progress::Indeterminate if motion::enabled() => track.relative().child(
            part()
                .absolute()
                .top_0()
                .w(relative(SWEEP_WIDTH))
                .bg(ui.accent)
                .with_animation(
                    "progress-sweep",
                    Animation::new(SWEEP.duration()).repeat(),
                    |segment, t| {
                        segment.left(relative(motion::lerp(-SWEEP_WIDTH, 1., SWEEP.ease(t))))
                    },
                ),
        ),
        Progress::Indeterminate => track.child(part().w_full().bg(UiColors::tint(ui.accent, 0.45))),
        Progress::Fraction(done) => {
            // Not a number (a plugin's 0 / 0) is nothing done: a target that never equals itself
            // would glide forever.
            let done = if done.is_nan() {
                0.
            } else {
                done.clamp(0., 1.)
            };
            let fill = part().bg(ui.accent);
            match glide {
                Some(id) => track.child(fill.with_transition(
                    id,
                    done,
                    motion::SMOOTH,
                    |fill, done| fill.w(relative(done)),
                )),
                None => track.child(fill.w(relative(done))),
            }
        }
    }
}

pub fn kind_icon(kind: NotificationKind, ui: UiColors) -> (IconName, Hsla) {
    match kind {
        NotificationKind::Info => (IconName::Info, ui.info),
        NotificationKind::Success => (IconName::CheckCircle, ui.success),
        NotificationKind::Warning => (IconName::Warning, ui.warning),
        NotificationKind::Error => (IconName::Error, ui.error),
    }
}

impl Render for Notifications {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.slots.retain(|slot| match slot {
            Slot::Shown(_) => true,
            Slot::Leaving(leaving) => !leaving.done(),
        });
        for slot in &mut self.slots {
            if let Slot::Shown(shown) = slot {
                shown.drawn = true;
            }
        }
        let cards: Vec<_> = self
            .slots
            .iter()
            .map(|slot| {
                let id = slot.id() as usize;
                let card = match slot {
                    // In from the right, with a bounce — once: the card keeps its id.
                    Slot::Shown(shown) => motion::Appear::slide(motion::SLIDE, 0.)
                        .apply(("notification-in", id), self.render_card(shown, window, cx)),
                    // Out to the right, fading, taking no input.
                    Slot::Leaving(leaving) => {
                        let progress = leaving.progress(window);
                        let card = self
                            .render_card(&leaving.value, window, cx)
                            .opacity(1. - progress)
                            .transform_translate(px(EXIT_SLIDE * progress), px(0.));
                        motion::inert(card).into_any_element()
                    }
                };
                Glide::new(("notification-glide", id), card).into_any_element()
            })
            .collect();
        div().flex().flex_col().gap_2().children(cards)
    }
}

/// A card gliding to the place the layout gives it (FLIP): when the stack changes under it — a
/// card comes in at the bottom, one has gone — it moves from where it was drawn to its new place
/// ([`motion::SMOOTH`]) instead of jumping there. Its place is measured from the window's bottom
/// edge, which the stack hangs from: resizing the window moves nothing. The shift is applied when
/// the card is placed, so the mouse finds it where it is drawn.
struct Glide {
    id: ElementId,
    child: Option<AnyElement>,
}

/// What a [`Glide`] keeps between frames.
struct GlideState {
    /// The card's distance from the window's bottom edge in the last frame.
    rise: Pixels,
    /// The shift it glides away from (down is positive), and since when.
    from: f32,
    since: Instant,
}

impl GlideState {
    /// The shift now, and whether the glide is over.
    fn shift(&self) -> (f32, bool) {
        if self.from == 0. {
            return (0., true);
        }
        let (t, done) = motion::SMOOTH.at(self.since.elapsed());
        (self.from * (1. - t), done)
    }
}

impl Glide {
    fn new(id: impl Into<ElementId>, child: impl IntoElement) -> Self {
        Self {
            id: id.into(),
            child: Some(child.into_any_element()),
        }
    }
}

impl IntoElement for Glide {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Glide {
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
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, AnyElement) {
        let mut child = self.child.take().expect("laid out once");
        let layout_id = child.request_layout(window, cx);
        (layout_id, child)
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        child: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        let rise = window.viewport_size().height - bounds.bottom();
        let shift = window.with_element_state(
            global_id.expect("a glide has an id"),
            |state: Option<GlideState>, window| {
                let mut state = state.unwrap_or(GlideState {
                    rise,
                    from: 0.,
                    since: Instant::now(),
                });
                let moved = f32::from(rise - state.rise);
                if moved.abs() > GLIDE_MIN {
                    // Raised by `moved`: drawn that much lower at first, where it was.
                    let (current, _) = state.shift();
                    state.from = if motion::enabled() {
                        current + moved
                    } else {
                        0.
                    };
                    state.since = Instant::now();
                    state.rise = rise;
                }
                let (shift, done) = state.shift();
                if done {
                    state.from = 0.;
                } else {
                    window.request_animation_frame();
                }
                (shift, state)
            },
        );
        window.with_element_offset(point(px(0.), px(shift)), |window| {
            child.prepaint(window, cx)
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        child: &mut AnyElement,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        child.paint(window, cx);
    }
}

/// The cards over the window: in the bottom right corner, above the status bar.
pub fn overlay(
    notifications: &Entity<Notifications>,
    cx: &App,
) -> Option<impl IntoElement + use<>> {
    if notifications.read(cx).is_empty() {
        return None;
    }
    Some(
        div()
            .absolute()
            .bottom(px(STATUS_BAR_HEIGHT + GAP))
            .right(px(GAP + 4.))
            .child(notifications.clone()),
    )
}
