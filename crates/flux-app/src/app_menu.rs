//! The menu bar: the application menu (the bold «Flux» next to the Apple menu) with About, Settings,
//! Services, hiding, and Quit. The items dispatch the same actions as their keys; macOS shows the
//! shortcuts from the keymap.

use gpui::{App, KeyBinding, Menu, MenuItem, SystemMenuType, actions};

use crate::i18n::tr;
use crate::{settings_view, workspace};

actions!(flux, [Hide, HideOthers, ShowAll]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
    ]);
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.set_menus(vec![Menu {
        name: "Flux".into(),
        items: vec![
            MenuItem::action(tr("About Flux"), settings_view::About),
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
    }]);
}
