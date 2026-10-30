//! The menu bar: the application menu (the bold «Flux» next to the Apple menu) with About, Check
//! for Updates… (part 10.8), Settings,
//! Services, hiding, and Quit, the Window menu (part 10.1): Minimize, Zoom, the next and the
//! previous Flux window, Bring All to Front, and — added by macOS — the list of the windows, and the
//! Help menu: the welcome guide, the keys to remember and What's New (part 10.7), reporting a bug,
//! an idea or a question on GitHub, and the log (part 10.6) — with the search field macOS adds to
//! it. The items dispatch the same actions as their keys; macOS shows the shortcuts from the
//! keymap.
//!
//! The menu's titles are in the interface language: when it changes while Flux runs (Settings →
//! Appearance → Language), the menu is built anew ([`rebuild`]).

use gpui::{AnyWindowHandle, App, KeyBinding, Menu, MenuItem, SystemMenuType, actions};

use crate::i18n::tr;
use crate::workspace::Workspace;
use crate::{app_log, feedback, settings_view, updates, welcome, workspace};

actions!(flux, [Hide, HideOthers, ShowAll]);

actions!(
    window,
    [
        /// ⌘M: the active window goes to the Dock.
        Minimize,
        /// The active window fills the screen, or comes back to its size.
        Zoom,
        /// ⌘`: the next Flux window (macOS, and JetBrains' Next Project Window on macOS).
        NextWindow,
        /// ⇧⌘`: the previous Flux window (JetBrains: Previous Project Window).
        PreviousWindow,
        /// All the windows of Flux come in front of the other apps' windows.
        BringAllToFront,
    ]
);

/// The Window menu's place in the menu bar: after the application menu.
#[cfg(target_os = "macos")]
const WINDOW_MENU: usize = 1;
/// The Help menu's place: the last one, after Window.
#[cfg(target_os = "macos")]
const HELP_MENU: usize = 2;

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-`", NextWindow, None),
        // macOS may send ⇧⌘` as cmd-~: shift is already in the character.
        KeyBinding::new("cmd-shift-`", PreviousWindow, None),
        KeyBinding::new("cmd-~", PreviousWindow, None),
    ]);
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Minimize, cx| {
        with_active_window(cx, |window: &mut gpui::Window| window.minimize_window())
    });
    cx.on_action(|_: &Zoom, cx| {
        with_active_window(cx, |window: &mut gpui::Window| window.zoom_window())
    });
    cx.on_action(|_: &NextWindow, cx| cycle_windows(1, cx));
    cx.on_action(|_: &PreviousWindow, cx| cycle_windows(-1, cx));
    cx.on_action(|_: &BringAllToFront, _| {
        #[cfg(target_os = "macos")]
        appkit::bring_all_to_front();
    });
    rebuild(cx);
}

/// Sets the menu bar in the current interface language.
pub fn rebuild(cx: &mut App) {
    cx.set_menus(vec![
        Menu {
            name: "Flux".into(),
            items: vec![
                MenuItem::action(tr("About Flux"), settings_view::About),
                MenuItem::action(tr("Check for Updates…"), updates::CheckForUpdates),
                MenuItem::separator(),
                MenuItem::action(tr("Settings…"), settings_view::Toggle),
                MenuItem::separator(),
                MenuItem::os_submenu(tr("Services"), SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action(tr("Hide Flux"), Hide),
                MenuItem::action(tr("Hide Others"), HideOthers),
                MenuItem::action(tr("Show All"), ShowAll),
                MenuItem::separator(),
                MenuItem::action(tr("Quit Flux"), workspace::Quit),
            ],
        },
        Menu {
            name: tr("Window").into(),
            items: vec![
                MenuItem::action(tr("Minimize"), Minimize),
                MenuItem::action(tr("Zoom"), Zoom),
                MenuItem::separator(),
                MenuItem::action(tr("Next Window"), NextWindow),
                MenuItem::action(tr("Previous Window"), PreviousWindow),
                MenuItem::separator(),
                MenuItem::action(tr("Bring All to Front"), BringAllToFront),
            ],
        },
        Menu {
            name: tr("Help").into(),
            items: vec![
                MenuItem::action(tr("Welcome Guide"), welcome::OpenGuide),
                MenuItem::action(tr("Keyboard Shortcuts"), welcome::ShowShortcuts),
                MenuItem::action(tr("What's New"), welcome::ShowWhatsNew),
                MenuItem::separator(),
                MenuItem::action(tr("Report a Bug…"), feedback::ReportBug),
                MenuItem::action(tr("Suggest a Feature…"), feedback::SuggestFeature),
                MenuItem::action(tr("Ask a Question…"), feedback::AskQuestion),
                MenuItem::separator(),
                MenuItem::action(tr("Show Log in Finder"), app_log::ShowLog),
            ],
        },
    ]);
    // gpui makes a menu the windows menu only when it is named "Window" exactly: in Russian it
    // is «Окно», so it is set here. The same for the help menu («Справка»): macOS puts its search
    // field into it.
    #[cfg(target_os = "macos")]
    {
        appkit::set_windows_menu(WINDOW_MENU);
        appkit::set_help_menu(HELP_MENU);
    }
}

/// The windows of Flux in the order they opened: ⌘` goes along it, as macOS cycles a document
/// app's windows.
fn flux_windows(cx: &App) -> Vec<AnyWindowHandle> {
    cx.windows()
        .into_iter()
        .filter(|window| window.downcast::<Workspace>().is_some())
        .collect()
}

/// The window after the active one (`step` = 1) or before it (-1), wrapping around; without an
/// active one (Flux in the background), from the first.
fn cycle_windows(step: isize, cx: &mut App) {
    let windows = flux_windows(cx);
    if windows.len() < 2 {
        return;
    }
    let current = cx
        .active_window()
        .and_then(|active| windows.iter().position(|window| *window == active))
        .unwrap_or(0);
    let next = windows[(current as isize + step).rem_euclid(windows.len() as isize) as usize];
    // The action arrives while the active window is being updated: the other one is updated
    // after.
    cx.defer(move |cx| {
        next.update(cx, |_, window, _| window.activate_window())
            .ok();
    });
}

/// Does something to the active window once its update (the action's dispatch) is over.
fn with_active_window(cx: &mut App, change: impl FnOnce(&mut gpui::Window) + 'static) {
    let Some(active) = cx.active_window() else {
        return;
    };
    cx.defer(move |cx| {
        active.update(cx, |_, window, _| change(window)).ok();
    });
}

/// The menu bar in AppKit: the windows menu, Bring All to Front.
#[cfg(target_os = "macos")]
// `msg_send!` of objc 0.2 checks a `cargo-clippy` feature this crate doesn't declare.
#[allow(unexpected_cfgs)]
mod appkit {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    /// The menu at `index` of the menu bar becomes the app's windows menu: macOS adds the
    /// windows to it and keeps the list current. On the main thread.
    pub fn set_windows_menu(index: usize) {
        // SAFETY: AppKit's shared application; the submenu comes from `submenu`.
        unsafe {
            if let Some((app, submenu)) = submenu(index) {
                let _: () = msg_send![app, setWindowsMenu: submenu];
            }
        }
    }

    /// The menu at `index` becomes the app's help menu: macOS puts its search field at the top.
    /// On the main thread.
    pub fn set_help_menu(index: usize) {
        // SAFETY: as in `set_windows_menu`.
        unsafe {
            if let Some((app, submenu)) = submenu(index) {
                let _: () = msg_send![app, setHelpMenu: submenu];
            }
        }
    }

    /// The shared application and the submenu at `index` of its menu bar; `None` past the end
    /// or without one.
    ///
    /// SAFETY: on the main thread.
    unsafe fn submenu(index: usize) -> Option<(*mut Object, *mut Object)> {
        // SAFETY: AppKit's shared application and its main menu; the index is checked against
        // the number of items (`itemAtIndex:` raises past the end); nil results are skipped.
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let main: *mut Object = msg_send![app, mainMenu];
            if main.is_null() {
                return None;
            }
            let count: isize = msg_send![main, numberOfItems];
            if index as isize >= count {
                return None;
            }
            let item: *mut Object = msg_send![main, itemAtIndex: index as isize];
            let submenu: *mut Object = msg_send![item, submenu];
            (!submenu.is_null()).then_some((app, submenu))
        }
    }

    /// Window → Bring All to Front: `arrangeInFront:`, as in every Mac app.
    pub fn bring_all_to_front() {
        // SAFETY: AppKit's shared application; the sender may be nil.
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![app, arrangeInFront: std::ptr::null_mut::<Object>()];
        }
    }
}
