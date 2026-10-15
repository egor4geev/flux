//! `settings.json` as a file (part 10.5). Flux reads it again whenever it changes on disk — saved in
//! a tab of Flux, edited by another program, written by Flux itself — and puts what changed in
//! use: the language servers follow by themselves (they observe the settings), the theme, the
//! icons, the look and the scale of the windows and the interface language are applied here; the
//! rest is read where it is used. A file that isn't valid JSON is left as it is: a notification
//! says where it broke, Flux keeps the settings it has and doesn't write the file until it is
//! fixed. "Open Settings File" (the command palette, Settings → Language Servers, the
//! notification) opens it in a tab of the window; an open tab follows the file like any other.

use std::path::{Path, PathBuf};
use std::time::Duration;

use flux_fs::watch::{FsChange, Watcher};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{App, Context, Global, Task, Window, actions};

use crate::i18n::{tr, trf};
use crate::notification_center::NotificationGroup;
use crate::notifications::Notification;
use crate::settings::{self, FileError, Reload, Settings};
use crate::workspace::{Location, Workspace};

actions!(
    settings,
    [
        /// `settings.json` in a tab of the window (the caret where it broke, if it is broken).
        OpenSettingsFile
    ]
);

/// A burst of events (Flux writes a temporary file and renames it) is read once, this much after
/// the first of them.
const SETTLE: Duration = Duration::from_millis(120);

pub fn init(cx: &mut App) {
    cx.on_action(|_: &OpenSettingsFile, cx| {
        // The action arrives while its window is being updated: the window is updated after.
        let Some(window) = workspace_window(cx) else {
            return;
        };
        cx.defer(move |cx| {
            window
                .update(cx, |workspace, window, cx| open(workspace, None, window, cx))
                .ok();
        });
    });
    watch(cx);
    // A file broken before Flux started: told once the windows are open.
    if let Some(error) = settings::file_error(cx) {
        cx.defer(move |cx| tell_error(&error, cx));
    }
}

/// Keeps the watcher of the file and the task that reads it alive.
struct Watching {
    _watcher: Option<Watcher>,
    _task: Task<()>,
}

impl Global for Watching {}

/// Watches the folder of the file (FSEvents; the file itself is replaced on every write).
fn watch(cx: &mut App) {
    let Some(path) = settings::file_path(cx).map(|path| absolute(&path)) else {
        return;
    };
    let Some(dir) = path.parent().map(Path::to_path_buf) else {
        return;
    };
    // Nothing was ever saved: the folder may not be there yet.
    std::fs::create_dir_all(&dir).ok();
    let (sender, mut changes) = mpsc::unbounded::<()>();
    let file = path.clone();
    let watcher = Watcher::new(&dir, move |change| {
        let ours = match change {
            FsChange::Paths(paths) => paths.contains(&file),
            FsChange::Rescan => true,
        };
        if ours {
            sender.unbounded_send(()).ok();
        }
    })
    .inspect_err(|err| eprintln!("flux: can't watch {}: {err}", path.display()))
    .ok();
    let task = cx.spawn(async move |cx| {
        while changes.next().await.is_some() {
            cx.background_executor().timer(SETTLE).await;
            while changes.try_recv().is_ok() {}
            if cx.update(file_changed).is_err() {
                break;
            }
        }
    });
    cx.set_global(Watching {
        _watcher: watcher,
        _task: task,
    });
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The file changed on disk: the settings are read again and what changed is applied; an open tab
/// of the file takes the new text (unless it has unsaved edits).
fn file_changed(cx: &mut App) {
    match settings::reload(cx) {
        Reload::Unchanged => {}
        Reload::Changed { old, fixed } => {
            apply(&old, cx);
            if fixed {
                for window in workspace_windows(cx) {
                    window
                        .update(cx, |workspace, _, cx| {
                            workspace.dismiss_notifications(&[OPEN_SETTINGS_FILE], cx)
                        })
                        .ok();
                }
            }
        }
        Reload::Invalid { error, new } => {
            if new {
                tell_error(&error, cx);
            }
        }
    }
    if let Some(path) = settings::file_path(cx) {
        for window in workspace_windows(cx) {
            window
                .update(cx, |workspace, _, cx| {
                    workspace.sync_documents(Some(vec![path.clone()]), cx)
                })
                .ok();
        }
    }
}

/// The name of [`OpenSettingsFile`]: its notification goes once the file is fixed.
const OPEN_SETTINGS_FILE: &str = "settings::OpenSettingsFile";

/// Puts in use what changed against `old` and needs more than reading: the theme, the file icons,
/// the look and the scale of the windows, the interface language; then every window redraws.
fn apply(old: &Settings, cx: &mut App) {
    let before = &old.appearance;
    let now = settings::appearance(cx);
    if before.theme != now.theme
        || before.sync_with_os != now.sync_with_os
        || before.light_theme != now.light_theme
        || before.dark_theme != now.dark_theme
        || before.ui_style != now.ui_style
    {
        crate::theme::apply(cx);
    }
    if before.ui_style != now.ui_style {
        for window in cx.windows() {
            window
                .update(cx, |_, window, cx| crate::ui::apply_window_style(window, cx))
                .ok();
        }
    }
    if before.icon_theme != now.icon_theme {
        crate::icon_themes::apply(cx);
    }
    if (before.ui_scale - now.ui_scale).abs() > f32::EPSILON {
        crate::ui_scale::set(now.ui_scale, cx);
    }
    if before.language != now.language {
        crate::appearance_settings::apply_language(cx);
    }
    cx.refresh_windows();
}

/// "Couldn't read settings.json" in the active window (the first one, when none is active).
fn tell_error(error: &FileError, cx: &mut App) {
    let Some(window) = workspace_window(cx) else {
        return;
    };
    let message = if error.message == settings::NOT_AN_OBJECT {
        tr("The settings must be a JSON object").to_string()
    } else {
        error.message.clone()
    };
    let place = if error.line > 0 {
        trf(
            "{0} — line {1}, column {2}.",
            // The parser says column 0 at the very start of a line (the end of the file).
            &[&message, &error.line, &error.column.max(1)],
        )
    } else {
        format!("{message}.")
    };
    let notification = Notification::error(tr("Couldn't read settings.json"))
        .body(format!(
            "{place} {}",
            tr("Flux keeps the settings it has and won't write the file until it's fixed.")
        ))
        .action(tr("Open settings.json"), OpenSettingsFile)
        .group(NotificationGroup::General);
    window
        .update(cx, |workspace, _, cx| workspace.notify(notification, cx))
        .ok();
}

/// Opens `settings.json` in a tab of the window and selects `place` (a line and the columns of a
/// key, 0-based); without it, the place the file broke at, or its start. A file that isn't there
/// yet is written first.
pub fn open(
    workspace: &mut Workspace,
    place: Option<(usize, usize, usize)>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(path) = settings::file_path(cx) else {
        let notification = Notification::info(tr("Settings aren't saved to a file in this session"))
            .group(NotificationGroup::General);
        return workspace.notify(notification, cx);
    };
    if !path.exists() {
        settings::save_file(cx);
    }
    let error = settings::file_error(cx).filter(|error| error.line > 0);
    let (line, start, end) = place
        .or_else(|| {
            error.map(|error| {
                let column = error.column.saturating_sub(1);
                (error.line - 1, column, column)
            })
        })
        .unwrap_or_default();
    let location = Location {
        path,
        line,
        start,
        end,
    };
    workspace.open_location(location, true, window, cx);
}

/// The active window's workspace, or the first one when none is active (a scenario in the
/// background, every window minimized).
pub(crate) fn workspace_window(cx: &App) -> Option<gpui::WindowHandle<Workspace>> {
    cx.active_window()
        .into_iter()
        .chain(cx.windows())
        .find_map(|window| window.downcast::<Workspace>())
}

fn workspace_windows(cx: &App) -> Vec<gpui::WindowHandle<Workspace>> {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<Workspace>())
        .collect()
}
