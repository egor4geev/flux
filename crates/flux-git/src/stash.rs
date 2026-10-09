//! Stashes, as the Stash tab of JetBrains IDEs shows them: the list with messages, branches and
//! dates, the files of a stash (untracked ones too), creating one (with a message, untracked files,
//! keeping the index, only some paths), applying or popping it (conflicts are left to resolve),
//! unstashing into a new branch, dropping and clearing.
//!
//! A stash is named by its commit: indices (`stash@{2}`) move when another stash comes or goes, so
//! an operation finds the current index of the commit it was asked about.

use crate::cli::GitError;
use crate::ops::{Outcome, rev_parse, step_outcome};
use crate::repo::Repo;
use crate::status::{FileChange, FileStatus, parse_name_status};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stash {
    /// Its place in the list now: `stash@{index}`.
    pub index: usize,
    pub oid: String,
    /// The user's message ("Parser experiment"), or git's ("WIP on main: 4d7de13 Fix the parser").
    pub message: String,
    /// The branch it was made on; `None` — a detached HEAD.
    pub branch: Option<String>,
    /// Seconds since the epoch.
    pub time: i64,
}

/// The stashes, newest first.
pub fn stashes(repo: &Repo) -> Result<Vec<Stash>, GitError> {
    let output = repo
        .git()
        .read_only()
        .args(["stash", "list", "--format=%gd%x00%H%x00%gs%x00%ct"])
        .output_string()?;
    Ok(output
        .lines()
        .enumerate()
        .filter_map(|(position, line)| {
            let mut fields = line.split('\0');
            let reference = fields.next()?;
            let oid = fields.next()?.trim();
            let subject = fields.next()?;
            let time = fields.next()?.trim().parse().unwrap_or(0);
            let index = reference
                .strip_prefix("stash@{")
                .and_then(|rest| rest.strip_suffix('}'))
                .and_then(|index| index.parse().ok())
                .unwrap_or(position);
            let (message, branch) = parse_subject(subject);
            (!oid.is_empty()).then(|| Stash {
                index,
                oid: oid.to_string(),
                message,
                branch,
                time,
            })
        })
        .collect())
}

/// A stash's reflog subject → its message and branch: "On main: Parser experiment" (a message of
/// the user's), "WIP on main: 4d7de13 Fix the parser" (git's own — kept whole), "On (no branch): …".
fn parse_subject(subject: &str) -> (String, Option<String>) {
    let (wip, rest) = match (subject.strip_prefix("WIP on "), subject.strip_prefix("On ")) {
        (Some(rest), _) => (true, rest),
        (None, Some(rest)) => (false, rest),
        (None, None) => return (subject.to_string(), None),
    };
    // A branch name has no ':' — the first ": " ends it.
    let Some((branch, message)) = rest.split_once(": ") else {
        return (subject.to_string(), None);
    };
    let branch = (branch != "(no branch)").then(|| branch.to_string());
    let message = if wip { subject } else { message };
    (message.to_string(), branch)
}

/// The files of a stash: its changes against the commit it was made on, and the untracked files it
/// took (as `Untracked`); relative paths, `/`.
pub fn stash_files(repo: &Repo, oid: &str) -> Result<Vec<(FileStatus, String)>, GitError> {
    Ok(stash_changes(repo, oid)?
        .into_iter()
        .map(|file| (file.status, file.path))
        .collect())
}

/// [`stash_files`] with the old path of a renamed file.
pub fn stash_changes(repo: &Repo, oid: &str) -> Result<Vec<FileChange>, GitError> {
    let output = repo
        .git()
        .read_only()
        .args(["diff-tree", "-r", "--name-status", "-z", "-M"])
        .args([stash_base(oid), oid.to_string()])
        .output_string()?;
    let mut files = parse_name_status(&output);
    let untracked = untracked_commit(oid);
    if rev_parse(repo, &untracked).is_some() {
        let output = repo
            .git()
            .read_only()
            .args(["ls-tree", "-r", "--name-only", "-z", &untracked])
            .output_string()?;
        files.extend(
            output
                .split('\0')
                .filter(|path| !path.is_empty())
                .map(|path| FileChange {
                    status: FileStatus::Untracked,
                    path: path.to_string(),
                    orig_path: None,
                }),
        );
    }
    Ok(files)
}

/// What to stash.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StashRequest {
    /// Empty — git's message ("WIP on main: …").
    pub message: String,
    pub include_untracked: bool,
    /// Staged changes stay staged in the working tree too (`--keep-index`).
    pub keep_index: bool,
    /// Only these paths (relative, `/`); empty — all changes.
    pub paths: Vec<String>,
}

/// Stashes local changes; `false` — there were none to stash.
pub fn stash_push(repo: &Repo, request: &StashRequest) -> Result<bool, GitError> {
    stash_push_oid(repo, request).map(|oid| oid.is_some())
}

/// [`stash_push`] that returns the new stash's commit (`None` — nothing to stash): a smart
/// checkout pops exactly the stash it made.
pub fn stash_push_oid(repo: &Repo, request: &StashRequest) -> Result<Option<String>, GitError> {
    let before = top_stash(repo);
    // Not `--literal-pathspecs`: `stash push -u` cleans the untracked files through a magic
    // pathspec of its own, which that flag breaks (they would stay). The paths are marked literal
    // one by one instead.
    let mut command = repo.git().args(["stash", "push"]);
    if request.include_untracked {
        command = command.arg("--include-untracked");
    }
    if request.keep_index {
        command = command.arg("--keep-index");
    }
    let message = request.message.trim();
    if !message.is_empty() {
        command = command.args(["-m", message]);
    }
    if !request.paths.is_empty() {
        command = command
            .arg("--")
            .args(request.paths.iter().map(|path| format!(":(literal){path}")));
    }
    command.output()?;
    // "No local changes to save" is a success that makes nothing: the top of the list tells.
    let after = top_stash(repo);
    Ok(after.filter(|after| before.as_ref() != Some(after)))
}

/// The stash's commit: the base of a stash's diff is `<oid>^1`, its untracked files are in
/// `<oid>^3`.
pub fn stash_base(oid: &str) -> String {
    format!("{oid}^1")
}

/// The commit with the untracked files of a stash (when it took any).
pub fn untracked_commit(oid: &str) -> String {
    format!("{oid}^3")
}

/// Applies a stash (`pop` — and drops it, unless it conflicts: then it stays in the list);
/// `reinstate_index` — staged changes come back staged. `Conflicts` — files are left to resolve.
pub fn stash_apply(
    repo: &Repo,
    oid: &str,
    pop: bool,
    reinstate_index: bool,
) -> Result<Outcome, GitError> {
    let reference = stash_ref(repo, oid)?;
    let mut command = repo
        .git()
        .args(["stash", if pop { "pop" } else { "apply" }]);
    if reinstate_index {
        command = command.arg("--index");
    }
    let result = command.arg(&reference).output().map(drop);
    step_outcome(repo, result)
}

/// A new branch at the commit the stash was made on, checked out, with the stash applied and
/// dropped (`git stash branch`).
pub fn stash_branch(repo: &Repo, oid: &str, branch: &str) -> Result<Outcome, GitError> {
    let reference = stash_ref(repo, oid)?;
    let result = repo
        .git()
        .args(["stash", "branch", branch, &reference])
        .output()
        .map(drop);
    step_outcome(repo, result)
}

pub fn stash_drop(repo: &Repo, oid: &str) -> Result<(), GitError> {
    let reference = stash_ref(repo, oid)?;
    repo.git()
        .args(["stash", "drop", "-q", &reference])
        .output()
        .map(drop)
}

/// Drops every stash.
pub fn stash_clear(repo: &Repo) -> Result<(), GitError> {
    repo.git().args(["stash", "clear"]).output().map(drop)
}

/// The commit at the top of the stash list.
fn top_stash(repo: &Repo) -> Option<String> {
    rev_parse(repo, "refs/stash")
}

/// `stash@{n}` of a stash's commit now (the list may have moved since it was read).
fn stash_ref(repo: &Repo, oid: &str) -> Result<String, GitError> {
    stashes(repo)?
        .into_iter()
        .find(|stash| stash.oid == oid)
        .map(|stash| format!("stash@{{{}}}", stash.index))
        .ok_or_else(|| GitError::Failed {
            command: "git stash".into(),
            message: format!("error: the stash {} is gone", &oid[..oid.len().min(7)]),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Sandbox, branch, commit_all, git, read, write};

    #[test]
    fn subjects_give_messages_and_branches() {
        assert_eq!(
            parse_subject("On main: Parser: experiment"),
            ("Parser: experiment".into(), Some("main".into()))
        );
        assert_eq!(
            parse_subject("WIP on feature/x: 4d7de13 Fix it"),
            (
                "WIP on feature/x: 4d7de13 Fix it".into(),
                Some("feature/x".into())
            )
        );
        assert_eq!(parse_subject("On (no branch): x"), ("x".into(), None));
        assert_eq!(parse_subject("autostash"), ("autostash".into(), None));
    }

    #[test]
    fn stashes_are_made_listed_applied_and_dropped() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "a.txt", "a\n");
        write(&repo, "b.txt", "b\n");
        commit_all(&repo, "First");
        assert!(
            !stash_push(&repo, &StashRequest::default()).unwrap(),
            "nothing to stash"
        );

        write(&repo, "a.txt", "a changed\n");
        write(&repo, "notes/идея.md", "idea\n");
        let request = StashRequest {
            message: "Parser experiment".into(),
            include_untracked: true,
            ..Default::default()
        };
        let first = stash_push_oid(&repo, &request).unwrap().unwrap();
        assert_eq!(read(&repo, "a.txt"), "a\n");
        assert!(!repo.absolute("notes/идея.md").exists());
        let mut files = stash_files(&repo, &first).unwrap();
        files.sort();
        assert_eq!(
            files,
            [
                (FileStatus::Modified, "a.txt".to_string()),
                (FileStatus::Untracked, "notes/идея.md".to_string())
            ]
        );

        write(&repo, "b.txt", "b changed\n");
        let second = stash_push_oid(&repo, &StashRequest::default())
            .unwrap()
            .unwrap();
        let list = stashes(&repo).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!((list[0].index, list[0].oid.as_str()), (0, second.as_str()));
        assert!(
            list[0].message.starts_with("WIP on main: "),
            "{}",
            list[0].message
        );
        assert_eq!((list[1].index, list[1].oid.as_str()), (1, first.as_str()));
        assert_eq!(list[1].message, "Parser experiment");
        assert_eq!(list[1].branch.as_deref(), Some("main"));
        assert!(list[1].time > 0);

        // Apply keeps the stash, pop drops it — by commit, whatever its index now.
        assert_eq!(
            stash_apply(&repo, &first, false, false).unwrap(),
            Outcome::Done
        );
        assert_eq!(read(&repo, "a.txt"), "a changed\n");
        assert_eq!(read(&repo, "notes/идея.md"), "idea\n");
        assert_eq!(stashes(&repo).unwrap().len(), 2);
        git(&repo, &["reset", "-q", "--hard"]);
        git(&repo, &["clean", "-q", "-fd"]);
        assert_eq!(
            stash_apply(&repo, &first, true, false).unwrap(),
            Outcome::Done
        );
        let list = stashes(&repo).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].oid, second);
        stash_drop(&repo, &second).unwrap();
        assert!(stashes(&repo).unwrap().is_empty());
        assert!(stash_drop(&repo, &second).is_err(), "gone");

        // Only some paths, keeping the index.
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "Second"]);
        write(&repo, "a.txt", "staged\n");
        git(&repo, &["add", "a.txt"]);
        write(&repo, "b.txt", "b again\n");
        let request = StashRequest {
            paths: vec!["b.txt".into()],
            ..Default::default()
        };
        assert!(stash_push(&repo, &request).unwrap());
        assert_eq!(read(&repo, "b.txt"), "b\n", "stashed");
        assert_eq!(read(&repo, "a.txt"), "staged\n", "not asked for");
        let keep = StashRequest {
            keep_index: true,
            ..Default::default()
        };
        assert!(stash_push(&repo, &keep).unwrap());
        assert_eq!(
            read(&repo, "a.txt"),
            "staged\n",
            "the index stays in the working tree"
        );
        stash_clear(&repo).unwrap();
        assert!(stashes(&repo).unwrap().is_empty());
    }

    #[test]
    fn a_conflicting_unstash_keeps_the_stash() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "a.txt", "one\ntwo\nthree\n");
        commit_all(&repo, "First");
        write(&repo, "a.txt", "one\nstashed\nthree\n");
        let stash = stash_push_oid(&repo, &StashRequest::default())
            .unwrap()
            .unwrap();
        write(&repo, "a.txt", "one\ncommitted\nthree\n");
        commit_all(&repo, "Second");
        assert_eq!(
            stash_apply(&repo, &stash, true, false).unwrap(),
            Outcome::Conflicts
        );
        assert_eq!(
            stashes(&repo).unwrap()[0].oid,
            stash,
            "a conflicting pop keeps it"
        );
        let status = crate::status(&repo).unwrap();
        assert_eq!(status.entries[0].status, FileStatus::Conflicted);
        crate::conflict::accept_side(&repo, &["a.txt".into()], crate::ConflictSide::Theirs)
            .unwrap();
        assert_eq!(read(&repo, "a.txt"), "one\nstashed\nthree\n");
        assert!(!crate::ops::has_unmerged(&repo));
    }

    #[test]
    fn a_stash_becomes_a_branch_on_its_base() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "a.txt", "one\ntwo\nthree\n");
        let base = commit_all(&repo, "First");
        write(&repo, "a.txt", "one\nstashed\nthree\n");
        let stash = stash_push_oid(&repo, &StashRequest::default())
            .unwrap()
            .unwrap();
        write(&repo, "a.txt", "one\ncommitted\nthree\n");
        commit_all(&repo, "Second");
        assert_eq!(
            stash_branch(&repo, &stash, "from-stash").unwrap(),
            Outcome::Done
        );
        assert_eq!(branch(&repo), "from-stash");
        assert_eq!(crate::testing::head(&repo), base);
        assert_eq!(read(&repo, "a.txt"), "one\nstashed\nthree\n");
        assert!(stashes(&repo).unwrap().is_empty());
    }
}
