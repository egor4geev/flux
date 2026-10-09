//! Commits, as the Commit tool window of JetBrains IDEs makes them: the commit is HEAD plus the
//! checked files — whole, or only the checked changes of a file (a partial commit) — whatever the
//! user's staging area holds. Files that are staged but not checked stay staged.
//!
//! How: a temporary index is filled from HEAD and the checked files (`GIT_INDEX_FILE`), `git commit`
//! runs on it (hooks, signing and the user's config apply as usual), and then the real index takes
//! the committed versions of those paths (`git reset -- <paths>`), so that they don't show up as
//! staged reverts.
//!
//! A merge in progress is concluded differently: its result is the real index (git put the merged
//! files there, the resolved conflicts were added), so the checked files go into the real index and
//! `git commit` makes the merge commit from it — with both parents, and with the local changes a
//! `merge --autostash` put away coming back after it. A merge is committed whole: partial content
//! and amending are refused, and so is a commit while conflicts are left.

use std::path::PathBuf;

use crate::cli::{Cancel, GitError};
use crate::ops::has_unmerged;
use crate::repo::Repo;
use crate::status::{RepoState, repo_state};

/// A file to commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitFile {
    /// Relative to the working tree, with `/`.
    pub path: String,
    pub content: CommitContent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitContent {
    /// As it is in the working tree; a file that is gone from the working tree is deleted.
    WorkTree,
    /// This content instead: only the checked changes of the file.
    Partial(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRequest {
    pub message: String,
    /// Replace the last commit (its changes stay, the checked files are added to them).
    pub amend: bool,
    pub files: Vec<CommitFile>,
    /// "Name <email>" instead of the configured author.
    pub author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitResult {
    /// The new commit.
    pub oid: String,
    /// Its message's first line.
    pub summary: String,
}

/// Makes the commit. Blocking: hooks may take long; `cancel` stops them.
pub fn commit(
    repo: &Repo,
    request: &CommitRequest,
    cancel: &Cancel,
) -> Result<CommitResult, GitError> {
    if repo_state(&repo.git_dir) == RepoState::Merging {
        return commit_merge(repo, request, cancel);
    }
    let index = TempIndex::new(repo);
    let with_index = |command: crate::cli::GitCommand| command.env("GIT_INDEX_FILE", &index.path);
    let has_head = repo
        .git()
        .read_only()
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .output()
        .is_ok();
    if has_head {
        with_index(repo.git())
            .args(["read-tree", "HEAD"])
            .output()?;
    } else {
        with_index(repo.git())
            .args(["read-tree", "--empty"])
            .output()?;
    }
    let mut whole = Vec::new();
    for file in &request.files {
        match &file.content {
            CommitContent::WorkTree => whole.push(file.path.as_str()),
            CommitContent::Partial(content) => {
                let oid = repo
                    .git()
                    .args(["hash-object", "-w", "--stdin", "--path", &file.path])
                    .stdin(content.clone())
                    .output_string()?;
                let mode = file_mode(repo, &file.path);
                with_index(repo.git())
                    .args([
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        &format!("{mode},{},{}", oid.trim(), file.path),
                    ])
                    .output()?;
            }
        }
    }
    if !whole.is_empty() {
        // `-z --stdin`: any number of paths, any characters in them.
        let mut paths = Vec::new();
        for path in &whole {
            paths.extend_from_slice(path.as_bytes());
            paths.push(0);
        }
        with_index(repo.git())
            .args(["update-index", "--add", "--remove", "-z", "--stdin"])
            .stdin(paths)
            .output()?;
    }
    let mut command = with_index(repo.git()).args(["commit", "-q", "-F", "-"]);
    if request.amend {
        command = command.arg("--amend");
    }
    if let Some(author) = &request.author {
        command = command.arg(format!("--author={author}"));
    }
    command
        .stdin(request.message.clone())
        .run(Some(cancel), |_| {})?;
    // The real index follows the committed paths.
    let mut paths: Vec<&str> = request
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    paths.sort_unstable();
    paths.dedup();
    let mut reset = repo.git().args(["reset", "-q", "HEAD", "--"]);
    for path in paths {
        reset = reset.arg(path);
    }
    reset.output().ok();
    committed(repo, request)
}

/// Concludes a merge in progress: the checked files go into the real index, which holds the merge
/// result, and `git commit` makes the merge commit (MERGE_HEAD is its second parent).
fn commit_merge(
    repo: &Repo,
    request: &CommitRequest,
    cancel: &Cancel,
) -> Result<CommitResult, GitError> {
    let refuse = |message: &str| GitError::Failed {
        command: "git commit".into(),
        message: message.into(),
    };
    if request
        .files
        .iter()
        .any(|file| matches!(file.content, CommitContent::Partial(_)))
    {
        return Err(refuse(
            "A merge is committed whole: include all the changes of every file",
        ));
    }
    if request.amend {
        return Err(refuse("A merge in progress can't amend the last commit"));
    }
    if has_unmerged(repo) {
        return Err(refuse("Resolve the conflicts before committing the merge"));
    }
    if !request.files.is_empty() {
        let mut paths = Vec::new();
        for file in &request.files {
            paths.extend_from_slice(file.path.as_bytes());
            paths.push(0);
        }
        repo.git()
            .args(["update-index", "--add", "--remove", "-z", "--stdin"])
            .stdin(paths)
            .output()?;
    }
    let mut command = repo.git().args(["commit", "-q", "-F", "-"]);
    if let Some(author) = &request.author {
        command = command.arg(format!("--author={author}"));
    }
    command
        .stdin(request.message.clone())
        .run(Some(cancel), |_| {})?;
    committed(repo, request)
}

/// The commit just made: its hash and the message's first line.
fn committed(repo: &Repo, request: &CommitRequest) -> Result<CommitResult, GitError> {
    let oid = repo
        .git()
        .read_only()
        .args(["rev-parse", "HEAD"])
        .output_string()?
        .trim()
        .to_string();
    let summary = request
        .message
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string();
    Ok(CommitResult { oid, summary })
}

/// The mode of a file for the index: executable bits survive a partial commit.
fn file_mode(repo: &Repo, path: &str) -> &'static str {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::symlink_metadata(repo.absolute(path)) {
        Ok(meta) if meta.file_type().is_symlink() => "120000",
        Ok(meta) if meta.permissions().mode() & 0o111 != 0 => "100755",
        _ => "100644",
    }
}

/// The message of the last commit (Amend puts it into the message field); `None` before the first
/// commit.
pub fn head_message(repo: &Repo) -> Result<Option<String>, GitError> {
    match repo
        .git()
        .read_only()
        .args(["log", "-1", "--format=%B"])
        .output_string()
    {
        Ok(message) => Ok(Some(message.trim_end().to_string())),
        Err(GitError::Failed { .. }) => Ok(None),
        Err(err) => Err(err),
    }
}

/// Messages of the latest commits, newest first: the commit message history.
pub fn recent_messages(repo: &Repo, limit: usize) -> Result<Vec<String>, GitError> {
    let output = match repo
        .git()
        .read_only()
        .args(["log", &format!("-n{limit}"), "--format=%B%x00"])
        .output_string()
    {
        Ok(output) => output,
        Err(GitError::Failed { .. }) => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    Ok(output
        .split('\0')
        .map(|message| message.trim().to_string())
        .filter(|message| !message.is_empty())
        .collect())
}

/// An index file in the git directory, removed when dropped.
struct TempIndex {
    path: PathBuf,
}

impl TempIndex {
    fn new(repo: &Repo) -> Self {
        let path = repo
            .git_dir
            .join(format!("flux-index-{}", std::process::id()));
        std::fs::remove_file(&path).ok();
        Self { path }
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        std::fs::remove_file(&self.path).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::tests::init_repo;
    use crate::status::{FileStatus, status};
    use std::fs;

    fn request(files: Vec<CommitFile>) -> CommitRequest {
        CommitRequest {
            message: "Second\n\nBody".into(),
            amend: false,
            files,
            author: None,
        }
    }

    fn whole(path: &str) -> CommitFile {
        CommitFile {
            path: path.into(),
            content: CommitContent::WorkTree,
        }
    }

    #[test]
    fn only_checked_files_are_committed() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let repo = init_repo(&root);
        fs::write(root.join("a.txt"), "a\n").unwrap();
        fs::write(root.join("b.txt"), "b\n").unwrap();
        // The first commit, of an untracked file, without HEAD.
        let first = commit(&repo, &request(vec![whole("a.txt")]), &Cancel::new()).unwrap();
        assert_eq!(first.summary, "Second");
        fs::write(root.join("a.txt"), "a2\n").unwrap();
        fs::write(root.join("c.txt"), "c\n").unwrap();
        commit(
            &repo,
            &request(vec![whole("a.txt"), whole("c.txt")]),
            &Cancel::new(),
        )
        .unwrap();
        let left: Vec<(String, FileStatus)> = status(&repo)
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| (entry.path, entry.status))
            .collect();
        // a.txt and c.txt are committed and not shown as staged reverts; b.txt stays untracked.
        assert_eq!(left, vec![("b.txt".to_string(), FileStatus::Untracked)]);
        assert_eq!(
            head_message(&repo).unwrap().as_deref(),
            Some("Second\n\nBody")
        );
        assert_eq!(recent_messages(&repo, 5).unwrap().len(), 2);
    }

    #[test]
    fn a_merge_is_concluded_with_both_parents_and_its_autostash() {
        use crate::testing::{Sandbox, commit_all, git, read, write};
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "a.txt", "one\ntwo\nthree\n");
        write(&repo, "b.txt", "b\n");
        commit_all(&repo, "Base");
        git(&repo, &["switch", "-q", "-c", "theirs"]);
        write(&repo, "a.txt", "one\ntheirs\nthree\n");
        write(&repo, "t.txt", "t\n");
        commit_all(&repo, "Theirs");
        git(&repo, &["switch", "-q", "main"]);
        write(&repo, "a.txt", "one\nours\nthree\n");
        commit_all(&repo, "Ours");
        // A local change the merge stashes away and brings back after its commit.
        write(&repo, "b.txt", "b local\n");
        assert_eq!(
            crate::branch::merge(&repo, "theirs", true).unwrap(),
            crate::Outcome::Conflicts
        );
        let message = crate::operation(&repo).message.unwrap();
        let request = |content| CommitRequest {
            message: message.clone(),
            amend: false,
            files: vec![CommitFile {
                path: "a.txt".into(),
                content,
            }],
            author: None,
        };
        let error = commit(&repo, &request(CommitContent::WorkTree), &Cancel::new()).unwrap_err();
        assert!(
            error.to_string().contains("Resolve the conflicts"),
            "{error}"
        );
        write(&repo, "a.txt", "one\nours and theirs\nthree\n");
        let partial = CommitContent::Partial(b"x".to_vec());
        assert!(commit(&repo, &request(partial), &Cancel::new()).is_err());
        crate::conflict::mark_resolved(&repo, "a.txt", None).unwrap();
        let done = commit(&repo, &request(CommitContent::WorkTree), &Cancel::new()).unwrap();
        assert_eq!(done.summary, "Merge branch 'theirs'");
        let parents = git(&repo, &["rev-list", "--parents", "-n1", "HEAD"]);
        assert_eq!(parents.split_whitespace().count(), 3);
        assert_eq!(crate::operation(&repo).state, crate::RepoState::Normal);
        assert_eq!(
            git(&repo, &["show", "HEAD:t.txt"]),
            "t\n",
            "the merged file is in"
        );
        assert_eq!(read(&repo, "b.txt"), "b local\n", "the autostash is back");
        let left = status(&repo).unwrap().entries;
        assert_eq!(left.len(), 1);
        assert_eq!(
            (left[0].path.as_str(), left[0].status),
            ("b.txt", FileStatus::Modified)
        );
    }

    #[test]
    fn partial_content_is_committed_and_the_rest_stays_changed() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let repo = init_repo(&root);
        fs::write(root.join("a.txt"), "1\n2\n3\n").unwrap();
        commit(&repo, &request(vec![whole("a.txt")]), &Cancel::new()).unwrap();
        fs::write(root.join("a.txt"), "1 changed\n2\n3 changed\n").unwrap();
        let partial = CommitFile {
            path: "a.txt".into(),
            content: CommitContent::Partial(b"1 changed\n2\n3\n".to_vec()),
        };
        commit(&repo, &request(vec![partial]), &Cancel::new()).unwrap();
        let head = repo
            .git()
            .args(["show", "HEAD:a.txt"])
            .output_string()
            .unwrap();
        assert_eq!(head, "1 changed\n2\n3\n");
        let entries = status(&repo).unwrap().entries;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, FileStatus::Modified);
        assert!(!entries[0].staged, "nothing is left staged");
    }
}
