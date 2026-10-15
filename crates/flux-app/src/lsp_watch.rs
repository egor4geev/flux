//! Files changed outside Flux go to the language servers (part 10.5): the servers register what
//! they watch (`client/registerCapability` with `workspace/didChangeWatchedFiles` and glob
//! patterns), the window's watcher feeds the matching changes to them.
//!
//! A watcher (FSEvents, [`flux_fs::watch::Watcher`]) runs for each workspace root of a server that
//! watches files, only while one does. Its events are coalesced for [`COALESCE`], then — off the
//! UI thread — the paths no server watches are dropped, the rest are told apart: gone — Deleted;
//! born a moment ago (the file's creation time) — Created; otherwise Changed. A server gets the
//! changes it watches in one notification ([`flux_lsp::LanguageServer::files_changed`]); a
//! `cargo build` that writes thousands of files under `target/` costs a few batches, and only
//! what matches the globs (rust-analyzer's `**/*.rs`) is sent.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use flux_fs::watch::{FsChange, Watcher};
use flux_lsp::LanguageServer;
use flux_lsp::lsp_types::FileChangeType;
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::{App, AppContext as _, Task, WeakEntity};

use crate::lsp::LspStore;

/// A burst of events (a save, a `git checkout`, a build) goes as one batch.
const COALESCE: Duration = Duration::from_millis(150);
/// A file born this recently is new (Created), not changed.
const NEW_FILE: Duration = Duration::from_secs(3);

/// The watchers of a window's language servers.
#[derive(Default)]
pub(crate) struct WatchState {
    /// One per root of the servers that watch files.
    watchers: HashMap<PathBuf, Watcher>,
    /// The watchers' events, to the coalescing task.
    sender: Option<UnboundedSender<FsChange>>,
    _task: Option<Task<()>>,
}

impl WatchState {
    /// Watches exactly `roots` (a root under another one is covered by it); none — nothing is
    /// watched.
    pub(crate) fn set_roots(
        &mut self,
        roots: Vec<PathBuf>,
        store: WeakEntity<LspStore>,
        cx: &mut App,
    ) {
        let roots = outermost(roots);
        self.watchers.retain(|root, _| roots.contains(root));
        if roots.is_empty() {
            self.sender = None;
            self._task = None;
            return;
        }
        if self.sender.is_none() {
            let (sender, receiver) = unbounded();
            self._task = Some(cx.spawn(async move |cx| forward(receiver, store, cx).await));
            self.sender = Some(sender);
        }
        let Some(sender) = self.sender.clone() else {
            return;
        };
        for root in roots {
            if self.watchers.contains_key(&root) {
                continue;
            }
            let sender = sender.clone();
            match Watcher::new(&root, move |change| {
                sender.unbounded_send(change).ok();
            }) {
                Ok(watcher) => {
                    self.watchers.insert(root, watcher);
                }
                Err(err) => eprintln!(
                    "flux: can't watch {} for the language servers: {err}",
                    root.display()
                ),
            }
        }
    }
}

/// The roots that aren't inside another root of the list, each once.
fn outermost(mut roots: Vec<PathBuf>) -> Vec<PathBuf> {
    roots.sort();
    roots.dedup();
    let mut kept: Vec<PathBuf> = Vec::new();
    for root in roots {
        if !kept.iter().any(|outer| root.starts_with(outer)) {
            kept.push(root);
        }
    }
    kept
}

/// Coalesces the watchers' events and sends each batch to the servers that watch the files.
async fn forward(
    mut receiver: UnboundedReceiver<FsChange>,
    store: WeakEntity<LspStore>,
    cx: &mut gpui::AsyncApp,
) {
    while let Some(first) = receiver.next().await {
        cx.background_executor().timer(COALESCE).await;
        let mut paths = BTreeSet::new();
        let mut take = |change: FsChange| {
            // Lost events (an overflow) can't be told: the servers keep what they know.
            if let FsChange::Paths(changed) = change {
                paths.extend(changed);
            }
        };
        take(first);
        while let Ok(change) = receiver.try_recv() {
            take(change);
        }
        if paths.is_empty() {
            continue;
        }
        let Ok(servers) = store.update(cx, |store, _| store.watching_servers()) else {
            break;
        };
        if servers.is_empty() {
            continue;
        }
        cx.background_spawn(async move { send(&servers, paths) })
            .await;
    }
}

/// Tells the changes of `paths` to the servers that watch them.
fn send(servers: &[LanguageServer], paths: BTreeSet<PathBuf>) {
    let now = SystemTime::now();
    let changes: Vec<(PathBuf, FileChangeType)> = paths
        .into_iter()
        // Any kind of change: a path no server watches isn't looked at.
        .filter(|path| {
            servers.iter().any(|server| {
                [
                    FileChangeType::CHANGED,
                    FileChangeType::CREATED,
                    FileChangeType::DELETED,
                ]
                .into_iter()
                .any(|change| server.watches(path, change))
            })
        })
        .filter_map(|path| classify(&path, now).map(|change| (path, change)))
        .collect();
    if changes.is_empty() {
        return;
    }
    for server in servers {
        server.files_changed(&changes);
    }
}

/// What happened to `path`: gone — Deleted; born within [`NEW_FILE`] — Created; a file otherwise
/// — Changed. A directory that is there is skipped: the servers watch files.
fn classify(path: &Path, now: SystemTime) -> Option<FileChangeType> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Some(FileChangeType::DELETED);
        }
        Err(_) => return None,
    };
    if metadata.is_dir() {
        return None;
    }
    let born = metadata.created().ok();
    let new = born.is_some_and(|born| now.duration_since(born).map_or(true, |age| age <= NEW_FILE));
    Some(if new {
        FileChangeType::CREATED
    } else {
        FileChangeType::CHANGED
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_inside_others_are_covered() {
        let roots = outermost(vec![
            PathBuf::from("/work/proj/sub"),
            PathBuf::from("/work/proj"),
            PathBuf::from("/work/other"),
            PathBuf::from("/work/proj"),
            PathBuf::from("/work/project"),
        ]);
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/work/other"),
                PathBuf::from("/work/proj"),
                PathBuf::from("/work/project"),
            ]
        );
    }

    #[test]
    fn changes_are_told_apart_by_the_file() {
        let dir = std::env::temp_dir().join(format!("flux-lsp-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("new.rs");
        std::fs::write(&file, "fn main() {}").unwrap();
        let now = SystemTime::now();
        assert_eq!(classify(&file, now), Some(FileChangeType::CREATED));
        // The same file a while later: changed.
        let later = now + Duration::from_secs(60);
        assert_eq!(classify(&file, later), Some(FileChangeType::CHANGED));
        assert_eq!(
            classify(&dir.join("gone.rs"), now),
            Some(FileChangeType::DELETED)
        );
        assert_eq!(classify(&dir, now), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
