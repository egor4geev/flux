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

use crate::context_menu::ContextMenu;
use crate::i18n::tr;
use crate::keymap::KeyOs;
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
    crate::keymap::bind_keys(
        cx,
        [
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("alt-cmd-h", HideOthers, None),
            KeyBinding::new("cmd-m", Minimize, None),
            KeyBinding::new("cmd-`", NextWindow, None),
            // macOS may send ⇧⌘` as cmd-~: shift is already in the character.
            KeyBinding::new("cmd-shift-`", PreviousWindow, None),
            KeyBinding::new("cmd-~", PreviousWindow, None),
        ],
    );
    // Hiding is the Mac's: gpui's Windows backend has none (it panics on hiding the other apps),
    // so the actions aren't there — nor in the palette.
    if KeyOs::current() == KeyOs::Mac {
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    }
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
    cx.set_menus(menus(KeyOs::current()));
    // gpui makes a menu the windows menu only when it is named "Window" exactly: in Russian it
    // is «Окно», so it is set here. The same for the help menu («Справка»): macOS puts its search
    // field into it.
    #[cfg(target_os = "macos")]
    {
        appkit::set_windows_menu(WINDOW_MENU);
        appkit::set_help_menu(HELP_MENU);
    }
}

/// The menus of the menu bar in the current interface language: on macOS the menu bar itself; on
/// Windows, which has no menu bar for them, the popup of the title bar's menu button ([`popup`],
/// part 10.9) — one source, so the two never drift apart. Windows has no place for Services,
/// hiding the app and bringing all its windows to front; quitting is Exit there, maximizing is
/// Maximize, and the log is shown in Explorer.
pub fn menus(os: KeyOs) -> Vec<Menu> {
    let mac = os == KeyOs::Mac;
    let mut flux = vec![
        MenuItem::action(tr("About Flux"), settings_view::About),
        MenuItem::action(tr("Check for Updates…"), updates::CheckForUpdates),
        MenuItem::separator(),
        MenuItem::action(tr("Settings…"), settings_view::Toggle),
        MenuItem::separator(),
    ];
    if mac {
        flux.extend([
            MenuItem::os_submenu(tr("Services"), SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(tr("Hide Flux"), Hide),
            MenuItem::action(tr("Hide Others"), HideOthers),
            MenuItem::action(tr("Show All"), ShowAll),
            MenuItem::separator(),
            MenuItem::action(tr("Quit Flux"), workspace::Quit),
        ]);
    } else {
        flux.push(MenuItem::action(tr("Exit"), workspace::Quit));
    }
    let mut window = vec![
        MenuItem::action(tr("Minimize"), Minimize),
        if mac {
            MenuItem::action(tr("Zoom"), Zoom)
        } else {
            MenuItem::action(tr("Maximize"), Zoom)
        },
        MenuItem::separator(),
        MenuItem::action(tr("Next Window"), NextWindow),
        MenuItem::action(tr("Previous Window"), PreviousWindow),
    ];
    if mac {
        window.extend([
            MenuItem::separator(),
            MenuItem::action(tr("Bring All to Front"), BringAllToFront),
        ]);
    }
    vec![
        Menu {
            name: "Flux".into(),
            items: flux,
        },
        Menu {
            name: tr("Window").into(),
            items: window,
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
                if mac {
                    MenuItem::action(tr("Show Log in Finder"), app_log::ShowLog)
                } else {
                    MenuItem::action(tr("Show Log in Explorer"), app_log::ShowLog)
                },
            ],
        },
    ]
}

/// The title bar's menu on Windows (part 10.9): every menu of [`menus`] as a submenu of Flux's own
/// popup — as the main menu button of a Windows IDE shows its menu bar. The shortcuts come from the
/// keymap, in Windows keys; what only the system draws (a system submenu) is left out.
pub fn popup(menu: ContextMenu, os: KeyOs) -> ContextMenu {
    menus(os)
        .into_iter()
        .fold(menu, |menu, Menu { name, items }| {
            menu.submenu(name, |submenu| {
                items.into_iter().fold(submenu, |submenu, item| match item {
                    MenuItem::Action { name, action, .. } => {
                        submenu.boxed_entry_if(true, name, action)
                    }
                    MenuItem::Separator => submenu.separator(),
                    MenuItem::Submenu(_) | MenuItem::SystemMenu(_) => submenu,
                })
            })
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every menu's items: an action's name and its label, `-` for a separator, the name of a
    /// system submenu.
    fn items(menus: &[Menu]) -> Vec<(String, Vec<(String, String)>)> {
        menus
            .iter()
            .map(|menu| {
                let items = menu
                    .items
                    .iter()
                    .map(|item| match item {
                        MenuItem::Action { name, action, .. } => {
                            (action.name().to_string(), name.to_string())
                        }
                        MenuItem::Separator => ("-".into(), String::new()),
                        MenuItem::Submenu(menu) => ("submenu".into(), menu.name.to_string()),
                        MenuItem::SystemMenu(menu) => ("system".into(), menu.name.to_string()),
                    })
                    .collect();
                (menu.name.to_string(), items)
            })
            .collect()
    }

    #[test]
    fn windows_menus_are_the_mac_ones_without_what_windows_has_no_place_for() {
        let mac = items(&menus(KeyOs::Mac));
        let windows = items(&menus(KeyOs::Windows));
        let mac_only = [
            "system",
            "flux::Hide",
            "flux::HideOthers",
            "flux::ShowAll",
            "window::BringAllToFront",
        ];
        assert_eq!(mac.len(), windows.len());
        for ((mac_name, mac_items), (name, items)) in mac.iter().zip(&windows) {
            assert_eq!(mac_name, name);
            let actions = |items: &[(String, String)]| -> Vec<String> {
                let mut actions: Vec<String> = Vec::new();
                for (action, _) in items {
                    if mac_only.contains(&action.as_str())
                        || (action == "-" && actions.last().is_none_or(|last| last == "-"))
                    {
                        continue;
                    }
                    actions.push(action.clone());
                }
                while actions.last().is_some_and(|last| last == "-") {
                    actions.pop();
                }
                actions
            };
            assert_eq!(actions(mac_items), actions(items), "the {name} menu");
            assert!(
                !items
                    .iter()
                    .any(|(action, _)| mac_only.contains(&action.as_str()))
            );
        }
    }

    #[test]
    fn windows_names_quit_maximize_and_the_log_its_own_way() {
        let label = |os: KeyOs, wanted: &str| {
            items(&menus(os))
                .into_iter()
                .flat_map(|(_, items)| items)
                .find(|(action, _)| action == wanted)
                .map(|(_, label)| label)
        };
        assert_eq!(
            label(KeyOs::Mac, "workspace::Quit").as_deref(),
            Some(tr("Quit Flux"))
        );
        assert_eq!(
            label(KeyOs::Windows, "workspace::Quit").as_deref(),
            Some(tr("Exit"))
        );
        assert_eq!(
            label(KeyOs::Windows, "window::Zoom").as_deref(),
            Some(tr("Maximize"))
        );
        assert_eq!(
            label(KeyOs::Windows, "help::ShowLog").as_deref(),
            Some(tr("Show Log in Explorer"))
        );
    }
}
