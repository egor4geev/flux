//! The project's Git repositories (the `project` permission): their branches, HEADs and
//! operations, the changes of the working copy, a file's diff. A change of any of it comes as
//! `Event::GitChanged`. With `project = "write"` (API 0.3) — a new branch and a checkout the way
//! the Branches popup does them (local changes come along; Smart Checkout asks when they'd be in
//! the way), their end as `Event::GitDone((id, result))`; and the commit window's template while
//! a task of the plugin's is active.
//!
//! ```ignore
//! let branch = git::repository_of(&path).and_then(|repo| repo.branch); // "feature/TRACKER-12"
//! let diff = git::diff(&path)?;
//! self.branching = Some(git::new_branch(&repo.root, "TRACK-142-fix-sign-in", None));
//! git::commit_template("TRACK-142 Fix the sign-in", Some("TRACK-142"));
//! ```

use crate::host::git as raw;
pub use crate::host::git::{
    Branch, Change, ChangeKind, CommitTemplate, Repository, branches, checkout, diff, new_branch,
    repositories, status,
};

/// While a task of the plugin's is active: `text` fills an empty commit message (and replaces the
/// previous template's text the user didn't change), and a message generated with Claude starts
/// with `prefix`.
pub fn commit_template(text: &str, prefix: Option<&str>) {
    raw::set_commit_template(Some(&CommitTemplate {
        text: text.to_string(),
        prefix: prefix.map(str::to_string),
    }))
}

/// No template any more: the task is over.
pub fn clear_commit_template() {
    raw::set_commit_template(None)
}

/// The checked-out branch of the repository whose root is `root`; none — a detached HEAD, or no
/// such repository.
pub fn current_branch(root: &str) -> Option<String> {
    repositories()
        .into_iter()
        .find(|repo| repo.root == root)?
        .branch
}

/// The repository of a file or a folder (absolute): the one with the deepest root that holds it.
pub fn repository_of(path: &str) -> Option<Repository> {
    repositories()
        .into_iter()
        .filter(|repo| holds(&repo.root, path))
        .max_by_key(|repo| repo.root.len())
}

/// The changes in one repository (by its root).
pub fn changes_in(root: &str) -> Vec<Change> {
    status()
        .into_iter()
        .filter(|change| holds(root, &change.path))
        .collect()
}

/// Whether `path` is `root` or inside it.
fn holds(root: &str, path: &str) -> bool {
    let root = root.trim_end_matches('/');
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::holds;

    #[test]
    fn roots_hold_their_paths() {
        assert!(holds("/p/app", "/p/app/src/main.rs"));
        assert!(holds("/p/app/", "/p/app/src"));
        assert!(holds("/p/app", "/p/app"));
        assert!(!holds("/p/app", "/p/application/main.rs"));
        assert!(!holds("/p/app", "/p"));
    }
}
