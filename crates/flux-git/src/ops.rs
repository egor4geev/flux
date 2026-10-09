//! Operations that can stop halfway — merge, rebase, cherry-pick, revert, unstash — and what is
//! known about the one in progress: what comes in, the step of a rebase, the prepared message; how
//! to continue, skip a commit or abort; and the errors that mean "your local changes are in the
//! way" (the smart checkout and the smart merge stash them).

use std::fs;
use std::path::Path;

use crate::cli::GitError;
use crate::repo::Repo;
use crate::status::{RepoState, repo_state};

/// How an operation that may stop on conflicts ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing to do: already up to date.
    UpToDate,
    /// The branch moved forward without a merge commit.
    FastForward,
    /// Done: a merge commit made, a branch rebased, a stash applied, an operation finished.
    Done,
    /// Stopped on conflicts: the repository is in the middle of the operation (merge, rebase,
    /// cherry-pick) or, after an unstash, has conflicted files.
    Conflicts,
}

/// The operation in progress in a repository, for the window: the commit window's banner, the
/// title bar, the merge tool's captions. Read from the files git keeps in its directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Operation {
    pub state: RepoState,
    /// Merge, cherry-pick, revert: the commit that comes in (`MERGE_HEAD`, `CHERRY_PICK_HEAD`,
    /// `REVERT_HEAD`).
    pub incoming: Option<String>,
    /// Merge: what is merged as git named it in `MERGE_MSG` ("feature/x", "origin/main").
    pub incoming_name: Option<String>,
    /// Rebase: the branch being rebased ("main"; `None` — a detached HEAD was rebased).
    pub rebase_branch: Option<String>,
    /// Rebase: the commit it is rebased onto.
    pub rebase_onto: Option<String>,
    /// Rebase: the commit being applied when it stopped.
    pub stopped_at: Option<String>,
    /// Rebase: the step being applied (from 1) and how many there are.
    pub step: Option<(u32, u32)>,
    /// The message prepared for the commit that concludes the operation (`MERGE_MSG`), without
    /// comment lines.
    pub message: Option<String>,
}

/// Reads the operation in progress: only files, no git process — cheap enough for every status.
pub fn operation(repo: &Repo) -> Operation {
    let dir = &repo.git_dir;
    let state = repo_state(dir);
    let mut op = Operation {
        state,
        ..Default::default()
    };
    let read = |name: &str| read_trimmed(&dir.join(name));
    match state {
        RepoState::Merging => {
            op.incoming = read("MERGE_HEAD").and_then(|heads| heads.lines().next().map(Into::into));
        }
        RepoState::CherryPicking => op.incoming = read("CHERRY_PICK_HEAD"),
        RepoState::Reverting => op.incoming = read("REVERT_HEAD"),
        RepoState::Rebasing => {
            let base = if dir.join("rebase-merge").is_dir() {
                "rebase-merge"
            } else {
                "rebase-apply"
            };
            let file = |name: &str| read(&format!("{base}/{name}"));
            op.rebase_branch = file("head-name")
                .filter(|name| name != "detached HEAD")
                .map(|name| name.trim_start_matches("refs/heads/").to_string());
            op.rebase_onto = file("onto");
            op.stopped_at = file("stopped-sha").or_else(|| file("original-commit"));
            let number = |name: &str| file(name).and_then(|value| value.parse::<u32>().ok());
            op.step = match base {
                "rebase-merge" => number("msgnum").zip(number("end")),
                _ => number("next").zip(number("last")),
            };
        }
        RepoState::Normal | RepoState::Bisecting => {}
    }
    if matches!(
        state,
        RepoState::Merging | RepoState::CherryPicking | RepoState::Reverting
    ) && let Some(message) = read("MERGE_MSG")
    {
        let message: Vec<&str> = message
            .lines()
            .filter(|line| !line.starts_with('#'))
            .collect();
        let message = message.join("\n").trim().to_string();
        if state == RepoState::Merging {
            op.incoming_name = merged_name(&message);
        }
        op.message = (!message.is_empty()).then_some(message);
    }
    op
}

/// "Merge branch 'feature/x'", "Merge remote-tracking branch 'origin/main' into dev" →
/// "feature/x", "origin/main".
fn merged_name(message: &str) -> Option<String> {
    let first = message.lines().next()?;
    let start = first.find('\'')? + 1;
    let end = start + first[start..].find('\'')?;
    Some(first[start..end].to_string())
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// Continues the operation after the conflicts are resolved: `rebase --continue`, `cherry-pick
/// --continue`, `revert --continue` (the commit messages stay as they are: no editor opens). A merge
/// is concluded by a commit instead (the commit window). `Conflicts` — the next step stopped too
/// (or conflicts are still there).
pub fn continue_operation(repo: &Repo, state: RepoState) -> Result<Outcome, GitError> {
    let command = match state {
        RepoState::Rebasing => "rebase",
        RepoState::CherryPicking => "cherry-pick",
        RepoState::Reverting => "revert",
        RepoState::Merging => {
            return Err(GitError::Failed {
                command: "git merge --continue".into(),
                message: "Commit the changes to conclude the merge".into(),
            });
        }
        RepoState::Normal | RepoState::Bisecting => return Err(nothing_in_progress("continue")),
    };
    let result = repo
        .git()
        .no_editor()
        .args([command, "--continue"])
        .output()
        .map(drop);
    step_outcome(repo, result)
}

/// Aborts the operation: the branch and the working tree go back to where they were before it
/// (`merge --abort`, `rebase --abort`, …); an autostash comes back.
pub fn abort_operation(repo: &Repo, state: RepoState) -> Result<(), GitError> {
    let command = match state {
        RepoState::Merging => "merge",
        RepoState::Rebasing => "rebase",
        RepoState::CherryPicking => "cherry-pick",
        RepoState::Reverting => "revert",
        RepoState::Normal | RepoState::Bisecting => return Err(nothing_in_progress("abort")),
    };
    repo.git().args([command, "--abort"]).output().map(drop)
}

/// Rebase: skips the commit that stopped (`rebase --skip`).
pub fn skip_commit(repo: &Repo) -> Result<Outcome, GitError> {
    let result = repo
        .git()
        .no_editor()
        .args(["rebase", "--skip"])
        .output()
        .map(drop);
    step_outcome(repo, result)
}

fn nothing_in_progress(what: &str) -> GitError {
    GitError::Failed {
        command: format!("git {what}"),
        message: "No merge, rebase, cherry-pick or revert is in progress".into(),
    }
}

/// How a step of an operation ended (continue, skip, unstash): conflicts are what the index says.
pub(crate) fn step_outcome(repo: &Repo, result: Result<(), GitError>) -> Result<Outcome, GitError> {
    match result {
        _ if has_unmerged(repo) => Ok(Outcome::Conflicts),
        Ok(()) => Ok(Outcome::Done),
        Err(err) => Err(err),
    }
}

/// How a command that brings commits into the current branch ended (merge, rebase, pull), by the
/// repository rather than git's words: unmerged entries are conflicts (also those an autostash left
/// behind), an unmoved HEAD is "up to date", a HEAD that only moved forward is a fast-forward.
/// `before` — the tip that was integrated into (HEAD, or the branch a rebase checked out).
pub(crate) fn integration_outcome(
    repo: &Repo,
    before: Option<&str>,
    result: Result<(), GitError>,
) -> Result<Outcome, GitError> {
    if has_unmerged(repo) {
        return Ok(Outcome::Conflicts);
    }
    result?;
    let after = head_oid(repo);
    Ok(match (before, after.as_deref()) {
        (Some(before), Some(after)) if before == after => Outcome::UpToDate,
        (_, Some(after)) if parent_count(repo, after) > 1 => Outcome::Done,
        (None, Some(_)) => Outcome::FastForward,
        (Some(before), Some(after)) if is_ancestor(repo, before, after) => Outcome::FastForward,
        _ => Outcome::Done,
    })
}

/// The commit HEAD points to; `None` before the first commit.
pub(crate) fn head_oid(repo: &Repo) -> Option<String> {
    rev_parse(repo, "HEAD")
}

/// The commit a revision names (`rev-parse --verify -q <rev>^{commit}`).
pub(crate) fn rev_parse(repo: &Repo, rev: &str) -> Option<String> {
    repo.git()
        .read_only()
        .args(["rev-parse", "--verify", "-q", &format!("{rev}^{{commit}}")])
        .output_string()
        .ok()
        .map(|oid| oid.trim().to_string())
        .filter(|oid| !oid.is_empty())
}

/// The checked-out branch; `None` — a detached HEAD.
pub(crate) fn current_branch(repo: &Repo) -> Option<String> {
    repo.git()
        .read_only()
        .args(["symbolic-ref", "--short", "-q", "HEAD"])
        .output_string()
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// Whether the index has unmerged entries: files left to resolve.
pub(crate) fn has_unmerged(repo: &Repo) -> bool {
    repo.git()
        .read_only()
        .args(["ls-files", "-u", "-z"])
        .output()
        .is_ok_and(|output| !output.is_empty())
}

fn is_ancestor(repo: &Repo, ancestor: &str, descendant: &str) -> bool {
    repo.git()
        .read_only()
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .is_ok()
}

fn parent_count(repo: &Repo, commit: &str) -> usize {
    repo.git()
        .read_only()
        .args(["rev-list", "--parents", "-n1", commit])
        .output_string()
        .map(|line| line.split_whitespace().count().saturating_sub(1))
        .unwrap_or(0)
}

/// Local changes in the way of an operation, from git's error: files whose changes would be
/// overwritten (`local`), untracked files that would be overwritten (`untracked`), or a rebase
/// that needs a clean working tree (`dirty`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Blocked {
    pub local: Vec<String>,
    pub untracked: Vec<String>,
    pub dirty: bool,
}

/// Whether an error says that local changes are in the way (checkout, merge, rebase, unstash);
/// `None` — some other failure. git's messages are English (`LANGUAGE=en`, [`crate::cli`]).
pub fn blocked_by(error: &GitError) -> Option<Blocked> {
    let GitError::Failed { message, .. } = error else {
        return None;
    };
    #[derive(PartialEq)]
    enum List {
        Local,
        Untracked,
    }
    let mut blocked = Blocked::default();
    let mut list = None;
    for line in message.lines() {
        // The files of a list are indented with a tab.
        if let Some(path) = line.strip_prefix('\t') {
            match list {
                Some(List::Local) => blocked.local.push(unquote(path.trim_end())),
                Some(List::Untracked) => blocked.untracked.push(unquote(path.trim_end())),
                None => {}
            }
            continue;
        }
        list = None;
        if line.contains("Your local changes to the following files would be overwritten by") {
            list = Some(List::Local);
        } else if line.contains("untracked working tree files would be overwritten by")
            || line.contains("untracked working tree files would be removed by")
        {
            list = Some(List::Untracked);
        } else if line.contains("You have unstaged changes")
            || line.contains("Your index contains uncommitted changes")
            || line.contains("Please commit or stash them")
        {
            blocked.dirty = true;
        } else if let Some(path) = line.strip_suffix(" already exists, no checkout") {
            // An unstash whose untracked files are in the way.
            let path = path.strip_prefix("error: ").unwrap_or(path);
            blocked.untracked.push(unquote(path));
        }
    }
    (blocked != Blocked::default()).then_some(blocked)
}

/// A path as git prints it: as is, or C-quoted ("\"tab\\there\"") when it has special characters.
fn unquote(path: &str) -> String {
    let Some(inner) = path
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    else {
        return path.to_string();
    };
    let mut bytes = Vec::with_capacity(inner.len());
    let mut chars = inner.bytes().peekable();
    while let Some(byte) = chars.next() {
        if byte != b'\\' {
            bytes.push(byte);
            continue;
        }
        match chars.next() {
            Some(b'n') => bytes.push(b'\n'),
            Some(b't') => bytes.push(b'\t'),
            Some(b'r') => bytes.push(b'\r'),
            Some(b'a') => bytes.push(0x07),
            Some(b'b') => bytes.push(0x08),
            Some(b'f') => bytes.push(0x0c),
            Some(b'v') => bytes.push(0x0b),
            Some(digit @ b'0'..=b'7') => {
                let mut value = u32::from(digit - b'0');
                for _ in 0..2 {
                    match chars.peek() {
                        Some(next @ b'0'..=b'7') => {
                            value = value * 8 + u32::from(next - b'0');
                            chars.next();
                        }
                        _ => break,
                    }
                }
                bytes.push(value as u8);
            }
            Some(other) => bytes.push(other),
            None => bytes.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(message: &str) -> GitError {
        GitError::Failed {
            command: "git checkout".into(),
            message: message.into(),
        }
    }

    #[test]
    fn local_changes_in_the_way_are_read_from_errors() {
        let checkout = failed(
            "error: Your local changes to the following files would be overwritten by checkout:\n\
             \tsrc/main.rs\n\t\"tab\\there.txt\"\n\t\"\\320\\266.txt\"\n\
             Please commit your changes or stash them before you switch branches.\nAborting",
        );
        assert_eq!(
            blocked_by(&checkout),
            Some(Blocked {
                local: vec!["src/main.rs".into(), "tab\there.txt".into(), "ж.txt".into()],
                ..Default::default()
            })
        );
        let merge = failed(
            "error: Your local changes to the following files would be overwritten by merge:\n\
             \ta.txt\nPlease commit your changes or stash them before you merge.\n\
             error: The following untracked working tree files would be overwritten by merge:\n\
             \tnew.txt\nPlease move or remove them before you merge.\nAborting",
        );
        assert_eq!(
            blocked_by(&merge),
            Some(Blocked {
                local: vec!["a.txt".into()],
                untracked: vec!["new.txt".into()],
                dirty: false,
            })
        );
        let rebase = failed(
            "error: cannot rebase: You have unstaged changes.\nerror: Please commit or stash them.",
        );
        assert!(blocked_by(&rebase).unwrap().dirty);
        let unstash = failed(
            "notes/idea.md already exists, no checkout\nerror: could not restore untracked files from stash",
        );
        assert_eq!(blocked_by(&unstash).unwrap().untracked, ["notes/idea.md"]);
        assert_eq!(blocked_by(&failed("fatal: not a git repository")), None);
        assert_eq!(blocked_by(&GitError::Canceled), None);
    }

    #[test]
    fn merged_names_come_from_the_message() {
        assert_eq!(
            merged_name("Merge branch 'feature/x'").as_deref(),
            Some("feature/x")
        );
        assert_eq!(
            merged_name("Merge remote-tracking branch 'origin/main' into dev\n\nbody").as_deref(),
            Some("origin/main")
        );
        assert_eq!(merged_name("Merge commit"), None);
    }
}
