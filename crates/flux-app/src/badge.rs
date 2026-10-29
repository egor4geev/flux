//! Counts that bump (Motion, 2026-10-29): the bell's unread count, the launchpad's badges, the
//! branch's ↑/↓ in the title bar. When a count grows, its number starts [`BUMP`] larger and
//! springs back ([`motion::BOUNCE`]) — a glance tells something came. A count that shrinks or
//! stays only changes its digits; the first frame takes the count as it is.
//!
//! [`Bump`] keeps the count of a view that is drawn anyway (the bell, whose icon wobbles with
//! it); [`bump`] wraps an element and keeps it in the element's state, keyed by its id. A count
//! that hides at zero keeps its wrapper drawn (an empty element), so its coming back bumps.

use std::panic::Location;
use std::time::Instant;

use gpui::{
    AnyElement, App, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Styled, Window,
};

use crate::motion::{self, Curve};

/// A count that grew starts this much larger (`1 + BUMP`) and springs back.
pub const BUMP: f32 = 0.45;

/// A count as last drawn, and when it last grew.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bump {
    /// `None` before the first frame: it takes the count as it is, without a bump.
    seen: Option<usize>,
    since: Option<Instant>,
}

impl Bump {
    /// Takes the count drawn now: when it grew since the last frame, a bump starts (animations
    /// on). Returns when the running bump started; it runs while [`motion::BOUNCE`] or `lasts` (a
    /// curve the caller drives from it as well, as the bell's wobble) does.
    pub fn track(&mut self, count: usize, lasts: Curve) -> Option<Instant> {
        let grew = self.seen.is_some_and(|seen| count > seen);
        self.since = match self.since {
            _ if grew && motion::enabled() => Some(Instant::now()),
            Some(since)
                if motion::running(since, motion::BOUNCE) || motion::running(since, lasts) =>
            {
                Some(since)
            }
            _ => None,
        };
        self.seen = Some(count);
        self.since
    }
}

/// How far the bump that started at `since` has come (`motion::BOUNCE`, 0 → 1, overshooting),
/// asking for the next frame while it runs; 1 when there is none.
pub fn progress(since: Option<Instant>, window: &Window) -> f32 {
    since.map_or(1., |since| motion::animate(since, motion::BOUNCE, window))
}

/// A count at `value` of its bump ([`progress`]): larger, springing back to its size around its
/// middle. As the bell always had it, the spring's overshoot doesn't shrink the count below its
/// size.
pub fn bump_style<E: Styled>(element: E, value: f32) -> E {
    if value >= 1. {
        return element;
    }
    element
        .transform_scale(1. + BUMP * (1. - value))
        .transform_origin(0.5, 0.5)
}

/// `element` showing `count`, bumping whenever the count grows. `id` keeps the count between
/// frames (unique among its siblings; a stateful parent's id scopes it).
pub fn bump<E>(id: impl Into<ElementId>, count: usize, element: E) -> Bumped<E>
where
    E: Styled + IntoElement + 'static,
{
    Bumped {
        id: id.into(),
        count,
        element: Some(element),
    }
}

/// The element of [`bump`].
pub struct Bumped<E> {
    id: ElementId,
    count: usize,
    element: Option<E>,
}

impl<E: Styled + IntoElement + 'static> IntoElement for Bumped<E> {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl<E: Styled + IntoElement + 'static> Element for Bumped<E> {
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
        let count = self.count;
        let since = window.with_element_state(
            global_id.expect("a bump has an id"),
            |state: Option<Bump>, _| {
                let mut state = state.unwrap_or_default();
                (state.track(count, motion::BOUNCE), state)
            },
        );
        let element = self.element.take().expect("laid out once");
        let mut element = bump_style(element, progress(since, window)).into_any_element();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_count_and_a_smaller_one_dont_bump() {
        let mut bump = Bump::default();
        assert!(bump.track(3, motion::BOUNCE).is_none(), "the first frame");
        assert!(bump.track(3, motion::BOUNCE).is_none(), "the same count");
        assert!(bump.track(1, motion::BOUNCE).is_none(), "fewer");
    }

    #[test]
    fn a_count_that_grew_bumps_until_the_spring_settles() {
        let mut bump = Bump::default();
        bump.track(0, motion::BOUNCE);
        let since = bump.track(2, motion::BOUNCE);
        // Motion may be off where the tests run (macOS's "Reduce motion"): then nothing bumps.
        if motion::enabled() {
            assert!(since.is_some());
            assert_eq!(
                bump.track(2, motion::BOUNCE),
                since,
                "the same bump goes on"
            );
        } else {
            assert!(since.is_none());
        }
    }
}
