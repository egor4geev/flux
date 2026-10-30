//! The parts of Flux's title bar that differ between macOS and Windows (part 10.9). Flux draws its
//! own title bar on a window whose system title bar is transparent (macOS) or absent (Windows).
//!
//! - **macOS**: the system's traffic lights sit at the left, in the room [`leading`] leaves for
//!   them (`ui_scale::traffic_lights` places them at the interface scale); the system's
//!   transparent title bar moves the window, and a double click acts as on any Mac title bar.
//! - **Windows**: the title bar is the window's caption. A menu button at the left opens the menu
//!   bar's menus in Flux's own popup (`app_menu::popup`); the window buttons — minimize, maximize
//!   or restore, close — sit at the right end, as Windows 11 draws them ([`window_buttons`]).
//!
//! On Windows gpui answers the system's question which part of the window is under the pointer
//! (`WM_NCHITTEST`, `handle_hit_test_msg` in gpui's `platform/windows/events.rs`) with the first
//! element marked with a `WindowControlArea` among those under the pointer — down to the first one
//! that blocks the mouse. `Drag` is the caption: Windows moves the window, maximizes or restores it
//! on a double click and shows the system menu on a right click — so Flux's own double click is
//! left out there. `Min`, `Max` and `Close` are the window buttons: Windows minimizes, maximizes
//! and closes as for its own buttons, and Windows 11 shows the snap layouts over `Max`. So the
//! caption blocks the mouse for what is beneath it ([`caption`]: a focusable root would otherwise
//! take the press and keep the window from moving), and everything clickable on it blocks the
//! mouse as well ([`clickable`]): it is then the window's client area, with its own clicks,
//! pointer and hover. On macOS none of this applies and nothing is marked.
//!
//! Which title bar is drawn is a value, [`KeyOs`] (as for the keys), so the layouts are tested on a
//! Mac too, and a UI scenario shows the Windows one on a Mac ([`os`]).

use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, Styled, WindowControlArea,
    div, px, rgb, white,
};

use crate::icons::{IconName, icon};
use crate::keymap::KeyOs;
use crate::theme::UiColors;
use crate::ui::GAP;
use crate::workspace::TRAFFIC_LIGHTS_WIDTH;

/// A window button's width at 100 %, as Windows 11 draws its own.
pub const WINDOW_BUTTON_WIDTH: f32 = 46.;
/// The glyph of a window button: an icon whose drawing is Windows' 10 × 10 glyph in the middle.
const WINDOW_GLYPH_SIZE: f32 = 16.;
/// The close button under the pointer: the system's red, the same in every theme, as on every
/// Windows 11 window — not a color of Flux's themes.
const CLOSE_HOVER: u32 = 0xC42B1C;
/// The menu button's distance from the window's left edge on Windows.
pub const MENU_BUTTON_MARGIN: f32 = 6.;

/// Whose title bar Flux draws: the system's it runs on — or Windows', which a UI scenario can show
/// on a Mac (`FLUX_SCENARIO_TITLE_BAR=windows`).
pub fn os() -> KeyOs {
    #[cfg(feature = "scenario")]
    if crate::scenario::windows_title_bar() {
        return KeyOs::Windows;
    }
    KeyOs::current()
}

/// The room at the left of the title bar, before the project: the traffic lights' on macOS (none in
/// full screen, where macOS hides them), the menu button's margin on Windows.
pub fn leading(os: KeyOs, fullscreen: bool) -> f32 {
    match os {
        KeyOs::Mac if fullscreen => GAP + 4.,
        KeyOs::Mac => TRAFFIC_LIGHTS_WIDTH,
        KeyOs::Windows => MENU_BUTTON_MARGIN,
    }
}

/// The room at the right end of the title bar: none on Windows, where the window buttons reach the
/// window's edge (and in full screen, without them, the same as on macOS).
pub fn trailing(os: KeyOs, fullscreen: bool) -> f32 {
    match os {
        KeyOs::Windows if !fullscreen => 0.,
        _ => GAP + 2.,
    }
}

/// A window button on Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowButton {
    Minimize,
    Maximize,
    /// Maximize of a maximized window: it comes back to its size.
    Restore,
    Close,
}

impl WindowButton {
    /// What Windows does with a press on it.
    fn area(self) -> WindowControlArea {
        match self {
            Self::Minimize => WindowControlArea::Min,
            Self::Maximize | Self::Restore => WindowControlArea::Max,
            Self::Close => WindowControlArea::Close,
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Minimize => IconName::WindowMinimize,
            Self::Maximize => IconName::WindowMaximize,
            Self::Restore => IconName::WindowRestore,
            Self::Close => IconName::WindowClose,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Minimize => "window-minimize",
            Self::Maximize => "window-maximize",
            Self::Restore => "window-restore",
            Self::Close => "window-close",
        }
    }
}

/// The window buttons at the right end of the title bar: none on macOS (the system's traffic
/// lights) and in full screen; on Windows minimize, maximize — restore once maximized — for a
/// window that can be resized (Windows gives the others no maximize), and close.
pub fn window_buttons(
    os: KeyOs,
    fullscreen: bool,
    resizable: bool,
    maximized: bool,
) -> Vec<WindowButton> {
    if os != KeyOs::Windows || fullscreen {
        return Vec::new();
    }
    let mut buttons = vec![WindowButton::Minimize];
    if resizable {
        buttons.push(if maximized {
            WindowButton::Restore
        } else {
            WindowButton::Maximize
        });
    }
    buttons.push(WindowButton::Close);
    buttons
}

/// The window buttons as Windows 11 draws them: the title bar's full height, 46 px wide (at the
/// interface scale), a 10 px glyph of 1 px lines in the middle; a subtle hover, and the system's
/// red with a white glyph under the pointer for close. They have no click handlers: gpui hands the
/// press to Windows (`handle_nc_mouse_up_msg`), which minimizes, maximizes, restores and closes —
/// closing goes through the window's `on_window_should_close`, as Alt+F4 does.
pub fn render_window_buttons(
    buttons: Vec<WindowButton>,
    ui: UiColors,
) -> Option<impl IntoElement + use<>> {
    if buttons.is_empty() {
        return None;
    }
    let close_hover: Hsla = rgb(CLOSE_HOVER).into();
    Some(
        div()
            .flex_none()
            .h_full()
            .flex()
            .children(buttons.into_iter().map(move |button| {
                let close = button == WindowButton::Close;
                let group = button.id();
                div()
                    .id(group)
                    .group(group)
                    // The press goes to Windows, not to what is beneath (the module's doc).
                    .occlude()
                    .window_control_area(button.area())
                    .flex_none()
                    .w(px(WINDOW_BUTTON_WIDTH))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(move |style| style.bg(if close { close_hover } else { ui.hover }))
                    .child(
                        icon(button.icon(), ui.text_muted)
                            .size(px(WINDOW_GLYPH_SIZE))
                            .group_hover(group, move |style| {
                                style.text_color(if close { white() } else { ui.foreground })
                            }),
                    )
            })),
    )
}

/// The title bar as the window's caption (the module's doc): on Windows it moves the window and
/// blocks the mouse for what is beneath it. Nothing on macOS, where the system's transparent title
/// bar is the caption.
pub fn caption<E: InteractiveElement>(os: KeyOs, element: E) -> E {
    match os {
        KeyOs::Mac => element,
        KeyOs::Windows => element
            .occlude()
            .window_control_area(WindowControlArea::Drag),
    }
}

/// Something clickable on the caption: on Windows it blocks the mouse, so it is the window's client
/// area — clicked, with its pointer, instead of moving the window. As it is on macOS.
pub fn clickable<E: InteractiveElement>(os: KeyOs, element: E) -> E {
    match os {
        KeyOs::Mac => element,
        KeyOs::Windows => element.occlude(),
    }
}

/// [`clickable`] for an element built elsewhere: on Windows it is wrapped in one that blocks the
/// mouse; on macOS it stays as it is.
pub fn clickable_any(os: KeyOs, element: AnyElement) -> AnyElement {
    match os {
        KeyOs::Mac => element,
        KeyOs::Windows => div()
            .flex_none()
            .flex()
            .items_center()
            .occlude()
            .child(element)
            .into_any_element(),
    }
}

/// A title bar's empty middle on Windows can't take Flux's double click: Windows maximizes the
/// window itself (the module's doc). `true` where Flux handles it, as on macOS.
pub fn handles_double_click(os: KeyOs) -> bool {
    os == KeyOs::Mac
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_keeps_the_traffic_lights_room_and_no_buttons() {
        assert_eq!(leading(KeyOs::Mac, false), TRAFFIC_LIGHTS_WIDTH);
        assert_eq!(leading(KeyOs::Mac, true), GAP + 4.);
        assert_eq!(trailing(KeyOs::Mac, false), GAP + 2.);
        assert_eq!(trailing(KeyOs::Mac, true), GAP + 2.);
        assert!(window_buttons(KeyOs::Mac, false, true, false).is_empty());
        assert!(handles_double_click(KeyOs::Mac));
    }

    #[test]
    fn windows_has_the_menu_at_the_left_and_the_buttons_at_the_right_edge() {
        assert_eq!(leading(KeyOs::Windows, false), MENU_BUTTON_MARGIN);
        assert!(leading(KeyOs::Windows, false) < TRAFFIC_LIGHTS_WIDTH);
        assert_eq!(trailing(KeyOs::Windows, false), 0.);
        assert!(!handles_double_click(KeyOs::Windows));
        assert_eq!(
            window_buttons(KeyOs::Windows, false, true, false),
            [
                WindowButton::Minimize,
                WindowButton::Maximize,
                WindowButton::Close
            ]
        );
    }

    #[test]
    fn a_maximized_window_offers_restore() {
        let buttons = window_buttons(KeyOs::Windows, false, true, true);
        assert_eq!(buttons[1], WindowButton::Restore);
        assert_eq!(buttons[1].area(), WindowControlArea::Max);
        assert_eq!(buttons[1].icon(), IconName::WindowRestore);
    }

    #[test]
    fn a_fixed_size_window_has_no_maximize() {
        // The welcome guide: Windows gives a window that can't be resized no maximize box.
        assert_eq!(
            window_buttons(KeyOs::Windows, false, false, false),
            [WindowButton::Minimize, WindowButton::Close]
        );
    }

    #[test]
    fn full_screen_has_no_buttons_and_the_usual_margins() {
        assert!(window_buttons(KeyOs::Windows, true, true, false).is_empty());
        assert_eq!(trailing(KeyOs::Windows, true), GAP + 2.);
    }

    #[test]
    fn every_button_takes_its_own_area() {
        assert_eq!(WindowButton::Minimize.area(), WindowControlArea::Min);
        assert_eq!(WindowButton::Maximize.area(), WindowControlArea::Max);
        assert_eq!(WindowButton::Close.area(), WindowControlArea::Close);
    }
}
