//! Folders and files macOS asks Flux to open: a recent folder from the Dock menu, a folder dropped
//! on the Dock icon, the Finder's "Open With". They come as `file://` URLs
//! (`application:openURLs:`), at launch (right after it) or while Flux runs.

use std::path::PathBuf;

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::{App, Application};

use crate::workspace::{self, Workspace};

/// The URLs macOS sends: the callback has no `App`, so they go through a channel to [`run`].
pub fn listen(app: &Application) -> UnboundedReceiver<Vec<String>> {
    let (sender, receiver): (UnboundedSender<Vec<String>>, _) = unbounded();
    app.on_open_urls(move |urls| {
        sender.unbounded_send(urls).ok();
    });
    receiver
}

/// Opens what arrives, for as long as Flux runs.
pub fn run(mut urls: UnboundedReceiver<Vec<String>>, cx: &mut App) {
    cx.spawn(async move |cx| {
        while let Some(urls) = urls.next().await {
            let paths: Vec<PathBuf> = urls.iter().filter_map(|url| file_path(url)).collect();
            if cx.update(|cx| open(paths, cx)).is_err() {
                return;
            }
        }
    })
    .detach();
}

/// A folder becomes a project: its window comes forward, a window without a project (the start
/// screen) takes it, otherwise it opens in a new window. A file opens in the window of the project
/// that holds it, or the window in front.
fn open(paths: Vec<PathBuf>, cx: &mut App) {
    let (dirs, files): (Vec<_>, Vec<_>) = paths.into_iter().partition(|path| path.is_dir());
    for dir in dirs {
        let root = std::fs::canonicalize(&dir).unwrap_or(dir);
        if let Some(handle) = workspace::project_window(&root, cx) {
            handle
                .update(cx, |_, window, _| window.activate_window())
                .ok();
        } else if let Some(handle) = projectless_window(cx) {
            handle
                .update(cx, |workspace, window, cx| {
                    window.activate_window();
                    workspace.open_project(root, Vec::new(), window, cx);
                })
                .ok();
        } else {
            workspace::open_project_window(root, Vec::new(), None, cx);
        }
    }
    for file in files.into_iter().filter(|file| file.is_file()) {
        let target = holding_window(&file, cx).or_else(|| front_window(cx));
        match target {
            Some(handle) => {
                handle
                    .update(cx, |workspace, window, cx| {
                        window.activate_window();
                        workspace.open_files(vec![file], window, cx);
                    })
                    .ok();
            }
            None => {
                workspace::open_startup_windows(None, vec![file], cx);
            }
        }
    }
    cx.activate(true);
}

fn workspaces(cx: &App) -> Vec<gpui::WindowHandle<Workspace>> {
    cx.windows()
        .into_iter()
        .filter_map(|handle| handle.downcast::<Workspace>())
        .collect()
}

/// The start screen: a window without a project.
fn projectless_window(cx: &App) -> Option<gpui::WindowHandle<Workspace>> {
    workspaces(cx).into_iter().find(|handle| {
        handle
            .read(cx)
            .is_ok_and(|workspace| workspace.root().is_none())
    })
}

/// The window whose project holds the file (the deepest root, for nested projects).
fn holding_window(file: &std::path::Path, cx: &App) -> Option<gpui::WindowHandle<Workspace>> {
    workspaces(cx)
        .into_iter()
        .filter_map(|handle| {
            let root = handle.read(cx).ok()?.root()?.to_path_buf();
            file.starts_with(&root).then_some((root, handle))
        })
        .max_by_key(|(root, _)| root.components().count())
        .map(|(_, handle)| handle)
}

fn front_window(cx: &App) -> Option<gpui::WindowHandle<Workspace>> {
    cx.active_window()
        .and_then(|window| window.downcast::<Workspace>())
        .or_else(|| workspaces(cx).into_iter().next())
}

/// The path of a `file://` URL, its `%XX` escapes decoded; `None` for another scheme.
fn file_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // `file://localhost/…` is the same file as `file:///…`.
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let path = percent_decode(rest)?;
    // A folder's URL ends with a slash.
    let path = match path.strip_suffix('/') {
        Some(trimmed) if !trimmed.is_empty() => trimmed.to_string(),
        _ => path,
    };
    Some(PathBuf::from(path))
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_become_paths() {
        assert_eq!(
            file_path("file:///Users/me/My%20Project/"),
            Some(PathBuf::from("/Users/me/My Project"))
        );
        assert_eq!(
            file_path("file:///Users/me/%D0%BF%D1%80%D0%BE%D0%B5%D0%BA%D1%82/main.rs"),
            Some(PathBuf::from("/Users/me/проект/main.rs"))
        );
        assert_eq!(
            file_path("file://localhost/tmp/"),
            Some(PathBuf::from("/tmp"))
        );
        assert_eq!(file_path("file:///"), Some(PathBuf::from("/")));
    }

    #[test]
    fn other_urls_are_ignored() {
        assert_eq!(file_path("https://example.com/a"), None);
        assert_eq!(file_path("file://server/share"), None);
        assert_eq!(file_path("file:///bad%2"), None);
        assert_eq!(file_path("file:///bad%zz"), None);
    }
}
