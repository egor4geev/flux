//! Test helpers: real repositories in temporary directories — a working tree with an identity, a
//! bare remote, a teammate's clone that pushes to it. The user's global git config (rerere, signing,
//! `pull.rebase`) must not change what the tests see: the repositories turn those off locally.

use std::fs;
use std::path::{Path, PathBuf};

use crate::cli::GitCommand;
use crate::repo::Repo;

/// A temporary directory with canonical paths (`/private/var/…` on macOS).
pub(crate) struct Sandbox {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
}

impl Sandbox {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        Self { _dir: dir, root }
    }

    /// A fresh repository at `name` with its first branch `main`.
    pub fn repo(&self, name: &str) -> Repo {
        let path = self.root.join(name);
        fs::create_dir_all(&path).unwrap();
        run(&path, &["init", "-q", "-b", "main"]);
        configure(&path);
        Repo::discover(&path).unwrap()
    }

    /// A bare repository at `name`: a remote.
    pub fn bare(&self, name: &str) -> PathBuf {
        run(&self.root, &["init", "-q", "--bare", "-b", "main", name]);
        self.root.join(name)
    }

    /// A clone of `remote` at `name` (a teammate).
    pub fn clone(&self, remote: &Path, name: &str) -> Repo {
        run(&self.root, &["clone", "-q", remote.to_str().unwrap(), name]);
        let path = self.root.join(name);
        configure(&path);
        Repo::discover(&path).unwrap()
    }

    /// A repository with one commit on `main`, pushed to a bare remote `origin` (upstream set), and
    /// the remote's path.
    pub fn with_remote(&self) -> (Repo, PathBuf) {
        let remote = self.bare("remote.git");
        let repo = self.repo("work");
        git(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        write(&repo, "a.txt", "one\ntwo\nthree\n");
        commit_all(&repo, "First");
        git(&repo, &["push", "-q", "-u", "origin", "main"]);
        (repo, remote)
    }
}

fn configure(path: &Path) {
    for (key, value) in [
        ("user.name", "Flux Test"),
        ("user.email", "test@flux.dev"),
        ("commit.gpgsign", "false"),
        ("tag.gpgsign", "false"),
        ("rerere.enabled", "false"),
        ("pull.rebase", "false"),
        ("merge.conflictStyle", "merge"),
    ] {
        run(path, &["config", key, value]);
    }
}

fn run(dir: &Path, args: &[&str]) -> String {
    GitCommand::new(dir)
        .args(args)
        .output_string()
        .unwrap_or_else(|err| panic!("git {args:?}: {err} {:?}", err.details()))
}

/// Runs git in the repository; panics on failure.
pub(crate) fn git(repo: &Repo, args: &[&str]) -> String {
    run(&repo.work_dir, args)
}

/// Writes a file of the working tree (directories are made).
pub(crate) fn write(repo: &Repo, path: &str, content: &str) {
    let path = repo.absolute(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

pub(crate) fn read(repo: &Repo, path: &str) -> String {
    fs::read_to_string(repo.absolute(path)).unwrap()
}

/// Commits everything; returns the commit.
pub(crate) fn commit_all(repo: &Repo, message: &str) -> String {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", message]);
    head(repo)
}

pub(crate) fn head(repo: &Repo) -> String {
    git(repo, &["rev-parse", "HEAD"]).trim().to_string()
}

/// The commit a revision names.
pub(crate) fn oid(repo: &Repo, rev: &str) -> String {
    git(repo, &["rev-parse", rev]).trim().to_string()
}

/// The branch HEAD is on ("" — detached).
pub(crate) fn branch(repo: &Repo) -> String {
    GitCommand::new(&repo.work_dir)
        .args(["symbolic-ref", "--short", "-q", "HEAD"])
        .output_string()
        .map(|name| name.trim().to_string())
        .unwrap_or_default()
}
