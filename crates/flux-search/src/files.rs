//! Project files: the root determined by the version control system, and a walk that respects
//! `.gitignore`. The rules are shared with the file tree (`flux_fs::rules`); project search
//! ([`crate::grep`]) uses them too.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use flux_fs::project_walker;
use flux_fs::rules::VCS_DIRS;
use ignore::{DirEntry, WalkBuilder, WalkState};

/// The walk does not yield more files than this (`truncated`): searching for a file across the
/// whole home directory must not eat up memory and time.
pub const MAX_FILES: usize = 100_000;

/// The result of the walk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WalkSummary {
    /// How many files were yielded.
    pub files: usize,
    /// Stopped at [`MAX_FILES`].
    pub truncated: bool,
    /// Stopped by the cancellation flag.
    pub cancelled: bool,
}

/// The project root for the directory `start`: the nearest ancestor (including `start` itself) that
/// contains `.git` (a directory or a file; for git worktrees and submodules it is a file), `.hg`,
/// `.jj`, or `.svn`.
pub fn find_vcs_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| VCS_DIRS.iter().any(|vcs| dir.join(vcs).exists()))
        .map(Path::to_path_buf)
}

/// Walks the project files in parallel. Honors `.gitignore` (inside a git repository), `.ignore`,
/// the global gitignore, and `.git/info/exclude`, including ones from directories above `root`.
/// Hidden files are included; the directories `.git`, `.hg`, `.jj`, `.svn` and the files
/// `.DS_Store` are not. Only regular files are yielded: symlinks are skipped (and not followed).
/// `f` is called from worker threads with a path relative to `root`. The walk stops on `cancel` or
/// after [`MAX_FILES`] files.
pub fn walk_files(root: &Path, cancel: &AtomicBool, f: impl Fn(&Path) + Sync) -> WalkSummary {
    walk_files_limited(root, cancel, MAX_FILES, f)
}

pub(crate) fn walk_files_limited(
    root: &Path,
    cancel: &AtomicBool,
    limit: usize,
    f: impl Fn(&Path) + Sync,
) -> WalkSummary {
    let files = AtomicUsize::new(0);
    let truncated = AtomicBool::new(false);
    let cancelled = AtomicBool::new(false);
    walker(root, None).build_parallel().run(|| {
        let (f, files, truncated, cancelled) = (&f, &files, &truncated, &cancelled);
        Box::new(move |entry| {
            if cancel.load(Ordering::Relaxed) {
                cancelled.store(true, Ordering::Relaxed);
                return WalkState::Quit;
            }
            let Some(entry) = file_entry(entry) else {
                return WalkState::Continue;
            };
            if files.fetch_add(1, Ordering::Relaxed) >= limit {
                truncated.store(true, Ordering::Relaxed);
                return WalkState::Quit;
            }
            f(relative(root, entry.path()));
            WalkState::Continue
        })
    });
    WalkSummary {
        files: files.into_inner().min(limit),
        truncated: truncated.into_inner(),
        cancelled: cancelled.into_inner(),
    }
}

/// A project walk for file search and project search, following the shared rules
/// ([`flux_fs::project_walker`]). `max_file_bytes`: larger files are skipped.
pub(crate) fn walker(root: &Path, max_file_bytes: Option<u64>) -> WalkBuilder {
    let mut builder = project_walker(root, root);
    builder.max_filesize(max_file_bytes);
    builder
}

/// A regular file from the walk; directory read errors and anything that is not a file give `None`.
pub(crate) fn file_entry(entry: Result<DirEntry, ignore::Error>) -> Option<DirEntry> {
    let entry = entry.ok()?;
    entry
        .file_type()
        .is_some_and(|kind| kind.is_file())
        .then_some(entry)
}

pub(crate) fn relative<'a>(root: &Path, path: &'a Path) -> &'a Path {
    path.strip_prefix(root).unwrap_or(path)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    /// A file tree in a temporary directory: `(path, contents)`.
    pub(crate) fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, contents) in files {
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        dir
    }

    fn walk(root: &Path) -> Vec<String> {
        let found = Mutex::new(Vec::new());
        let summary = walk_files(root, &AtomicBool::new(false), |path| {
            found
                .lock()
                .unwrap()
                .push(path.to_string_lossy().into_owned());
        });
        let mut found = found.into_inner().unwrap();
        found.sort();
        assert_eq!(summary.files, found.len());
        assert!(!summary.truncated && !summary.cancelled);
        found
    }

    #[test]
    fn respects_gitignore_and_shows_hidden_files() {
        let dir = tree(&[
            (".git/HEAD", "ref: refs/heads/main"),
            (".git/config", ""),
            (".gitignore", "/target\n*.log\n"),
            (".github/workflows/ci.yml", ""),
            (".env", ""),
            (".DS_Store", ""),
            ("src/main.rs", ""),
            ("src/.DS_Store", ""),
            ("src/debug.log", ""),
            ("target/debug/flux", ""),
            ("crates/a/.gitignore", "generated.rs\n"),
            ("crates/a/generated.rs", ""),
            ("crates/a/lib.rs", ""),
        ]);
        assert_eq!(
            walk(dir.path()),
            [
                ".env",
                ".github/workflows/ci.yml",
                ".gitignore",
                "crates/a/.gitignore",
                "crates/a/lib.rs",
                "src/main.rs",
            ]
        );
    }

    #[test]
    fn gitignore_needs_a_repository() {
        // As in git and ripgrep: outside a repository, .gitignore has no effect, but .ignore does.
        let dir = tree(&[
            (".gitignore", "a.txt\n"),
            (".ignore", "b.txt\n"),
            ("a.txt", ""),
            ("b.txt", ""),
        ]);
        assert_eq!(walk(dir.path()), [".gitignore", ".ignore", "a.txt"]);
    }

    #[test]
    fn vcs_dirs_and_files_are_skipped() {
        let dir = tree(&[
            (".hg/store", ""),
            (".jj/repo", ""),
            (".svn/entries", ""),
            ("sub/.git", "gitdir: ../.git/worktrees/sub"),
            ("sub/file.txt", ""),
        ]);
        assert_eq!(walk(dir.path()), ["sub/file.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_skipped() {
        let dir = tree(&[("real/file.txt", "")]);
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("real/file.txt"),
            dir.path().join("file-link.txt"),
        )
        .unwrap();
        assert_eq!(walk(dir.path()), ["real/file.txt"]);
    }

    #[test]
    fn walk_stops_at_the_limit_and_on_cancel() {
        let files: Vec<(String, &str)> = (0..50).map(|i| (format!("f{i}.txt"), "")).collect();
        let files: Vec<(&str, &str)> = files.iter().map(|(p, c)| (p.as_str(), *c)).collect();
        let dir = tree(&files);
        let count = AtomicUsize::new(0);
        let summary = walk_files_limited(dir.path(), &AtomicBool::new(false), 10, |_| {
            count.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(count.into_inner(), 10);
        assert_eq!(summary.files, 10);
        assert!(summary.truncated);

        let summary = walk_files(dir.path(), &AtomicBool::new(true), |_| {
            panic!("cancelled walk must not report files")
        });
        assert!(summary.cancelled);
        assert_eq!(summary.files, 0);
    }

    /// The non-excluded files of the tree (`flux_fs::list_dir` over the directories) are exactly
    /// what the walk finds: the tree dims the same files that are absent from file search.
    #[test]
    fn tree_listing_agrees_with_the_walk() {
        let dir = tree(&[
            (".git/HEAD", ""),
            (".git/info/exclude", "secret.txt\n"),
            (
                ".gitignore",
                "/target\n*.log\nbuild/\n!keep.log\n*.gen\nnode_modules\nlogs/*\n!logs/.gitkeep\ndocs/*.md\n!docs/README.md\n",
            ),
            (".ignore", "scratch/\n"),
            (".github/workflows/ci.yml", ""),
            (".env.example", ""),
            ("keep.log", ""),
            ("debug.log", ""),
            ("secret.txt", ""),
            ("target/debug/flux", ""),
            ("target/.gitignore", "!*\n"),
            ("src/main.rs", ""),
            ("src/build/out.o", ""),
            ("src/deep/er/file.rs", ""),
            ("src/deep/er/trace.log", ""),
            ("crates/a/.gitignore", "!wanted.gen\nlocal.txt\n"),
            ("crates/a/wanted.gen", ""),
            ("crates/a/other.gen", ""),
            ("crates/a/local.txt", ""),
            ("crates/a/node_modules/x/index.js", ""),
            ("crates/b/local.txt", ""),
            ("logs/.gitkeep", ""),
            ("logs/today.txt", ""),
            ("docs/README.md", ""),
            ("docs/guide.md", ""),
            ("docs/img/logo.png", ""),
            ("scratch/tmp.txt", ""),
        ]);
        let root = dir.path();
        let walked = walk(root);
        assert_eq!(
            walked,
            [
                ".env.example",
                ".github/workflows/ci.yml",
                ".gitignore",
                ".ignore",
                "crates/a/.gitignore",
                "crates/a/wanted.gen",
                "crates/b/local.txt",
                "docs/README.md",
                "docs/img/logo.png",
                "keep.log",
                "logs/.gitkeep",
                "src/deep/er/file.rs",
                "src/main.rs",
            ]
        );
        let mut listed = Vec::new();
        list_tree(root, root, false, &mut listed);
        listed.sort();
        assert_eq!(listed, walked);
    }

    /// The files of a fully expanded tree, except the excluded ones.
    fn list_tree(root: &Path, dir: &Path, ignored: bool, files: &mut Vec<String>) {
        for entry in flux_fs::list_dir(root, dir, ignored).unwrap() {
            let path = dir.join(&entry.name);
            match entry.kind {
                flux_fs::EntryKind::Dir => list_tree(root, &path, entry.ignored, files),
                flux_fs::EntryKind::File if !entry.ignored => {
                    files.push(relative(root, &path).to_string_lossy().into_owned())
                }
                flux_fs::EntryKind::File => {}
            }
        }
    }

    #[test]
    fn vcs_root_is_the_nearest_marked_ancestor() {
        let dir = tree(&[
            ("repo/.git/HEAD", ""),
            ("repo/src/deep/file.rs", ""),
            ("repo/worktree/.git", "gitdir: ../.git/worktrees/w"),
            ("repo/worktree/src/lib.rs", ""),
            ("plain/sub/file.txt", ""),
        ]);
        let root = dir.path();
        let repo = root.join("repo");
        assert_eq!(find_vcs_root(&repo.join("src/deep")), Some(repo.clone()));
        assert_eq!(find_vcs_root(&repo), Some(repo.clone()));
        assert_eq!(
            find_vcs_root(&repo.join("worktree/src")),
            Some(repo.join("worktree"))
        );
        // The temporary directory may live inside another repository, so we compare against what is
        // found starting from the temporary directory itself.
        assert_eq!(find_vcs_root(&root.join("plain/sub")), find_vcs_root(root));
    }
}
