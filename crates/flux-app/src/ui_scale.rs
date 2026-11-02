//! The interface scale (part 10.4): the whole window larger or smaller — the text of the code, the
//! panels, the icons, the popups — as Zoom IDE of JetBrains IDEs (View → Appearance → Zoom IDE:
//! ⌃⌥= / ⌃⌥- / ⌃⌥0) and Settings → Appearance → Interface. The scale is one for every window and
//! is saved in `settings.json` (`appearance.ui_scale`).
//!
//! gpui 0.2.2 has no such scale, and the interface is laid out in pixels (`px`), so Flux carries a
//! patch of gpui (`vendor/gpui`, FLUX-PATCH.md): the window lays out a viewport smaller by the scale
//! and draws it with the display's scale factor times it; mouse positions come back divided by it.

use gpui::{App, KeyBinding, Pixels, Point, SharedString, Window, actions, point, px};

use crate::i18n::trf;
use crate::settings;
use crate::ui;
use crate::workspace::{TRAFFIC_LIGHTS_WIDTH, Workspace};

actions!(
    interface,
    [
        /// The interface one step larger (⌃⌥=).
        ZoomIn,
        /// The interface one step smaller (⌃⌥-).
        ZoomOut,
        /// The interface at 100 % (⌃⌥0).
        ResetZoom
    ]
);

/// The scales offered: in Settings and as the steps of Zoom In / Zoom Out.
pub const STEPS: &[f32] = &[0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0];
pub const MIN: f32 = 0.8;
pub const MAX: f32 = 2.0;
/// Scales closer than this are the same step (a scale read back from JSON isn't exact).
const SAME_STEP: f32 = 0.001;

/// The macOS window buttons (close, minimize, zoom) keep their size at any scale: 14 pt.
const TRAFFIC_LIGHT_SIZE: f32 = 14.;
/// The close button's left edge at 100 %.
const TRAFFIC_LIGHTS_LEFT: f32 = 16.;

pub fn init(cx: &mut App) {
    // No context: the keys work wherever focus is — the editor, a terminal (bindings come before
    // the keys the terminal sends to its program), a dialog.
    crate::keymap::bind_keys(
        cx,
        [
            KeyBinding::new("ctrl-alt-=", ZoomIn, None),
            KeyBinding::new("ctrl-alt--", ZoomOut, None),
            KeyBinding::new("ctrl-alt-0", ResetZoom, None),
        ],
    );
    // Global actions: they are also in the command palette, whatever has focus.
    cx.on_action(|_: &ZoomIn, cx| zoom(next_step(settings::ui_scale(cx)), cx));
    cx.on_action(|_: &ZoomOut, cx| zoom(previous_step(settings::ui_scale(cx)), cx));
    cx.on_action(|_: &ResetZoom, cx| zoom(1., cx));
}

/// A scale from the settings file, kept within the limits.
pub fn clamp(scale: f32) -> f32 {
    scale.clamp(MIN, MAX)
}

/// "110%" ("110 %" in Russian) — the scale as the settings and the status bar show it.
pub fn label(scale: f32) -> String {
    trf("{0}%", &[&((scale * 100.).round() as i32)])
}

/// The step after `scale` (Zoom In); at the largest, the largest.
pub fn next_step(scale: f32) -> f32 {
    STEPS
        .iter()
        .copied()
        .find(|step| *step > scale + SAME_STEP)
        .unwrap_or(MAX)
}

/// The step before `scale` (Zoom Out); at the smallest, the smallest.
pub fn previous_step(scale: f32) -> f32 {
    STEPS
        .iter()
        .rev()
        .copied()
        .find(|step| *step < scale - SAME_STEP)
        .unwrap_or(MIN)
}

/// Applies the saved scale to a window Flux is opening, before its first frame.
pub fn apply(window: &mut Window, cx: &mut App) {
    scale_window(window, settings::ui_scale(cx), cx);
}

/// Sets the scale of every Flux window and saves it.
pub fn set(scale: f32, cx: &mut App) {
    let scale = clamp(scale);
    if (settings::ui_scale(cx) - scale).abs() >= SAME_STEP {
        settings::update_appearance(cx, |appearance| appearance.ui_scale = scale);
    }
    // The window this comes from (a key, the palette, the Settings page) is being updated: every
    // window is scaled once the current update is over.
    cx.defer(move |cx| {
        for handle in cx.windows() {
            handle
                .update(cx, |_, window, cx| scale_window(window, scale, cx))
                .ok();
        }
    });
}

/// Where the macOS traffic lights go at `scale`: the close button's top-left corner in the window's
/// points. The buttons keep their size while the title bar scales, so they are placed anew: on the
/// middle of the title bar, at the same distance from the middle of the room the title bar leaves
/// for them. Above ~105 % the middle is lower than the system's own title bar, out of which macOS
/// doesn't take clicks on them: gpui keeps them inside it, a little above the middle.
pub fn traffic_lights(scale: f32) -> Point<Pixels> {
    point(
        px(TRAFFIC_LIGHTS_LEFT + TRAFFIC_LIGHTS_WIDTH * (scale - 1.) / 2.),
        px(ui::TITLE_BAR_HEIGHT * scale / 2. - TRAFFIC_LIGHT_SIZE / 2.),
    )
}

fn scale_window(window: &mut Window, scale: f32, cx: &mut App) {
    window.set_ui_scale(scale, cx);
    window.set_traffic_light_position(Some(traffic_lights(scale)));
}

/// Zoom In, Zoom Out, Reset Zoom: the scale everywhere and a word about it in the status bar of the
/// window in front.
fn zoom(scale: f32, cx: &mut App) {
    let scale = clamp(scale);
    set(scale, cx);
    // A scenario's window never comes in front: the last one opened.
    let front = cx
        .active_window()
        .or_else(|| cx.windows().last().copied())
        .and_then(|handle| handle.downcast::<Workspace>());
    let message: SharedString = trf("Zoom: {0}", &[&label(scale)]).into();
    cx.defer(move |cx| {
        if let Some(handle) = front {
            handle
                .update(cx, |workspace, _, cx| workspace.show_message(message, cx))
                .ok();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_go_up_and_down_and_stop_at_the_ends() {
        assert_eq!(next_step(1.), 1.1);
        assert_eq!(next_step(1.1), 1.25);
        assert_eq!(previous_step(1.25), 1.1);
        assert_eq!(previous_step(1.), 0.9);
        assert_eq!(next_step(MAX), MAX);
        assert_eq!(previous_step(MIN), MIN);
        // A scale between the steps goes to the nearest one in that direction; one read back from
        // JSON (1.100000023841858) is that step.
        assert_eq!(next_step(1.2), 1.25);
        assert_eq!(previous_step(1.2), 1.1);
        assert_eq!(next_step(1.1_f64 as f32), 1.25);
        assert_eq!(previous_step(1.1_f64 as f32), 1.);
    }

    #[test]
    fn every_step_is_within_the_limits() {
        assert_eq!(STEPS.first(), Some(&MIN));
        assert_eq!(STEPS.last(), Some(&MAX));
        assert!(STEPS.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(clamp(0.5), MIN);
        assert_eq!(clamp(3.), MAX);
        assert_eq!(clamp(1.25), 1.25);
    }

    #[test]
    fn labels_are_whole_percents() {
        assert_eq!(label(1.), "100%");
        assert_eq!(label(1.1_f64 as f32), "110%");
        assert_eq!(label(1.25), "125%");
        assert_eq!(label(0.8), "80%");
    }

    #[test]
    fn traffic_lights_stay_centered_in_the_scaled_title_bar() {
        // 100 %: where the window was created before the scale existed.
        assert_eq!(traffic_lights(1.), point(px(16.), px(13.)));
        // The middle of the close button is the middle of the title bar.
        for scale in STEPS {
            let lights = traffic_lights(*scale);
            let middle = f32::from(lights.y) + TRAFFIC_LIGHT_SIZE / 2.;
            assert!((middle - ui::TITLE_BAR_HEIGHT * scale / 2.).abs() < 0.01);
        }
        // The group (54 pt) keeps its distance from the middle of its room.
        let room_middle = |scale: f32| TRAFFIC_LIGHTS_WIDTH * scale / 2.;
        let group_middle = |scale: f32| f32::from(traffic_lights(scale).x) + 27.;
        for scale in STEPS {
            let offset = group_middle(*scale) - room_middle(*scale);
            assert!((offset - (group_middle(1.) - room_middle(1.))).abs() < 0.01);
        }
        // At 80 % the group still ends before the project chip.
        assert!(f32::from(traffic_lights(0.8).x) + 54. < TRAFFIC_LIGHTS_WIDTH * 0.8);
    }
}
