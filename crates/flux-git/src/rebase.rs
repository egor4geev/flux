//! Rewriting history: interactive rebase with a plan made in the window (JetBrains' "Interactively
//! Rebase from Here"), and the log's shortcuts built on it — Edit Commit Message, Fixup, Squash
//! Into, Drop Commits.
//!
//! git gets the plan through `GIT_SEQUENCE_EDITOR`; messages (reword, squash) go to the commits
//! without an editor. A rebase that stops (conflicts, `edit`) goes on with
//! [`crate::ops::continue_operation`].

use std::fs;
use std::path::Path;

use crate::cli::GitError;
use crate::ops::{Outcome, has_unmerged, head_oid, rev_parse};
use crate::repo::Repo;
use crate::status::{RepoState, repo_state};

/// What happens to a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RebaseAction {
    Pick,
    /// Pick with a new message ([`RebaseEntry::message`]).
    Reword,
    /// Pick and stop: the user amends the commit, then continues.
    Edit,
    /// Melds into the commit above it in the plan (the previous one); the messages are joined — or
    /// replaced by [`RebaseEntry::message`] of the last commit of the run.
    Squash,
    /// Melds into the previous commit, keeping its message.
    Fixup,
    Drop,
}

/// A line of the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebaseEntry {
    pub oid: String,
    pub summary: String,
    pub action: RebaseAction,
    /// Reword: the new message. Squash: the message of the commit the run becomes (`None` — the
    /// messages joined as git does).
    pub message: Option<String>,
}

/// Where the prepared plan and the messages live while a rebase runs: in the git directory, so
/// that a rebase stopped on conflicts or at an `edit` still finds its messages after `--continue`.
/// Removed when the rebase finishes, and before the next one starts.
const WORK_DIR: &str = "flux-rebase";

/// Removes the plan and messages of a rebase that is over (continued to the end, aborted).
pub(crate) fn clean_up_finished(repo: &Repo) {
    if crate::status::repo_state(&repo.git_dir) != crate::status::RepoState::Rebasing {
        fs::remove_dir_all(repo.git_dir.join(WORK_DIR)).ok();
    }
}

/// The plan for rebasing from `oid` (inclusive) up to HEAD: the commits oldest first, all `pick`.
/// Fails if the range has a merge commit (they aren't rebased interactively here).
pub fn rebase_plan(repo: &Repo, oid: &str) -> Result<Vec<RebaseEntry>, GitError> {
    let has_parent = rev_parse(repo, &format!("{oid}^")).is_some();
    let range = if has_parent {
        format!("{oid}^..HEAD")
    } else {
        "HEAD".to_string()
    };
    let output = repo
        .git()
        .read_only()
        .args([
            "log",
            "--reverse",
            "--topo-order",
            "--no-show-signature",
            "--format=%H%x1f%P%x1f%s",
            &range,
            "--",
        ])
        .output_string()?;
    let mut plan = Vec::new();
    for line in output.lines() {
        let mut fields = line.split('\x1f');
        let (Some(commit), Some(parents), Some(summary)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if parents.split_whitespace().count() > 1 {
            return Err(GitError::Failed {
                command: "git rebase".into(),
                message: format!(
                    "Can't rebase interactively: there is a merge commit {} in the range",
                    &commit[..commit.len().min(8)]
                ),
            });
        }
        plan.push(RebaseEntry {
            oid: commit.to_string(),
            summary: summary.to_string(),
            action: RebaseAction::Pick,
            message: None,
        });
    }
    let first = rev_parse(repo, oid);
    if plan.first().map(|entry| &entry.oid) != first.as_ref() {
        return Err(GitError::Failed {
            command: "git rebase".into(),
            message: format!("{oid} is not in the current branch"),
        });
    }
    Ok(plan)
}

/// Runs the plan: `onto` is the parent of the plan's first commit (`None` — the plan starts at the
/// root commit). `autostash`: local changes are stashed around it. [`Outcome::Conflicts`] — stopped
/// on conflicts or at an `edit` (the operation is in progress).
pub fn interactive_rebase(
    repo: &Repo,
    onto: Option<&str>,
    plan: &[RebaseEntry],
    autostash: bool,
) -> Result<Outcome, GitError> {
    if plan.is_empty() {
        return Ok(Outcome::UpToDate);
    }
    let work = repo.git_dir.join(WORK_DIR);
    if repo_state(&repo.git_dir) != RepoState::Rebasing {
        fs::remove_dir_all(&work).ok();
    }
    fs::create_dir_all(&work)?;
    let todo = match todo(plan, &work) {
        Ok(todo) => todo,
        Err(err) => {
            fs::remove_dir_all(&work).ok();
            return Err(err);
        }
    };
    let todo_path = work.join("git-rebase-todo");
    fs::write(&todo_path, todo)?;
    let mut command = repo
        .git()
        .no_editor()
        // The plan replaces what git proposes; the comments git would add don't matter.
        .env(
            "GIT_SEQUENCE_EDITOR",
            format!("cp {}", shell_quote(&todo_path.to_string_lossy())),
        )
        .args(["-c", "rebase.missingCommitsCheck=ignore"])
        .args(["-c", "rebase.updateRefs=false"])
        .args(["rebase", "-i", "--no-autosquash"]);
    if autostash {
        command = command.arg("--autostash");
    }
    command = match onto {
        Some(onto) => command.arg(onto),
        None => command.arg("--root"),
    };
    let result = command.output().map(drop);
    finish(repo, result)
}

/// How a rebase step ended: still rebasing — stopped (conflicts, or an `edit`); otherwise done,
/// and the work directory is no longer needed.
fn finish(repo: &Repo, result: Result<(), GitError>) -> Result<Outcome, GitError> {
    if repo_state(&repo.git_dir) == RepoState::Rebasing {
        return Ok(Outcome::Conflicts);
    }
    fs::remove_dir_all(repo.git_dir.join(WORK_DIR)).ok();
    if has_unmerged(repo) {
        // The autostash came back with conflicts.
        return Ok(Outcome::Conflicts);
    }
    result.map(|()| Outcome::Done)
}

/// The todo list for git: `pick`, `edit`, `fixup`, `squash`, `drop`; a new message goes to its
/// commit by an `exec git commit --amend` after it (and after the commits melded into it), so that
/// no editor opens — also when a step stops on conflicts and the rebase continues later.
fn todo(plan: &[RebaseEntry], work: &Path) -> Result<String, GitError> {
    let git = crate::cli::git_binary().ok_or(GitError::NotInstalled)?;
    let mut lines = Vec::new();
    let mut message_count = 0;
    let mut i = 0;
    while i < plan.len() {
        let entry = &plan[i];
        match entry.action {
            RebaseAction::Drop => {
                lines.push(format!("drop {}", entry.oid));
                i += 1;
                continue;
            }
            RebaseAction::Squash | RebaseAction::Fixup => {
                return Err(GitError::Failed {
                    command: "git rebase".into(),
                    message: format!(
                        "{} has no previous commit to meld into",
                        &entry.oid[..entry.oid.len().min(8)]
                    ),
                });
            }
            RebaseAction::Edit => lines.push(format!("edit {}", entry.oid)),
            RebaseAction::Pick | RebaseAction::Reword => lines.push(format!("pick {}", entry.oid)),
        }
        // The commits melded into this one (drops among them don't break the run).
        let mut message = match entry.action {
            RebaseAction::Reword => entry.message.clone(),
            _ => None,
        };
        let run: Vec<&RebaseEntry> = plan[i + 1..]
            .iter()
            .take_while(|next| {
                matches!(
                    next.action,
                    RebaseAction::Squash | RebaseAction::Fixup | RebaseAction::Drop
                )
            })
            .collect();
        // A squash with a message of its own: the run becomes fixups and the message is set after.
        let squash_message = run
            .iter()
            .rev()
            .find(|next| next.action == RebaseAction::Squash && next.message.is_some())
            .and_then(|next| next.message.clone());
        if squash_message.is_some() {
            message = squash_message.clone();
        }
        for next in &run {
            let verb = match next.action {
                RebaseAction::Drop => "drop",
                RebaseAction::Squash if squash_message.is_none() => "squash",
                _ => "fixup",
            };
            lines.push(format!("{verb} {}", next.oid));
        }
        if let Some(message) = message {
            message_count += 1;
            let file = work.join(format!("message-{message_count}"));
            fs::write(&file, message)?;
            lines.push(format!(
                "exec {} commit --amend --only --allow-empty --no-verify --cleanup=strip -q -F {}",
                shell_quote(&git.to_string_lossy()),
                shell_quote(&file.to_string_lossy())
            ));
        }
        i += 1 + run.len();
    }
    Ok(lines.join("\n") + "\n")
}

/// A new message for a commit of the current branch (HEAD — amend; older — a rebase).
pub fn reword(repo: &Repo, oid: &str, message: &str) -> Result<Outcome, GitError> {
    let target = rev_parse(repo, oid);
    if target.is_some() && target == head_oid(repo) {
        return repo
            .git()
            .no_editor()
            .args([
                "commit",
                "--amend",
                "--only",
                "--allow-empty",
                "--no-verify",
                "--cleanup=strip",
                "-q",
                "-F",
                "-",
            ])
            .stdin(message.as_bytes().to_vec())
            .output()
            .map(|_| Outcome::Done);
    }
    let mut plan = rebase_plan(repo, oid)?;
    let Some(first) = plan.first_mut() else {
        return Ok(Outcome::UpToDate);
    };
    first.action = RebaseAction::Reword;
    first.message = Some(message.to_string());
    let onto = rev_parse(repo, &format!("{oid}^"));
    interactive_rebase(repo, onto.as_deref(), &plan, true)
}

/// `'…'` for sh: the editor command and `exec` lines run through a shell.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{continue_operation, operation};
    use crate::testing::{Sandbox, commit_all, git, head, read, write};

    /// main: base, then commits a, b, c each adding a file; returns their oids.
    fn three_commits(repo: &Repo) -> (String, Vec<String>) {
        write(repo, "base.txt", "base\n");
        let base = commit_all(repo, "base");
        let commits = ["a", "b", "c"]
            .iter()
            .map(|name| {
                write(repo, &format!("{name}.txt"), &format!("{name}\n"));
                commit_all(repo, &format!("add {name}"))
            })
            .collect();
        (base, commits)
    }

    fn subjects(repo: &Repo) -> String {
        git(repo, &["log", "--format=%s"])
    }

    #[test]
    fn the_plan_lists_commits_oldest_first() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        let (base, commits) = three_commits(&repo);
        let plan = rebase_plan(&repo, &commits[0]).unwrap();
        let oids: Vec<&str> = plan.iter().map(|entry| entry.oid.as_str()).collect();
        assert_eq!(oids, commits.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(plan[2].summary, "add c");
        assert!(plan.iter().all(|entry| entry.action == RebaseAction::Pick));
        // From the root commit.
        assert_eq!(rebase_plan(&repo, &base).unwrap().len(), 4);
    }

    #[test]
    fn the_plan_refuses_merge_commits() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        let (_, commits) = three_commits(&repo);
        git(&repo, &["checkout", "-q", "-b", "side", &commits[0]]);
        write(&repo, "side.txt", "side\n");
        commit_all(&repo, "side");
        git(&repo, &["checkout", "-q", "main"]);
        git(&repo, &["merge", "-q", "--no-edit", "side"]);
        let error = rebase_plan(&repo, &commits[0]).unwrap_err();
        assert!(error.to_string().contains("merge commit"), "{error}");
    }

    #[test]
    fn reorder_drop_fixup_and_squash_with_a_message() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        let (base, commits) = three_commits(&repo);
        write(&repo, "d.txt", "d\n");
        commit_all(&repo, "add d");
        let mut plan = rebase_plan(&repo, &commits[0]).unwrap();
        // a; c squashed into a with a new message; b dropped; d fixed up into… a too.
        plan.swap(1, 2);
        plan[1].action = RebaseAction::Squash;
        plan[1].message = Some("a and c\n\nTogether.".into());
        plan[2].action = RebaseAction::Drop;
        plan[3].action = RebaseAction::Fixup;
        let outcome = interactive_rebase(&repo, Some(&base), &plan, false).unwrap();
        assert_eq!(outcome, Outcome::Done);
        assert_eq!(subjects(&repo), "a and c\nbase\n");
        assert_eq!(git(&repo, &["log", "-1", "--format=%b"]), "Together.\n\n");
        assert!(!repo.absolute("b.txt").exists());
        assert_eq!(read(&repo, "d.txt"), "d\n");
        assert!(!repo.git_dir.join(WORK_DIR).exists());
    }

    #[test]
    fn squash_without_a_message_joins_the_messages() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        let (base, commits) = three_commits(&repo);
        let mut plan = rebase_plan(&repo, &commits[0]).unwrap();
        plan[1].action = RebaseAction::Squash;
        interactive_rebase(&repo, Some(&base), &plan, false).unwrap();
        let message = git(&repo, &["log", "-1", "--skip=1", "--format=%B"]);
        assert!(
            message.contains("add a") && message.contains("add b"),
            "{message}"
        );
        assert!(!message.contains('#'), "{message}");
        assert_eq!(subjects(&repo).lines().count(), 3);
    }

    #[test]
    fn reword_an_older_commit_and_head() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        let (_, commits) = three_commits(&repo);
        write(&repo, "local.txt", "uncommitted\n");
        git(&repo, &["add", "local.txt"]);
        assert_eq!(
            reword(&repo, &commits[1], "B, reworded").unwrap(),
            Outcome::Done
        );
        assert_eq!(subjects(&repo), "add c\nB, reworded\nadd a\nbase\n");
        // The autostash brought the local change back.
        assert_eq!(read(&repo, "local.txt"), "uncommitted\n");
        let head_before = head(&repo);
        reword(&repo, &head_before, "C, reworded").unwrap();
        assert_eq!(subjects(&repo).lines().next(), Some("C, reworded"));
        // `--only`: the staged file didn't go into the amended commit.
        assert_eq!(
            git(&repo, &["diff", "--cached", "--name-only"]),
            "local.txt\n"
        );
    }

    #[test]
    fn edit_stops_and_continues() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        let (base, commits) = three_commits(&repo);
        let mut plan = rebase_plan(&repo, &commits[0]).unwrap();
        plan[1].action = RebaseAction::Edit;
        plan[2].action = RebaseAction::Reword;
        plan[2].message = Some("C!".into());
        let outcome = interactive_rebase(&repo, Some(&base), &plan, false).unwrap();
        assert_eq!(outcome, Outcome::Conflicts);
        let op = operation(&repo);
        assert_eq!(op.state, RepoState::Rebasing);
        assert_eq!(op.rebase_branch.as_deref(), Some("main"));
        assert_eq!(op.step.map(|(step, _)| step), Some(2));
        // The user amends the stopped commit.
        write(&repo, "b.txt", "b, edited\n");
        git(&repo, &["commit", "-q", "-a", "--amend", "--no-edit"]);
        let outcome = continue_operation(&repo, RepoState::Rebasing).unwrap();
        assert_eq!(outcome, Outcome::Done);
        assert_eq!(subjects(&repo), "C!\nadd b\nadd a\nbase\n");
        assert_eq!(read(&repo, "b.txt"), "b, edited\n");
    }

    #[test]
    fn a_reword_survives_a_conflict() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "x.txt", "1\n");
        let base = commit_all(&repo, "base");
        write(&repo, "x.txt", "2\n");
        commit_all(&repo, "two");
        write(&repo, "x.txt", "3\n");
        commit_all(&repo, "three");
        let mut plan = rebase_plan(&repo, &oid_of(&repo, "HEAD~1")).unwrap();
        // Swapped: "three" no longer applies cleanly on base.
        plan.swap(0, 1);
        plan[0].action = RebaseAction::Reword;
        plan[0].message = Some("three, first".into());
        let outcome = interactive_rebase(&repo, Some(&base), &plan, false).unwrap();
        assert_eq!(outcome, Outcome::Conflicts);
        write(&repo, "x.txt", "3\n");
        git(&repo, &["add", "x.txt"]);
        // The next pick conflicts again.
        let outcome = continue_operation(&repo, RepoState::Rebasing).unwrap();
        assert_eq!(outcome, Outcome::Conflicts);
        write(&repo, "x.txt", "2\n");
        git(&repo, &["add", "x.txt"]);
        assert_eq!(
            continue_operation(&repo, RepoState::Rebasing).unwrap(),
            Outcome::Done
        );
        assert_eq!(subjects(&repo), "two\nthree, first\nbase\n");
    }

    #[test]
    fn rebase_from_the_root() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        let (base, _) = three_commits(&repo);
        let mut plan = rebase_plan(&repo, &base).unwrap();
        plan[0].action = RebaseAction::Reword;
        plan[0].message = Some("root".into());
        assert_eq!(
            interactive_rebase(&repo, None, &plan, false).unwrap(),
            Outcome::Done
        );
        assert_eq!(subjects(&repo), "add c\nadd b\nadd a\nroot\n");
    }

    #[test]
    fn squash_needs_a_previous_commit() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        let (base, commits) = three_commits(&repo);
        let mut plan = rebase_plan(&repo, &commits[0]).unwrap();
        plan[0].action = RebaseAction::Fixup;
        assert!(interactive_rebase(&repo, Some(&base), &plan, false).is_err());
        assert_eq!(operation(&repo).state, RepoState::Normal);
    }

    fn oid_of(repo: &Repo, rev: &str) -> String {
        crate::testing::oid(repo, rev)
    }
}
