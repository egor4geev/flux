//! Operations on commits of the log: cherry-pick, revert, reset of the current branch, a tag at a
//! commit. (Checkout of a commit and a branch at it are [`crate::branch`]'s; rewriting history —
//! reword, squash, drop, interactive rebase — is [`crate::rebase`]'s.)

use crate::cli::GitError;
use crate::ops::{Outcome, blocked_by, step_outcome};
use crate::repo::Repo;
use crate::stash::{StashRequest, stash_apply, stash_push_oid};

/// How `git reset` treats the index and the working tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResetMode {
    /// Only the branch moves: the changes of the dropped commits stay staged.
    Soft,
    /// The branch and the index: the changes stay in the working tree, unstaged.
    Mixed,
    /// The branch, the index and the working tree: local changes are lost.
    Hard,
    /// As hard, but local changes are kept; refuses if they touch files that differ.
    Keep,
}

/// Applies commits onto the current branch, oldest first (the caller orders them). Stops on
/// conflicts ([`Outcome::Conflicts`]: the repository is cherry-picking). `autostash`: local changes
/// in the way are stashed around it (git has no `--autostash` here): they come back after it, or —
/// if it stopped on conflicts — stay in the stash list for an unstash after the conflicts.
pub fn cherry_pick(repo: &Repo, oids: &[String], autostash: bool) -> Result<Outcome, GitError> {
    apply_commits(repo, "cherry-pick", oids, autostash)
}

/// Reverts commits, newest first (the caller orders them), with a commit each. Stops on
/// conflicts. `autostash` as in [`cherry_pick`].
pub fn revert(repo: &Repo, oids: &[String], autostash: bool) -> Result<Outcome, GitError> {
    apply_commits(repo, "revert", oids, autostash)
}

fn apply_commits(
    repo: &Repo,
    command: &str,
    oids: &[String],
    autostash: bool,
) -> Result<Outcome, GitError> {
    let run = || {
        let result = repo
            .git()
            .no_editor()
            .args([command, "--no-edit"])
            .args(oids)
            .output()
            .map(drop);
        step_outcome(repo, result)
    };
    match run() {
        // Local changes in the way (git touched nothing): stash them, apply, bring them back.
        Err(err) if autostash && blocked_by(&err).is_some() => {
            let stashed = stash_push_oid(
                repo,
                &StashRequest {
                    message: format!("Flux: uncommitted changes before {command}"),
                    include_untracked: true,
                    ..Default::default()
                },
            )?;
            let outcome = run();
            match (&outcome, stashed) {
                (Ok(Outcome::Conflicts), _) | (_, None) => outcome,
                (_, Some(oid)) => {
                    let unstash = stash_apply(repo, &oid, true, true)?;
                    match outcome? {
                        _ if unstash == Outcome::Conflicts => Ok(Outcome::Conflicts),
                        outcome => Ok(outcome),
                    }
                }
            }
        }
        outcome => outcome,
    }
}

/// Moves the current branch (or a detached HEAD) to `oid`.
pub fn reset(repo: &Repo, oid: &str, mode: ResetMode) -> Result<(), GitError> {
    let mode = match mode {
        ResetMode::Soft => "--soft",
        ResetMode::Mixed => "--mixed",
        ResetMode::Hard => "--hard",
        ResetMode::Keep => "--keep",
    };
    repo.git()
        .args(["reset", "-q", mode, oid, "--"])
        .output()
        .map(drop)
}

/// A tag at `target`: annotated with `message`, lightweight without; `force` moves an existing
/// one.
pub fn create_tag(
    repo: &Repo,
    name: &str,
    target: &str,
    message: Option<&str>,
    force: bool,
) -> Result<(), GitError> {
    let mut command = repo.git().no_editor().arg("tag");
    if force {
        command = command.arg("-f");
    }
    if let Some(message) = message {
        command = command
            .args(["-a", "-F", "-"])
            .stdin(message.as_bytes().to_vec());
    }
    command.args([name, target]).output().map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{continue_operation, operation};
    use crate::status::RepoState;
    use crate::testing::{Sandbox, commit_all, git, head, oid, read, write};

    #[test]
    fn cherry_pick_applies_commits_in_order() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "a.txt", "1\n");
        commit_all(&repo, "base");
        git(&repo, &["checkout", "-q", "-b", "feature"]);
        write(&repo, "b.txt", "b\n");
        let first = commit_all(&repo, "add b");
        write(&repo, "c.txt", "c\n");
        let second = commit_all(&repo, "add c");
        git(&repo, &["checkout", "-q", "main"]);
        let outcome = cherry_pick(&repo, &[first, second], false).unwrap();
        assert_eq!(outcome, Outcome::Done);
        assert_eq!(read(&repo, "c.txt"), "c\n");
        let subjects = git(&repo, &["log", "--format=%s", "-3"]);
        assert_eq!(subjects, "add c\nadd b\nbase\n");
    }

    #[test]
    fn cherry_pick_stops_on_conflicts_and_continues() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "a.txt", "1\n");
        commit_all(&repo, "base");
        git(&repo, &["checkout", "-q", "-b", "feature"]);
        write(&repo, "a.txt", "feature\n");
        let theirs = commit_all(&repo, "feature change");
        git(&repo, &["checkout", "-q", "main"]);
        write(&repo, "a.txt", "main\n");
        commit_all(&repo, "main change");
        let outcome = cherry_pick(&repo, std::slice::from_ref(&theirs), false).unwrap();
        assert_eq!(outcome, Outcome::Conflicts);
        let op = operation(&repo);
        assert_eq!(op.state, RepoState::CherryPicking);
        assert_eq!(op.incoming.as_deref(), Some(theirs.as_str()));
        write(&repo, "a.txt", "both\n");
        git(&repo, &["add", "a.txt"]);
        assert_eq!(
            continue_operation(&repo, RepoState::CherryPicking).unwrap(),
            Outcome::Done
        );
        assert_eq!(operation(&repo).state, RepoState::Normal);
        assert_eq!(
            git(&repo, &["log", "--format=%s", "-1"]),
            "feature change\n"
        );
    }

    #[test]
    fn cherry_pick_stashes_local_changes_in_the_way() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "a.txt", "1\n2\n3\n4\n5\n");
        commit_all(&repo, "base");
        git(&repo, &["checkout", "-q", "-b", "feature"]);
        write(&repo, "a.txt", "one\n2\n3\n4\n5\n");
        let theirs = commit_all(&repo, "first line");
        git(&repo, &["checkout", "-q", "main"]);
        // An uncommitted change in the same file, elsewhere in it.
        write(&repo, "a.txt", "1\n2\n3\n4\nfive\n");
        assert!(cherry_pick(&repo, std::slice::from_ref(&theirs), false).is_err());
        let outcome = cherry_pick(&repo, std::slice::from_ref(&theirs), true).unwrap();
        assert_ne!(outcome, Outcome::Conflicts);
        assert_eq!(read(&repo, "a.txt"), "one\n2\n3\n4\nfive\n");
        assert_eq!(git(&repo, &["stash", "list"]), "");
    }

    #[test]
    fn revert_makes_a_commit_per_reverted_one() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "a.txt", "1\n");
        commit_all(&repo, "base");
        write(&repo, "b.txt", "b\n");
        let added = commit_all(&repo, "add b");
        assert_eq!(
            revert(&repo, std::slice::from_ref(&added), false).unwrap(),
            Outcome::Done
        );
        assert!(!repo.absolute("b.txt").exists());
        assert_eq!(
            git(&repo, &["log", "--format=%s", "-1"]),
            "Revert \"add b\"\n"
        );
    }

    #[test]
    fn reset_moves_the_branch_by_mode() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "a.txt", "1\n");
        let base = commit_all(&repo, "base");
        write(&repo, "a.txt", "2\n");
        let second = commit_all(&repo, "second");

        reset(&repo, &base, ResetMode::Soft).unwrap();
        assert_eq!(head(&repo), base);
        assert_eq!(git(&repo, &["diff", "--cached", "--name-only"]), "a.txt\n");

        git(&repo, &["reset", "-q", "--hard", &second]);
        reset(&repo, &base, ResetMode::Mixed).unwrap();
        assert_eq!(git(&repo, &["diff", "--cached", "--name-only"]), "");
        assert_eq!(read(&repo, "a.txt"), "2\n");

        git(&repo, &["reset", "-q", "--hard", &second]);
        reset(&repo, &base, ResetMode::Hard).unwrap();
        assert_eq!(read(&repo, "a.txt"), "1\n");

        // Keep: a local change to a file the reset doesn't touch survives.
        git(&repo, &["reset", "-q", "--hard", &second]);
        write(&repo, "b.txt", "local\n");
        git(&repo, &["add", "b.txt"]);
        git(&repo, &["commit", "-q", "-m", "b"]);
        write(&repo, "b.txt", "changed\n");
        let third = head(&repo);
        reset(&repo, &format!("{third}~1"), ResetMode::Keep).unwrap_err();
        reset(&repo, &oid(&repo, "HEAD"), ResetMode::Keep).unwrap();
        assert_eq!(read(&repo, "b.txt"), "changed\n");
    }

    #[test]
    fn tags_are_lightweight_or_annotated() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "a.txt", "1\n");
        let base = commit_all(&repo, "base");
        write(&repo, "a.txt", "2\n");
        let second = commit_all(&repo, "second");
        create_tag(&repo, "v1", &base, None, false).unwrap();
        assert_eq!(git(&repo, &["cat-file", "-t", "v1"]), "commit\n");
        create_tag(&repo, "v2", &second, Some("Release 2"), false).unwrap();
        assert_eq!(git(&repo, &["cat-file", "-t", "v2"]), "tag\n");
        assert_eq!(oid(&repo, "v2^{commit}"), second);
        assert!(create_tag(&repo, "v1", &second, None, false).is_err());
        create_tag(&repo, "v1", &second, None, true).unwrap();
        assert_eq!(oid(&repo, "v1"), second);
    }
}
