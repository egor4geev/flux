//! Watching project files: on macOS this uses FSEvents (via `notify`), recursively over the whole
//! root with a single event stream and no separate watcher per directory.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::rules::is_skipped;

/// What changed on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsChange {
    /// These paths changed (created, deleted, renamed — both old and new name — or written). They
    /// are absolute and carry the same prefix as the root passed to [`Watcher::new`]. Events inside
    /// VCS service directories (`.git/…`) and about junk files (`.DS_Store`) are not reported here;
    /// `.gitignore` and `.ignore` are (the excluded set is recomputed from them).
    Paths(Vec<PathBuf>),
    /// Events were lost (OS queue overflow, watcher error): re-read everything that is shown.
    Rescan,
}

/// Watcher for the project root; watching stops when it is dropped.
pub struct Watcher {
    _watcher: RecommendedWatcher,
}

impl Watcher {
    /// Watches `root` recursively. `on_change` is called from the watcher thread for every OS event
    /// (often several per millisecond), so the receiver must coalesce the calls.
    pub fn new(root: &Path, on_change: impl Fn(FsChange) + Send + 'static) -> io::Result<Self> {
        let paths = RootPaths::new(root);
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let change = match event {
                    Ok(event) if event.need_rescan() => FsChange::Rescan,
                    Ok(event) if matches!(event.kind, EventKind::Access(_)) => return,
                    Ok(event) => {
                        let changed: Vec<PathBuf> = event
                            .paths
                            .iter()
                            .filter_map(|path| paths.project_path(path))
                            .collect();
                        if changed.is_empty() {
                            return;
                        }
                        FsChange::Paths(changed)
                    }
                    Err(_) => FsChange::Rescan,
                };
                on_change(change);
            })
            .map_err(io::Error::other)?;
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(io::Error::other)?;
        Ok(Self { _watcher: watcher })
    }
}

/// Maps event paths back to the root as it was given: FSEvents reports real paths
/// (`/private/tmp/x/a.rs` for the root `/tmp/x`).
struct RootPaths {
    root: PathBuf,
    canonical: Option<PathBuf>,
}

impl RootPaths {
    fn new(root: &Path) -> Self {
        let canonical = fs::canonicalize(root).ok().filter(|real| real != root);
        Self {
            root: root.to_path_buf(),
            canonical,
        }
    }

    /// An event path inside the project comes back with the `root` prefix; for paths outside the
    /// root, inside `.git` or about `.DS_Store`, the result is `None`.
    fn project_path(&self, path: &Path) -> Option<PathBuf> {
        let rest = path.strip_prefix(&self.root).ok().or_else(|| {
            let canonical = self.canonical.as_deref()?;
            path.strip_prefix(canonical).ok()
        })?;
        if rest.iter().any(is_skipped) {
            return None;
        }
        Some(self.root.join(rest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn event_paths_are_mapped_to_the_given_root() {
        let paths = RootPaths {
            root: PathBuf::from("/tmp/p"),
            canonical: Some(PathBuf::from("/private/tmp/p")),
        };
        let mapped = |path: &str| paths.project_path(Path::new(path));
        assert_eq!(
            mapped("/private/tmp/p/src/a.rs"),
            Some(PathBuf::from("/tmp/p/src/a.rs"))
        );
        assert_eq!(mapped("/tmp/p/a.rs"), Some(PathBuf::from("/tmp/p/a.rs")));
        assert_eq!(mapped("/tmp/p"), Some(PathBuf::from("/tmp/p")));
        assert_eq!(
            mapped("/tmp/p/.gitignore"),
            Some(PathBuf::from("/tmp/p/.gitignore"))
        );
        assert_eq!(mapped("/tmp/p/.git/index"), None);
        assert_eq!(mapped("/tmp/p/sub/.hg/store"), None);
        assert_eq!(mapped("/tmp/p/.DS_Store"), None);
        assert_eq!(mapped("/tmp/other/a.rs"), None);
        // Special names above the root don't get in the way.
        let paths = RootPaths {
            root: PathBuf::from("/x/.git/worktree"),
            canonical: None,
        };
        assert_eq!(
            paths.project_path(Path::new("/x/.git/worktree/a.rs")),
            Some(PathBuf::from("/x/.git/worktree/a.rs"))
        );
    }

    /// Waits until all of `expected` are among the received paths (for at most 5 s).
    fn wait_for(events: &mpsc::Receiver<FsChange>, seen: &mut Vec<PathBuf>, expected: &[&Path]) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !expected.iter().all(|path| seen.iter().any(|p| p == path)) {
            let left = deadline.saturating_duration_since(Instant::now());
            match events.recv_timeout(left) {
                Ok(FsChange::Paths(paths)) => seen.extend(paths),
                Ok(FsChange::Rescan) => {}
                Err(_) => panic!("no events for {expected:?}; got {seen:?}"),
            }
        }
    }

    #[test]
    fn changes_on_disk_are_reported_under_the_given_root() {
        // The temporary directory is `/var/folders/…`, while FSEvents reports `/private/var/…`.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::create_dir(root.join(".git")).unwrap();
        let (sender, events) = mpsc::channel();
        let _watcher = Watcher::new(&root, move |change| {
            sender.send(change).ok();
        })
        .unwrap();
        // The FSEvents event stream doesn't start instantly.
        std::thread::sleep(Duration::from_millis(300));

        let mut seen = Vec::new();
        let created = root.join("new.rs");
        fs::write(&created, "x").unwrap();
        wait_for(&events, &mut seen, &[&created]);

        let renamed = root.join("renamed.rs");
        fs::rename(&created, &renamed).unwrap();
        wait_for(&events, &mut seen, &[&renamed]);

        fs::create_dir(root.join("sub")).unwrap();
        let nested = root.join("sub/deep.txt");
        fs::write(&nested, "y").unwrap();
        fs::write(root.join(".git/index"), "z").unwrap();
        let gitignore = root.join(".gitignore");
        fs::write(&gitignore, "*.log\n").unwrap();
        wait_for(&events, &mut seen, &[&nested, &gitignore]);

        fs::remove_file(&renamed).unwrap();
        seen.retain(|path| path != &renamed);
        wait_for(&events, &mut seen, &[&renamed]);

        assert!(seen.iter().all(|path| path.starts_with(&root)), "{seen:?}");
        assert!(
            !seen.iter().any(|path| path.iter().any(|c| c == ".git")),
            "{seen:?}"
        );
    }
}
