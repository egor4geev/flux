//! Watching a repository: its working tree (files saved, created, deleted — the status changes) and
//! its git directory (a commit, a checkout or a reset in a terminal moves HEAD, rewrites the index or
//! refs — the status and the HEAD versions change). One recursive FSEvents stream per directory; the
//! git directory gets its own when it lies outside the working tree (linked worktrees, submodules).
//!
//! Of the git directory, only what changes the status or HEAD is reported: `HEAD`, `index`,
//! `packed-refs`, `refs/…`, `config` (the upstream), and the markers of an operation in progress
//! (`MERGE_HEAD`, `rebase-merge/`…). Locks, objects, logs and Flux's own temporary index are noise.

use std::io;
use std::path::{Path, PathBuf};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::repo::Repo;

/// What changed in a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoEvent {
    /// Files of the working tree changed (absolute paths).
    WorkTree(Vec<PathBuf>),
    /// HEAD, the index, refs or the operation in progress changed.
    Git,
    /// Events were lost (queue overflow): re-read everything.
    Rescan,
}

/// Watches a repository until dropped.
pub struct RepoWatcher {
    _watchers: Vec<RecommendedWatcher>,
}

impl RepoWatcher {
    /// `on_event` is called from the watcher's thread for every OS event (often several per
    /// millisecond): the receiver coalesces.
    pub fn new(
        repo: &Repo,
        on_event: impl Fn(RepoEvent) + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let on_event = std::sync::Arc::new(on_event);
        let mut dirs = vec![repo.work_dir.clone()];
        for git_dir in [&repo.git_dir, &repo.common_dir] {
            if !git_dir.starts_with(&repo.work_dir) && !dirs.contains(git_dir) {
                dirs.push(git_dir.clone());
            }
        }
        let mut watchers = Vec::new();
        for dir in dirs {
            let classifier = Classifier::new(repo);
            let on_event = on_event.clone();
            let mut watcher =
                notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                    let event = match event {
                        Ok(event) if event.need_rescan() => RepoEvent::Rescan,
                        Ok(event) if matches!(event.kind, EventKind::Access(_)) => return,
                        Ok(event) => match classifier.classify(&event.paths) {
                            Some(event) => event,
                            None => return,
                        },
                        Err(_) => RepoEvent::Rescan,
                    };
                    on_event(event);
                })
                .map_err(io::Error::other)?;
            watcher
                .watch(&dir, RecursiveMode::Recursive)
                .map_err(io::Error::other)?;
            watchers.push(watcher);
        }
        Ok(Self {
            _watchers: watchers,
        })
    }
}

/// Sorts event paths into working tree changes and git directory changes.
struct Classifier {
    work_dir: PathBuf,
    git_dirs: Vec<PathBuf>,
    /// The same directories as FSEvents reports them (`/private/tmp/…` for `/tmp/…`).
    real: Vec<(PathBuf, PathBuf)>,
}

impl Classifier {
    fn new(repo: &Repo) -> Self {
        let mut git_dirs = vec![repo.git_dir.clone()];
        if repo.common_dir != repo.git_dir {
            git_dirs.push(repo.common_dir.clone());
        }
        let real = [&repo.work_dir]
            .into_iter()
            .chain(&git_dirs)
            .filter_map(|dir| {
                let real = std::fs::canonicalize(dir).ok()?;
                (real != *dir).then(|| (real, dir.clone()))
            })
            .collect();
        Self {
            work_dir: repo.work_dir.clone(),
            git_dirs,
            real,
        }
    }

    fn classify(&self, paths: &[PathBuf]) -> Option<RepoEvent> {
        let mut work = Vec::new();
        let mut git = false;
        for path in paths {
            let path = self.unreal(path);
            if let Some(rest) = self
                .git_dirs
                .iter()
                .find_map(|dir| path.strip_prefix(dir).ok())
            {
                git |= relevant_git_path(rest);
            } else if let Ok(rest) = path.strip_prefix(&self.work_dir) {
                // `.git` of a plain repository is inside the working tree: handled above.
                if rest.file_name().is_some_and(|name| name == ".DS_Store") {
                    continue;
                }
                work.push(path.clone());
            }
        }
        if git {
            Some(RepoEvent::Git)
        } else if !work.is_empty() {
            Some(RepoEvent::WorkTree(work))
        } else {
            None
        }
    }

    fn unreal(&self, path: &Path) -> PathBuf {
        for (real, given) in &self.real {
            if let Ok(rest) = path.strip_prefix(real) {
                return given.join(rest);
            }
        }
        path.to_path_buf()
    }
}

/// Whether a change inside the git directory matters to the status or HEAD.
fn relevant_git_path(rest: &Path) -> bool {
    let rest = rest.to_string_lossy();
    if rest.ends_with(".lock") || rest.starts_with("flux-index") {
        return false;
    }
    matches!(
        rest.as_ref(),
        "HEAD"
            | "index"
            | "packed-refs"
            | "config"
            | "MERGE_HEAD"
            | "CHERRY_PICK_HEAD"
            | "REVERT_HEAD"
            | "BISECT_LOG"
    ) || rest.starts_with("refs/")
        || rest.starts_with("rebase-merge")
        || rest.starts_with("rebase-apply")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classifier() -> Classifier {
        Classifier {
            work_dir: PathBuf::from("/p"),
            git_dirs: vec![PathBuf::from("/p/.git")],
            real: vec![(PathBuf::from("/private/p"), PathBuf::from("/p"))],
        }
    }

    #[test]
    fn events_are_sorted_into_work_tree_and_git() {
        let c = classifier();
        let paths = |list: &[&str]| list.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(
            c.classify(&paths(&["/p/src/a.rs"])),
            Some(RepoEvent::WorkTree(paths(&["/p/src/a.rs"])))
        );
        assert_eq!(
            c.classify(&paths(&["/private/p/b.rs"])),
            Some(RepoEvent::WorkTree(paths(&["/p/b.rs"])))
        );
        assert_eq!(c.classify(&paths(&["/p/.git/index"])), Some(RepoEvent::Git));
        assert_eq!(
            c.classify(&paths(&["/p/.git/refs/heads/main"])),
            Some(RepoEvent::Git)
        );
        assert_eq!(c.classify(&paths(&["/p/.git/index.lock"])), None);
        assert_eq!(c.classify(&paths(&["/p/.git/objects/ab/cdef"])), None);
        assert_eq!(c.classify(&paths(&["/p/.git/flux-index-42"])), None);
        assert_eq!(c.classify(&paths(&["/p/.DS_Store"])), None);
        assert_eq!(c.classify(&paths(&["/elsewhere/x"])), None);
    }
}
