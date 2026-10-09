//! Conflicted files: their three versions from the index stages (`:1:` base, `:2:` ours, `:3:`
//! theirs) for the merge tool, taking one side whole ("Accept Yours" / "Accept Theirs" — also for a
//! file one side deleted), and marking a file resolved with the merge tool's result.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::blob::BlobReader;
use crate::cli::GitError;
use crate::repo::Repo;

/// The versions of a conflicted file; `None` — that side has no such file (added on one side only,
/// deleted on one side).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConflictVersions {
    pub base: Option<Vec<u8>>,
    pub ours: Option<Vec<u8>>,
    pub theirs: Option<Vec<u8>>,
}

/// Reads the three versions of a conflicted file (relative path, `/`) from the index.
pub fn conflict_versions(blobs: &BlobReader, path: &str) -> Result<ConflictVersions, GitError> {
    Ok(ConflictVersions {
        base: blobs.read(":1", path)?,
        ours: blobs.read(":2", path)?,
        theirs: blobs.read(":3", path)?,
    })
}

/// One side of a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictSide {
    Ours,
    Theirs,
}

/// Resolves files by taking one side whole: its content (or its deletion) goes into the working
/// tree and the index. Files that are no longer conflicted are left alone.
pub fn accept_side(repo: &Repo, paths: &[String], side: ConflictSide) -> Result<(), GitError> {
    if paths.is_empty() {
        return Ok(());
    }
    let stages = unmerged_stages(repo, paths)?;
    let wanted = match side {
        ConflictSide::Ours => 2,
        ConflictSide::Theirs => 3,
    };
    let mut take = Vec::new();
    let mut delete = Vec::new();
    for path in paths {
        match stages.get(path.as_str()) {
            Some(stages) if stages.contains(&wanted) => take.push(path.as_str()),
            // That side deleted the file (or never had it): the resolution is the deletion.
            Some(_) => delete.push(path.as_str()),
            None => {}
        }
    }
    if !take.is_empty() {
        let flag = match side {
            ConflictSide::Ours => "--ours",
            ConflictSide::Theirs => "--theirs",
        };
        repo.git()
            .literal_pathspecs()
            .args(["checkout", flag, "--"])
            .args(&take)
            .output()?;
        repo.git()
            .literal_pathspecs()
            .args(["add", "--"])
            .args(&take)
            .output()?;
    }
    if !delete.is_empty() {
        repo.git()
            .literal_pathspecs()
            .args(["rm", "-q", "--ignore-unmatch", "--"])
            .args(&delete)
            .output()?;
    }
    Ok(())
}

/// The index stages (1 base, 2 ours, 3 theirs) of the unmerged files among `paths`.
fn unmerged_stages<'a>(
    repo: &Repo,
    paths: &'a [String],
) -> Result<HashMap<&'a str, Vec<u8>>, GitError> {
    let output = repo
        .git()
        .read_only()
        .literal_pathspecs()
        .args(["ls-files", "-u", "-z", "--"])
        .args(paths)
        .output_string()?;
    let mut stages: HashMap<&str, Vec<u8>> = HashMap::new();
    for entry in output.split('\0').filter(|entry| !entry.is_empty()) {
        // `<mode> <object> <stage>\t<path>`
        let Some((info, path)) = entry.split_once('\t') else {
            continue;
        };
        let Some(stage) = info
            .rsplit(' ')
            .next()
            .and_then(|stage| stage.parse::<u8>().ok())
        else {
            continue;
        };
        if let Some(path) = paths.iter().find(|candidate| *candidate == path) {
            stages.entry(path.as_str()).or_default().push(stage);
        }
    }
    Ok(stages)
}

/// Marks a file resolved: `content` (the merge tool's result) is written first, if given; then the
/// file goes into the index as it is (`git add`; a file that is gone — as deleted).
pub fn mark_resolved(repo: &Repo, path: &str, content: Option<&[u8]>) -> Result<(), GitError> {
    if let Some(content) = content {
        write_keeping_mode(&repo.absolute(path), content)?;
    }
    repo.git()
        .literal_pathspecs()
        .args(["add", "-A", "--", path])
        .output()
        .map(drop)
}

/// Writes a file through a temporary one next to it and a rename (never half-written), keeping the
/// mode of the file it replaces (an executable script stays executable).
fn write_keeping_mode(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let permissions = fs::metadata(path).ok().map(|meta| meta.permissions());
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = path.with_file_name(format!(".{name}.flux-merge"));
    fs::write(&temp, content)?;
    if let Some(permissions) = permissions {
        fs::set_permissions(&temp, permissions)?;
    }
    fs::rename(&temp, path).inspect_err(|_| {
        fs::remove_file(&temp).ok();
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{ConflictKind, FileStatus, status};
    use crate::testing::{Sandbox, commit_all, git, read, write};
    use std::os::unix::fs::PermissionsExt;

    /// Main and `theirs` change `both.txt`; main deletes `deleted-here.txt` that theirs changes, and
    /// theirs deletes `deleted-there.txt` that main changes. Merged: three conflicts.
    fn conflicted() -> (Sandbox, Repo) {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "both.txt", "one\ntwo\nthree\n");
        write(&repo, "deleted-here.txt", "x\n");
        write(&repo, "deleted-there.txt", "y\n");
        write(&repo, "run.sh", "echo base\n");
        std::fs::set_permissions(repo.absolute("run.sh"), fs::Permissions::from_mode(0o755))
            .unwrap();
        commit_all(&repo, "Base");
        git(&repo, &["switch", "-q", "-c", "theirs"]);
        write(&repo, "both.txt", "one\nTHEIRS\nthree\n");
        write(&repo, "deleted-here.txt", "x changed\n");
        fs::remove_file(repo.absolute("deleted-there.txt")).unwrap();
        write(&repo, "run.sh", "echo theirs\n");
        commit_all(&repo, "Theirs");
        git(&repo, &["switch", "-q", "main"]);
        write(&repo, "both.txt", "one\nOURS\nthree\n");
        fs::remove_file(repo.absolute("deleted-here.txt")).unwrap();
        write(&repo, "deleted-there.txt", "y changed\n");
        write(&repo, "run.sh", "echo ours\n");
        commit_all(&repo, "Ours");
        let merged = crate::branch::merge(&repo, "theirs", false).unwrap();
        assert_eq!(merged, crate::Outcome::Conflicts);
        (sandbox, repo)
    }

    #[test]
    fn conflicts_have_kinds_and_three_versions() {
        let (_sandbox, repo) = conflicted();
        let kinds: Vec<(String, Option<ConflictKind>)> = status(&repo)
            .unwrap()
            .entries
            .into_iter()
            .filter(|entry| entry.status == FileStatus::Conflicted)
            .map(|entry| (entry.path, entry.conflict))
            .collect();
        assert_eq!(
            kinds,
            [
                ("both.txt".to_string(), Some(ConflictKind::BothModified)),
                (
                    "deleted-here.txt".to_string(),
                    Some(ConflictKind::DeletedByUs)
                ),
                (
                    "deleted-there.txt".to_string(),
                    Some(ConflictKind::DeletedByThem)
                ),
                ("run.sh".to_string(), Some(ConflictKind::BothModified)),
            ]
        );
        let blobs = BlobReader::new(&repo);
        let versions = conflict_versions(&blobs, "both.txt").unwrap();
        assert_eq!(versions.base.as_deref(), Some(&b"one\ntwo\nthree\n"[..]));
        assert_eq!(versions.ours.as_deref(), Some(&b"one\nOURS\nthree\n"[..]));
        assert_eq!(
            versions.theirs.as_deref(),
            Some(&b"one\nTHEIRS\nthree\n"[..])
        );
        let deleted = conflict_versions(&blobs, "deleted-here.txt").unwrap();
        assert_eq!(deleted.ours, None);
        assert_eq!(deleted.theirs.as_deref(), Some(&b"x changed\n"[..]));
    }

    #[test]
    fn sides_are_taken_and_files_marked_resolved() {
        let (_sandbox, repo) = conflicted();
        accept_side(
            &repo,
            &["both.txt".into(), "deleted-there.txt".into()],
            ConflictSide::Theirs,
        )
        .unwrap();
        assert_eq!(read(&repo, "both.txt"), "one\nTHEIRS\nthree\n");
        assert!(
            !repo.absolute("deleted-there.txt").exists(),
            "theirs deleted it"
        );
        // Ours deleted this one: taking ours deletes it.
        accept_side(&repo, &["deleted-here.txt".into()], ConflictSide::Ours).unwrap();
        assert!(!repo.absolute("deleted-here.txt").exists());
        // The merge tool's result, written with the file's mode kept.
        mark_resolved(&repo, "run.sh", Some(b"echo both\n")).unwrap();
        assert_eq!(read(&repo, "run.sh"), "echo both\n");
        let mode = fs::metadata(repo.absolute("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
        assert!(!crate::ops::has_unmerged(&repo));
        // A resolved file is left alone by another accept.
        accept_side(&repo, &["both.txt".into()], ConflictSide::Ours).unwrap();
        assert_eq!(read(&repo, "both.txt"), "one\nTHEIRS\nthree\n");
        let staged = git(&repo, &["diff", "--cached", "--name-status"]);
        assert!(staged.contains("D\tdeleted-there.txt"), "{staged}");
    }
}
