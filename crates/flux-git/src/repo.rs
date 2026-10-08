//! Repositories of a project. A project root may be inside a repository (`flux ~/dev/x/crates`), be
//! one, or contain several (a folder with independent repositories, submodules): like JetBrains
//! IDEs, Flux works with all of them — a file belongs to the innermost repository containing it.

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::cli::GitCommand;

/// How deep under the project root nested repositories are looked for.
const NESTED_DEPTH: usize = 3;
/// Directories never searched for nested repositories: dependencies and build output.
const SKIPPED_DIRS: &[&str] = &["node_modules", "target", ".venv", "venv", "build", "dist"];

/// A repository: its working tree and git directories, as canonical paths.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Repo {
    /// The top of the working tree.
    pub work_dir: PathBuf,
    /// This working tree's git directory: `.git`, or `.git/worktrees/<name>` of a linked worktree,
    /// or `.git/modules/<name>` of a submodule. HEAD and the index live here.
    pub git_dir: PathBuf,
    /// The shared git directory: refs, objects, config (the same as `git_dir` for a plain
    /// repository).
    pub common_dir: PathBuf,
}

impl Repo {
    /// The repository whose working tree contains `path` (a file or a directory, existing or not:
    /// its nearest existing ancestor is asked). `None` outside any repository, or inside a `.git`
    /// directory, or without git.
    pub fn discover(path: &Path) -> Option<Repo> {
        let dir = path.ancestors().find(|dir| dir.is_dir())?;
        let output = GitCommand::new(dir)
            .read_only()
            .args([
                "rev-parse",
                "--show-toplevel",
                "--absolute-git-dir",
                "--git-common-dir",
            ])
            .output_string()
            .ok()?;
        let mut lines = output.lines();
        let work_dir = PathBuf::from(lines.next()?.trim());
        let git_dir = PathBuf::from(lines.next()?.trim());
        let common = PathBuf::from(lines.next()?.trim());
        if work_dir.as_os_str().is_empty() {
            return None;
        }
        // `--git-common-dir` is relative to the directory asked from, when it isn't absolute.
        let common_dir = if common.is_absolute() {
            common
        } else {
            dir.join(common)
        };
        let canonical = |path: PathBuf| fs::canonicalize(&path).unwrap_or(path);
        Some(Repo {
            work_dir: canonical(work_dir),
            git_dir: canonical(git_dir),
            common_dir: canonical(common_dir),
        })
    }

    /// A git command that runs in the working tree.
    pub fn git(&self) -> GitCommand {
        GitCommand::new(&self.work_dir)
    }

    /// `path` (absolute) relative to the working tree, with `/` separators, as git prints and takes
    /// paths; `None` outside the working tree.
    pub fn relative(&self, path: &Path) -> Option<String> {
        let rest = path.strip_prefix(&self.work_dir).ok()?;
        let parts: Vec<String> = rest
            .components()
            .map(|part| match part {
                Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect::<Option<_>>()?;
        Some(parts.join("/"))
    }

    /// A path as git prints it (relative, `/`) → absolute.
    pub fn absolute(&self, relative: &str) -> PathBuf {
        self.work_dir.join(relative)
    }

    /// Whether `path` is inside this repository's working tree.
    pub fn contains(&self, path: &Path) -> bool {
        path.starts_with(&self.work_dir)
    }
}

/// The repositories of a project root: the one containing the root (if any) and those nested in it
/// down to a few levels (independent repositories in subfolders, submodules), skipping dependency
/// and build directories. Sorted by working tree; no duplicates.
pub fn find_repos(root: &Path) -> Vec<Repo> {
    let mut repos = Vec::new();
    let mut seen = HashSet::new();
    if let Some(repo) = Repo::discover(root) {
        seen.insert(repo.work_dir.clone());
        repos.push(repo);
    }
    let mut pending = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = pending.pop() {
        if depth >= NESTED_DEPTH {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if !kind.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == ".git" || SKIPPED_DIRS.contains(&name.as_ref()) {
                continue;
            }
            let path = entry.path();
            // A `.git` directory or file (a submodule, a linked worktree) marks a working tree.
            if path.join(".git").exists()
                && let Some(repo) = Repo::discover(&path)
                && seen.insert(repo.work_dir.clone())
            {
                repos.push(repo);
            }
            pending.push((path, depth + 1));
        }
    }
    repos.sort_by(|a, b| a.work_dir.cmp(&b.work_dir));
    repos
}

/// The innermost repository containing `path`.
pub fn repo_for<'a>(repos: &'a [Repo], path: &Path) -> Option<&'a Repo> {
    repos
        .iter()
        .filter(|repo| repo.contains(path))
        .max_by_key(|repo| repo.work_dir.components().count())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fresh repository with an identity, so that commits work anywhere.
    pub(crate) fn init_repo(dir: &Path) -> Repo {
        let git = |args: &[&str]| {
            GitCommand::new(dir).args(args).output().unwrap();
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.name", "Flux Test"]);
        git(&["config", "user.email", "test@flux.dev"]);
        git(&["config", "commit.gpgsign", "false"]);
        Repo::discover(dir).unwrap()
    }

    #[test]
    fn a_repository_is_found_from_inside() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let repo = init_repo(&root);
        assert_eq!(repo.work_dir, root);
        assert_eq!(repo.git_dir, root.join(".git"));
        assert_eq!(repo.common_dir, root.join(".git"));
        fs::create_dir_all(root.join("src/deep")).unwrap();
        let inner = Repo::discover(&root.join("src/deep/missing.rs")).unwrap();
        assert_eq!(inner, repo);
        assert_eq!(
            repo.relative(&root.join("src/deep/a.rs")).as_deref(),
            Some("src/deep/a.rs")
        );
        assert_eq!(repo.relative(Path::new("/elsewhere/a.rs")), None);
        assert_eq!(repo.absolute("src/a.rs"), root.join("src/a.rs"));
    }

    #[test]
    fn nested_repositories_are_found_under_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        for name in ["app", "libs/core", "node_modules/pkg"] {
            let path = root.join(name);
            fs::create_dir_all(&path).unwrap();
            init_repo(&path);
        }
        let repos = find_repos(&root);
        let dirs: Vec<_> = repos.iter().map(|repo| repo.work_dir.clone()).collect();
        assert_eq!(dirs, vec![root.join("app"), root.join("libs/core")]);
        let file = root.join("libs/core/src/x.rs");
        assert_eq!(
            repo_for(&repos, &file).map(|repo| &repo.work_dir),
            Some(&root.join("libs/core"))
        );
        assert!(repo_for(&repos, &root.join("readme.md")).is_none());
    }
}
