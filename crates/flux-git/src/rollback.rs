//! Rollback: files go back to their HEAD state, in the index and in the working tree — the
//! "Rollback" of JetBrains IDEs. Added files leave the index and stay on disk as untracked, unless
//! asked to be deleted.

use crate::cli::GitError;
use crate::repo::Repo;
use crate::status::FileStatus;

/// A file to roll back: its path (relative, `/`), its status and, for a rename, the old path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackFile {
    pub path: String,
    pub status: FileStatus,
    pub orig_path: Option<String>,
}

/// Rolls the files back. Modified, deleted and type-changed files are restored from HEAD; added
/// ones are removed from the index (and from disk with `delete_added`); a rename is undone (the
/// old file comes back). Untracked files are left alone.
pub fn rollback(repo: &Repo, files: &[RollbackFile], delete_added: bool) -> Result<(), GitError> {
    let mut restore: Vec<&str> = Vec::new();
    let mut unadd: Vec<&str> = Vec::new();
    for file in files {
        match file.status {
            FileStatus::Modified | FileStatus::Deleted | FileStatus::TypeChanged => {
                restore.push(&file.path)
            }
            FileStatus::Added => unadd.push(&file.path),
            FileStatus::Renamed => {
                unadd.push(&file.path);
                if let Some(orig) = &file.orig_path {
                    restore.push(orig);
                }
            }
            FileStatus::Conflicted => restore.push(&file.path),
            FileStatus::Untracked => {}
        }
    }
    if !unadd.is_empty() {
        repo.git()
            .args(["rm", "-q", "--cached", "--ignore-unmatch", "-r", "--"])
            .args(&unadd)
            .output()?;
        if delete_added {
            for path in &unadd {
                std::fs::remove_file(repo.absolute(path)).ok();
            }
        }
    }
    if !restore.is_empty() {
        repo.git()
            .args(["checkout", "-q", "HEAD", "--"])
            .args(&restore)
            .output()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::tests::init_repo;
    use crate::status::status;
    use std::fs;

    #[test]
    fn modified_deleted_and_added_files_are_rolled_back() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let repo = init_repo(&root);
        fs::write(root.join("a.txt"), "a\n").unwrap();
        fs::write(root.join("b.txt"), "b\n").unwrap();
        repo.git().args(["add", "."]).output().unwrap();
        repo.git()
            .args(["commit", "-q", "-m", "x"])
            .output()
            .unwrap();
        fs::write(root.join("a.txt"), "changed\n").unwrap();
        fs::remove_file(root.join("b.txt")).unwrap();
        fs::write(root.join("c.txt"), "c\n").unwrap();
        repo.git().args(["add", "c.txt"]).output().unwrap();
        let files: Vec<RollbackFile> = status(&repo)
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| RollbackFile {
                path: entry.path,
                status: entry.status,
                orig_path: entry.orig_path,
            })
            .collect();
        assert_eq!(files.len(), 3);
        rollback(&repo, &files, false).unwrap();
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "a\n");
        assert_eq!(fs::read_to_string(root.join("b.txt")).unwrap(), "b\n");
        // The added file is kept on disk, untracked.
        let left = status(&repo).unwrap().entries;
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].path, "c.txt");
        assert_eq!(left[0].status, FileStatus::Untracked);
    }
}
