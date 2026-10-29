//! The motion of a tab strip (2026-10-29), shared by the tabs of the editor area, of the terminal
//! panel and of the Git window (`crate::motion`):
//!
//! - the highlight under the active tab glides to the next active one, and to its new width;
//! - tabs whose place changed — a tab before them closed, moved or opened, a title grew or shrank
//!   (a terminal following its process, a "same name" detail, a rename) — glide from where they
//!   were drawn into their new places: the places are measured after layout, each frame (FLIP);
//! - a new tab pops in; a closed one leaves as a ghost where it was — fading, taking no input —
//!   while its neighbours glide over it;
//! - a tab dragged over the strip parts the tabs where it would land, and the marker glides there;
//! - a tab scrolled out of view glides into it when it becomes active;
//! - a dropped tab is simply where it landed: the frame after a drop glides nothing.
//!
//! Places are left edges and widths in the strip's content (scrolling moves nothing). Off
//! (`motion::enabled`), the tabs simply are where they are.

use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::Location;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    AnyElement, App, Bounds, ContentMask, Element, ElementId, EntityId, GlobalElementId, Hsla,
    InspectorElementId, IntoElement, LayoutId, Pixels, ScrollHandle, Window, canvas, div, point,
    prelude::*, px, size,
};

use crate::motion::{self, Animatable, TransitionExt};
use crate::theme::UiColors;

/// How far the tabs part where a dragged tab would land.
const ROOM: f32 = 22.;

/// What a closed tab's ghost shows while it fades: its icon and labels as they were (built from
/// plain data — a ghost doesn't keep the tab's item alive). The strip sizes it to the tab's place.
pub type Face = Rc<dyn Fn() -> AnyElement>;

/// Where a dragged tab would land, as the tab drawn next to it sees it: the tabs part before it,
/// or after it (the last tab, for the end of the strip).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Room {
    None,
    Before,
    After,
}

impl Room {
    /// The room the tab at `index` (of `count`) makes for a tab that would land at `drop`.
    pub fn at(index: usize, count: usize, drop: Option<usize>) -> Self {
        match drop {
            Some(drop) if drop == index => Room::Before,
            Some(drop) if drop >= count && index + 1 == count => Room::After,
            _ => Room::None,
        }
    }
}

/// A tab's place in the strip's content, as laid out: left, top, width, height.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Place {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

/// A closed tab fading out where it was.
struct Ghost {
    id: EntityId,
    place: Place,
    face: Face,
    leaving: motion::Leaving<()>,
}

/// A value gliding to its target on [`motion::SMOOTH`]: from, to, since when.
struct Glide<T> {
    from: T,
    to: T,
    since: Instant,
}

impl<T: Animatable> Glide<T> {
    fn new(target: T) -> Self {
        Self {
            from: target,
            to: target,
            since: Instant::now(),
        }
    }

    /// The value now, and whether the glide is over.
    fn value(&self) -> (T, bool) {
        let (t, done) = motion::SMOOTH.at(self.since.elapsed());
        if done || self.from == self.to {
            (self.to, true)
        } else {
            (T::lerp(self.from, self.to, t), false)
        }
    }

    /// Glides on to `target` from where it is (a new target), asking for frames until it is there.
    fn toward(&mut self, target: T, window: &Window) -> T {
        if self.to != target {
            *self = Self {
                from: self.value().0,
                to: target,
                since: Instant::now(),
            };
        }
        let (value, done) = self.value();
        if !done {
            window.request_animation_frame();
        }
        value
    }
}

/// The strip scrolling to a tab: from and to (the scroll offset), since when, and the offset it
/// set last (another one means a scroll by hand, which stops it).
struct ScrollGlide {
    from: Pixels,
    to: Pixels,
    since: Instant,
    last: Pixels,
}

#[derive(Default)]
struct State {
    /// The tabs of the last frame, in order, with the looks of their ghosts.
    tabs: Vec<(EntityId, Face)>,
    /// Where each tab was laid out, measured in the last frame (this one, once measured).
    places: HashMap<EntityId, Place>,
    /// Tabs gliding into new places: the offset they started at, since when.
    moves: HashMap<EntityId, (f32, Instant)>,
    /// Tabs just opened, popping in: since when.
    opened: HashMap<EntityId, Instant>,
    ghosts: Vec<Ghost>,
    /// The highlight: whose, and its glide (left edge, width).
    pill: Option<(EntityId, Glide<(f32, f32)>)>,
    /// The marker of where a dragged tab would land: its glide (its middle).
    marker: Option<Glide<f32>>,
    /// A tab to scroll into view, asked for since the last frame.
    reveal: Option<usize>,
    scrolling: Option<ScrollGlide>,
    /// A drop asked the next frame to take the tabs where they are ([`TabStrip::settle`]).
    settle: bool,
    /// This frame takes the tabs where they are: nothing glides or pops in.
    settling: bool,
}

impl State {
    /// How far a tab is drawn from its place, gliding back to it.
    fn offset(&self, id: EntityId) -> f32 {
        self.moves.get(&id).map_or(0., |(offset, since)| {
            offset * (1. - motion::progress(*since, motion::SMOOTH))
        })
    }

    /// A tab laid out at `bounds` (window pixels, scrolled with the strip): its place, measured; a
    /// changed one starts a glide from where the tab was drawn. Returns how far to draw it from
    /// its place.
    fn measured(&mut self, id: EntityId, bounds: Bounds<Pixels>, scroll: &ScrollHandle) -> f32 {
        let origin = scroll.bounds().origin + scroll.offset();
        let place = Place {
            x: f32::from(bounds.left() - origin.x),
            y: f32::from(bounds.top() - origin.y),
            width: f32::from(bounds.size.width),
            height: f32::from(bounds.size.height),
        };
        if let Some(old) = self.places.insert(id, place)
            && (old.x - place.x).abs() > 0.5
            && motion::enabled()
            && !self.settling
        {
            let offset = old.x + self.offset(id) - place.x;
            self.moves.insert(id, (offset, Instant::now()));
        }
        self.offset(id)
    }
}

/// The motion of one tab strip, kept from frame to frame by the strip's owner; clones share it.
/// The strip is a scrolling row tracked by `scroll`, one element per tab ([`TabStrip::tab`]), the
/// ghosts after the tabs ([`TabStrip::ghosts`]), the highlight behind them
/// ([`TabStrip::highlight`]).
#[derive(Clone)]
pub struct TabStrip {
    state: Rc<RefCell<State>>,
    scroll: ScrollHandle,
}

impl TabStrip {
    pub fn new(scroll: ScrollHandle) -> Self {
        Self {
            state: Default::default(),
            scroll,
        }
    }

    /// Before the tabs are drawn, with the tabs in order and the looks of their ghosts: new tabs
    /// pop in; those gone since the last frame leave as ghosts where they were; a tab asked to be
    /// revealed is scrolled to.
    pub fn prepare(&self, tabs: Vec<(EntityId, Face)>, window: &Window) {
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        let now = Instant::now();
        let on = motion::enabled();
        state.settling = std::mem::take(&mut state.settle);
        if state.settling {
            state.moves.clear();
            state.opened.clear();
            state.pill = None;
            state.marker = None;
        }
        let ids: Vec<EntityId> = tabs.iter().map(|(id, _)| *id).collect();
        for (id, face) in state.tabs.drain(..) {
            if ids.contains(&id) || !on {
                continue;
            }
            if let Some(place) = state.places.get(&id).copied() {
                state.ghosts.push(Ghost {
                    id,
                    place,
                    face,
                    leaving: motion::Leaving::new(()),
                });
            }
        }
        let known: Vec<EntityId> = state.places.keys().copied().collect();
        for id in &ids {
            if on && !state.settling && !known.contains(id) {
                state.opened.entry(*id).or_insert(now);
            }
        }
        state.places.retain(|id, _| ids.contains(id));
        state
            .ghosts
            .retain(|ghost| !ghost.leaving.done() && !ids.contains(&ghost.id));
        state
            .moves
            .retain(|id, (_, since)| ids.contains(id) && motion::running(*since, motion::SMOOTH));
        state
            .opened
            .retain(|id, since| ids.contains(id) && motion::running(*since, motion::POP));
        state.tabs = tabs;
        self.scroll_step(state, window);
        if !state.moves.is_empty() || !state.opened.is_empty() || !state.ghosts.is_empty() {
            window.request_animation_frame();
        }
    }

    /// A tab was dropped on the strip (moved along it, or brought from another): the next frame
    /// shows the tabs where they landed — the hand put them there, nothing glides or pops in.
    pub fn settle(&self) {
        self.state.borrow_mut().settle = true;
    }

    /// The strip is gone (the start screen in its place): it forgets its tabs — the next ones pop
    /// in, the old ones leave no ghosts.
    pub fn clear(&self) {
        let mut state = self.state.borrow_mut();
        if !state.tabs.is_empty() || !state.places.is_empty() || !state.ghosts.is_empty() {
            *state = State::default();
        }
    }

    /// Scrolls the strip to show the tab at `index` (when it becomes active): gliding there on
    /// [`motion::SCROLL`] — at once in the strip's first frame or with animations off.
    pub fn reveal(&self, index: usize) {
        self.state.borrow_mut().reveal = Some(index);
    }

    /// A pending reveal starts its glide (from the last frame's layout); a running one moves on.
    fn scroll_step(&self, state: &mut State, window: &Window) {
        let scroll = &self.scroll;
        if let Some(index) = state.reveal.take() {
            let strip = scroll.bounds();
            match scroll.bounds_for_item(index) {
                // Laid out before (and drawn): glide to it.
                Some(tab) if strip.size.width > px(0.) && motion::enabled() => {
                    let offset = scroll.offset();
                    let target = if tab.left() + offset.x < strip.left() {
                        strip.left() - tab.left()
                    } else if tab.right() + offset.x > strip.right() {
                        strip.right() - tab.right()
                    } else {
                        offset.x
                    };
                    state.scrolling = ((target - offset.x).abs() > px(0.5)).then(|| ScrollGlide {
                        from: offset.x,
                        to: target,
                        since: Instant::now(),
                        last: offset.x,
                    });
                }
                // New, or the strip's first frame: gpui shows it in this frame's layout.
                _ => {
                    state.scrolling = None;
                    scroll.scroll_to_item(index);
                }
            }
        }
        let Some(glide) = &mut state.scrolling else {
            return;
        };
        let offset = scroll.offset();
        if (offset.x - glide.last).abs() > px(0.5) {
            // Scrolled by hand meanwhile: the hand wins.
            state.scrolling = None;
            return;
        }
        let (t, done) = motion::SCROLL.at(glide.since.elapsed());
        let x = if done {
            glide.to
        } else {
            glide.from + (glide.to - glide.from) * t
        };
        scroll.set_offset(point(x, offset.y));
        glide.last = x;
        if done {
            state.scrolling = None;
        } else {
            window.request_animation_frame();
        }
    }

    /// A tab of the strip (one element per tab: the scroll handle counts them): measured after
    /// layout and drawn gliding from where it was; popping in when new; with `room` before or
    /// after it while a dragged tab would land there.
    pub fn tab(&self, id: EntityId, room: Room, tab: impl IntoElement) -> AnyElement {
        let state = self.state.borrow();
        let slot = div()
            .flex_none()
            .when(room == Room::Before, |slot| slot.ml(px(ROOM)))
            .when(room == Room::After, |slot| slot.mr(px(ROOM)))
            .child(tab);
        let slot = match state.opened.get(&id) {
            Some(since) => {
                motion::Appear::pop((0.5, 0.5)).style(slot, motion::progress(*since, motion::POP))
            }
            None => slot,
        };
        Slot {
            id,
            state: self.state.clone(),
            scroll: self.scroll.clone(),
            element: Some(slot.into_any_element()),
        }
        .into_any_element()
    }

    /// The closed tabs fading out where they were, taking no input — after the tabs, in the
    /// scrolling row (absolute: they don't count as its items).
    pub fn ghosts(&self, window: &Window) -> Vec<AnyElement> {
        let state = self.state.borrow();
        state
            .ghosts
            .iter()
            .map(|ghost| {
                let Place {
                    x,
                    y,
                    width,
                    height,
                } = ghost.place;
                let progress = ghost.leaving.progress(window);
                let shell = div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(width))
                    .h(px(height))
                    .overflow_hidden()
                    .child((ghost.face)());
                motion::inert(motion::leaving_style(shell, progress, (0.5, 0.5))).into_any_element()
            })
            .collect()
    }

    /// Behind the tabs (put it before the scrolling row): paints the highlight under the active
    /// tab `active` — gliding to it, or riding along with it while it glides into a new place — and,
    /// while a tab is dragged over the strip, the marker where it would land (`drop`, an insertion
    /// point among the tabs), gliding from slot to slot.
    pub fn highlight(
        &self,
        active: Option<EntityId>,
        drop: Option<usize>,
        radius: f32,
        ui: UiColors,
    ) -> impl IntoElement + use<> {
        let (state, scroll) = (self.state.clone(), self.scroll.clone());
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let mut state = state.borrow_mut();
                let strip = scroll.bounds();
                let origin = strip.origin + scroll.offset();
                let pill = active.and_then(|id| Some((id, *state.places.get(&id)?)));
                let quads = paint_pill(&mut state, pill, window).map(|(x, place, width)| {
                    gpui::quad(
                        Bounds::new(
                            point(origin.x + px(x), origin.y + px(place.y)),
                            size(px(width), px(place.height)),
                        ),
                        px(radius),
                        ui.pressed,
                        px(1.),
                        ui.island_border,
                        gpui::BorderStyle::Solid,
                    )
                });
                let marker =
                    drop.and_then(|drop| marker_place(&state, drop))
                        .map(|(middle, place)| {
                            let middle = state
                                .marker
                                .get_or_insert_with(|| Glide::new(middle))
                                .toward(middle, window);
                            gpui::fill(
                                Bounds::new(
                                    point(origin.x + px(middle - 1.), origin.y + px(place.y + 3.)),
                                    size(px(2.), px((place.height - 6.).max(0.))),
                                ),
                                ui.accent,
                            )
                            .corner_radii(px(1.))
                        });
                if marker.is_none() {
                    state.marker = None;
                }
                window.with_content_mask(Some(ContentMask { bounds: strip }), |window| {
                    if let Some(quad) = quads {
                        window.paint_quad(quad);
                    }
                    if let Some(marker) = marker {
                        window.paint_quad(marker);
                    }
                });
            },
        )
        .absolute()
        .size_full()
    }
}

/// The dot at a tab's right edge (unsaved changes, a terminal's bell): it pops in with a bounce and
/// shrinks away (as the launchpad's marker of an open window); under the pointer of the tab
/// (`group`), it fades out for the "×" ([`close_reveal`]).
pub fn dot(shown: bool, color: Hsla, group: &'static str) -> impl IntoElement {
    let (target, curve) = if shown {
        (1., motion::BOUNCE)
    } else {
        (0., motion::EXIT)
    };
    div()
        .id("dot")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .group_hover(group, |style| style.opacity(0.))
        .child(
            div()
                .size(px(7.))
                .rounded(px(4.))
                .bg(color)
                .with_transition("dot-grown", target, curve, |dot, grown| {
                    dot.opacity((grown * 3.).clamp(0., 1.))
                        .transform_scale(grown.max(0.))
                        .transform_origin(0.5, 0.5)
                }),
        )
}

/// A tab's "×" (filling its box at the tab's right edge): shown when `shown` — fading in and out as
/// that changes — and under the pointer of the tab (`group`), fading in (an id keeps the fade).
pub fn close_reveal(close: impl IntoElement, shown: bool, group: &'static str) -> impl IntoElement {
    div()
        .id("close-reveal")
        .absolute()
        .inset_0()
        .group_hover(group, |style| style.opacity(1.))
        .child(close)
        .with_transition(
            "close-shown",
            if shown { 1. } else { 0. },
            motion::FADE,
            |close, shown| close.opacity(shown),
        )
}

/// The highlight's left edge, the place it is under, and its width — or none without an active
/// tab laid out.
fn paint_pill(
    state: &mut State,
    active: Option<(EntityId, Place)>,
    window: &Window,
) -> Option<(f32, Place, f32)> {
    let Some((id, place)) = active else {
        state.pill = None;
        return None;
    };
    let target = (place.x, place.width);
    let moving = state.offset(id);
    match &mut state.pill {
        // The same tab gliding into a new place: the highlight rides along with it.
        Some((owner, glide)) if *owner == id && moving != 0. => {
            *glide = Glide::new(target);
            Some((target.0 + moving, place, target.1))
        }
        Some((owner, glide)) => {
            *owner = id;
            let (x, width) = glide.toward(target, window);
            Some((x, place, width))
        }
        None => {
            state.pill = Some((id, Glide::new(target)));
            Some((target.0, place, target.1))
        }
    }
}

/// The middle of the room a dragged tab would land in (`drop`, an insertion point), and the place
/// of the tab next to it — as laid out, where the gliding tabs end up.
fn marker_place(state: &State, drop: usize) -> Option<(f32, Place)> {
    let ids: Vec<EntityId> = state.tabs.iter().map(|(id, _)| *id).collect();
    match ids.get(drop) {
        Some(id) => {
            let place = *state.places.get(id)?;
            Some((place.x - ROOM / 2., place))
        }
        None => {
            let place = *state.places.get(ids.last()?)?;
            Some((place.x + place.width + ROOM / 2., place))
        }
    }
}

/// The element of a tab ([`TabStrip::tab`]): measures the tab's place after layout and paints it
/// moved by its glide (paint only: the mouse finds the tab where it is laid out).
struct Slot {
    id: EntityId,
    state: Rc<RefCell<State>>,
    scroll: ScrollHandle,
    element: Option<AnyElement>,
}

impl IntoElement for Slot {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Slot {
    type RequestLayoutState = AnyElement;
    /// How far the tab is drawn from its place.
    type PrepaintState = f32;

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
        bounds: Bounds<Pixels>,
        element: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) -> f32 {
        let offset = self
            .state
            .borrow_mut()
            .measured(self.id, bounds, &self.scroll);
        if offset != 0. {
            window.request_animation_frame();
        }
        element.prepaint(window, cx);
        offset
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        element: &mut AnyElement,
        offset: &mut f32,
        window: &mut Window,
        cx: &mut App,
    ) {
        let translate = point(px(*offset), px(0.));
        window.with_element_transform(None, bounds.origin, Some(translate), |window| {
            element.paint(window, cx)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_room_is_before_the_landing_tab_or_after_the_last() {
        assert_eq!(Room::at(1, 3, Some(1)), Room::Before);
        assert_eq!(Room::at(0, 3, Some(1)), Room::None);
        assert_eq!(Room::at(2, 3, Some(3)), Room::After);
        assert_eq!(Room::at(1, 3, Some(3)), Room::None);
        assert_eq!(Room::at(1, 3, None), Room::None);
    }
}
