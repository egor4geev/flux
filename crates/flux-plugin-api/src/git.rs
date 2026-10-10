//! The project's Git repositories, read-only (the `project` permission): their branches, HEADs and
//! operations, the changes of the working copy, a file's diff. A change of any of it comes as
//! `Event::GitChanged`.
//!
//! ```ignore
//! let branch = git::repository_of(&path).and_then(|repo| repo.branch); // "feature/TRACKER-12"
//! let diff = git::diff(&path)?;
//! ```

pub use crate::host::git::{Change, ChangeKind, Repository, diff, repositories, status};

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
