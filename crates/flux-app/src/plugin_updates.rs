//! Plugin updates (stage 8.3), as in JetBrains IDEs: when Flux starts (unless turned off on the
//! Installed tab of Settings → Plugins), the catalog is read in the background and newer versions
//! of the installed plugins are offered in a notification — Update, or Show (the Installed tab).
//! The gear menu's «Check for Updates» does the same at any time and also says when there are none.

use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{App, Context, SharedString, Window, actions};

use crate::i18n::{tr, trn};
use crate::notification_center::NotificationGroup;
use crate::notifications::Notification;
use crate::workspace::Workspace;
use crate::{plugin_catalog, settings, settings_view};

actions!(
    plugin_updates,
    [
        /// Reads the catalog and offers the updates of the installed plugins (the gear menu).
        CheckForUpdates,
        /// Updates every installed plugin the catalog has a newer version of (a notification's
        /// «Update»).
        UpdatePlugins,
        /// Settings → Plugins on the Installed tab, at the first plugin to update (a notification's
        /// «Show»).
        ShowPluginUpdates,
    ]
);

/// Looks for updates of the installed plugins, if the settings say so; the catalog's cache is
/// read anyway (suggestions work offline).
pub fn check_on_start(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    register_actions(cx);
    plugin_catalog::index(cx);
    if settings::check_plugin_updates(cx) {
        check(workspace, false, cx);
    }
}

/// Reads the catalog and offers the updates in a notification. `tell_none`: also say that every
/// plugin is up to date, or why the catalog couldn't be read (asked by the user).
pub fn check(workspace: &mut Workspace, tell_none: bool, cx: &mut Context<Workspace>) {
    let _ = workspace;
    let fetch = plugin_catalog::fetch(cx);
    cx.spawn(async move |workspace, cx| {
        let result = fetch.await;
        workspace
            .update(cx, |workspace, cx| match result {
                Ok(index) => {
                    let updates = plugin_catalog::updates(workspace.plugins.read(cx), &index);
                    if updates.is_empty() {
                        if tell_none {
                            workspace.notify(
                                Notification::success(tr("All plugins are up to date"))
                                    .group(NotificationGroup::Plugins)
                                    .transient(),
                                cx,
                            );
                        }
                        return;
                    }
                    let names = updates
                        .iter()
                        .map(|entry| format!("{} {}", entry.name, entry.version))
                        .collect::<Vec<_>>()
                        .join(", ");
                    workspace.dismiss_notifications(&["plugin_updates::UpdatePlugins"], cx);
                    workspace.notify(
                        Notification::info(trn(
                            updates.len(),
                            "Plugin update available",
                            "Plugin updates available",
                        ))
                        .body(names)
                        .action(tr("Update"), UpdatePlugins)
                        .action(tr("Show"), ShowPluginUpdates)
                        .group(NotificationGroup::Plugins),
                        cx,
                    );
                }
                Err(error) if tell_none => workspace.notify(
                    Notification::warning(tr("Couldn't check for plugin updates"))
                        .body(error)
                        .group(NotificationGroup::Plugins),
                    cx,
                ),
                Err(_) => {}
            })
            .ok();
    })
    .detach();
}

/// Updates every installed plugin the catalog has a newer version of.
pub fn update_all(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(index) = plugin_catalog::peek(cx) else {
        return;
    };
    let updates = plugin_catalog::updates(workspace.plugins.read(cx), &index);
    let this = cx.weak_entity();
    for entry in updates {
        plugin_catalog::install(entry, this.clone(), window, cx);
    }
}

/// Settings → Plugins on the Installed tab, at the first plugin to update.
fn show_updates(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let first = plugin_catalog::peek(cx).and_then(|index| {
        plugin_catalog::updates(workspace.plugins.read(cx), &index)
            .into_iter()
            .next()
    });
    match first {
        Some(entry) => {
            settings_view::open_plugin(workspace, SharedString::from(entry.id), window, cx)
        }
        None => crate::plugin_manager::open(workspace, window, cx),
    }
}

/// The notifications' links are dispatched from wherever the focus is: the app handles them, in
/// the active window (once per process).
fn register_actions(cx: &mut App) {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.swap(true, Ordering::Relaxed) {
        return;
    }
    cx.on_action(|_: &UpdatePlugins, cx| in_active_workspace(cx, update_all));
    cx.on_action(|_: &ShowPluginUpdates, cx| in_active_workspace(cx, show_updates));
    cx.on_action(|_: &CheckForUpdates, cx| {
        in_active_workspace(cx, |workspace, _, cx| check(workspace, true, cx))
    });
}

/// Runs `f` on the workspace of the active window (or the first one), after the current update.
fn in_active_workspace(
    cx: &mut App,
    f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) {
    let window = cx
        .active_window()
        .into_iter()
        .chain(cx.windows())
        .find_map(|window| window.downcast::<Workspace>());
    let Some(window) = window else {
        return;
    };
    cx.defer(move |cx| {
        window.update(cx, f).ok();
    });
}
