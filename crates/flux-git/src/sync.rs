//! Keeping up with the remotes, as JetBrains IDEs do: Fetch (every remote, stale remote branches
//! pruned), Pull (a remote branch into the current one: merge, rebase or fast-forward only), Update
//! Project (the current branch from its upstream, merge or rebase, local changes stashed around it)
//! and Update of a branch that isn't checked out (fast-forward only).
//!
//! Progress comes from git's stderr as for push ([`PushProgress`]).

use std::collections::BTreeMap;

use crate::cli::{Cancel, GitError};
use crate::ops::{Outcome, current_branch, head_oid, integration_outcome, rev_parse};
use crate::push::{PushProgress, progress_reader};
use crate::repo::Repo;

/// What a fetch brought.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchResult {
    /// Remote branches that got new commits, and how many ("origin/main", 3).
    pub updated: Vec<(String, u32)>,
    /// Remote branches that appeared.
    pub new_branches: Vec<String>,
    /// Remote branches that are gone (pruned).
    pub pruned: Vec<String>,
}

impl FetchResult {
    pub fn is_empty(&self) -> bool {
        self.updated.is_empty() && self.new_branches.is_empty() && self.pruned.is_empty()
    }
}

/// Fetches `remote` (`None` — every remote), pruning remote branches that are gone. What changed is
/// found by comparing the remote branches before and after.
pub fn fetch(
    repo: &Repo,
    remote: Option<&str>,
    mut on_progress: impl FnMut(PushProgress) + Send,
    cancel: &Cancel,
) -> Result<FetchResult, GitError> {
    let before = remote_branches(repo)?;
    let command = repo.git().args(["fetch", "--prune", "--progress"]);
    let command = match remote {
        Some(remote) => command.arg(remote),
        None => command.arg("--all"),
    };
    command.run(Some(cancel), progress_reader(&mut on_progress))?;
    let after = remote_branches(repo)?;
    let mut result = FetchResult::default();
    for (name, new) in &after {
        match before.get(name) {
            None => result.new_branches.push(name.clone()),
            Some(old) if old != new => {
                let count = count_commits(repo, &format!("{old}..{new}"));
                result.updated.push((name.clone(), count));
            }
            Some(_) => {}
        }
    }
    result.pruned = before
        .keys()
        .filter(|name| !after.contains_key(*name))
        .cloned()
        .collect();
    Ok(result)
}

/// The remote branches ("origin/main") and their commits; a remote's `HEAD` is left out.
fn remote_branches(repo: &Repo) -> Result<BTreeMap<String, String>, GitError> {
    let output = repo
        .git()
        .read_only()
        .args([
            "for-each-ref",
            "--format=%(refname)%00%(objectname)%00%(symref)",
            "refs/remotes",
        ])
        .output_string()?;
    Ok(output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\0');
            let name = fields.next()?.strip_prefix("refs/remotes/")?;
            let oid = fields.next()?;
            let symref = fields.next().unwrap_or("");
            symref
                .is_empty()
                .then(|| (name.to_string(), oid.to_string()))
        })
        .collect())
}

/// `git rev-list --count <range>`; 0 when it can't be counted.
fn count_commits(repo: &Repo, range: &str) -> u32 {
    repo.git()
        .read_only()
        .args(["rev-list", "--count", range])
        .output_string()
        .ok()
        .and_then(|count| count.trim().parse().ok())
        .unwrap_or(0)
}

/// How the incoming commits join the current branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PullMode {
    #[default]
    Merge,
    Rebase,
    /// Only when the branch can simply move forward.
    FastForwardOnly,
}

/// Pulls `branch` of `remote` into the current branch: fetches it, then merges or rebases (local
/// changes are stashed around it and come back after).
pub fn pull(
    repo: &Repo,
    remote: &str,
    branch: &str,
    mode: PullMode,
    mut on_progress: impl FnMut(PushProgress) + Send,
    cancel: &Cancel,
) -> Result<Outcome, GitError> {
    let before = head_oid(repo);
    // The mode is always explicit: the user's `pull.rebase` doesn't decide for the dialog.
    let mode_args: &[&str] = match mode {
        PullMode::Merge => &["--no-rebase", "--no-edit"],
        PullMode::Rebase => &["--rebase"],
        PullMode::FastForwardOnly => &["--no-rebase", "--ff-only"],
    };
    let result = repo
        .git()
        .no_editor()
        .args(["pull", "--progress", "--autostash"])
        .args(mode_args)
        .args([remote, branch])
        .run(Some(cancel), progress_reader(&mut on_progress))
        .map(drop);
    integration_outcome(repo, before.as_deref(), result)
}

/// Update Project's method for the current branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateMethod {
    Merge,
    Rebase,
}

/// What Update Project did in a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateResult {
    pub outcome: Outcome,
    /// The branch and its upstream: "main", "origin/main".
    pub branch: String,
    pub upstream: String,
    /// How many commits came in, and how many files they changed.
    pub commits: u32,
    pub files: u32,
}

/// Update Project for one repository: fetches the upstream's remote, then merges or rebases the
/// upstream into the current branch, local changes stashed around it. A branch without an upstream
/// fails with [`GitError::NoUpstream`]; a detached HEAD — with a `Failed` error.
pub fn update(
    repo: &Repo,
    method: UpdateMethod,
    mut on_progress: impl FnMut(PushProgress) + Send,
    cancel: &Cancel,
) -> Result<UpdateResult, GitError> {
    let branch = current_branch(repo).ok_or_else(|| GitError::Failed {
        command: "git pull".into(),
        message: "HEAD is detached: check out a branch to update it".into(),
    })?;
    let upstream = upstream(repo, &branch).ok_or_else(|| GitError::NoUpstream(branch.clone()))?;
    if upstream.remote != "." {
        repo.git()
            .args(["fetch", "--prune", "--progress", &upstream.remote])
            .run(Some(cancel), progress_reader(&mut on_progress))?;
    }
    if rev_parse(repo, &upstream.full).is_none() {
        return Err(GitError::Failed {
            command: "git pull".into(),
            message: format!("The upstream {} is gone", upstream.short),
        });
    }
    let commits = count_commits(repo, &format!("HEAD..{}", upstream.full));
    let mut result = UpdateResult {
        outcome: Outcome::UpToDate,
        branch,
        upstream: upstream.short.clone(),
        commits,
        files: 0,
    };
    if commits == 0 {
        return Ok(result);
    }
    result.files = repo
        .git()
        .read_only()
        .args([
            "diff",
            "--name-only",
            "-z",
            &format!("HEAD...{}", upstream.full),
            "--",
        ])
        .output_string()
        .map(|names| names.split('\0').filter(|name| !name.is_empty()).count() as u32)
        .unwrap_or(0);
    let before = head_oid(repo);
    let command = match method {
        UpdateMethod::Merge => repo
            .git()
            .no_editor()
            .args(["merge", "--no-edit", "--autostash"]),
        UpdateMethod::Rebase => repo.git().no_editor().args(["rebase", "--autostash"]),
    };
    let integrated = command
        .arg(&upstream.full)
        .run(Some(cancel), |_| {})
        .map(drop);
    result.outcome = integration_outcome(repo, before.as_deref(), integrated)?;
    Ok(result)
}

/// Update of a local branch that isn't checked out: fetches its upstream and moves it forward
/// (`git fetch <remote> <branch>:<local>`); a branch that has diverged isn't touched (an error).
/// The current branch is fetched and fast-forwarded in place (local changes stashed around it).
pub fn fast_forward(
    repo: &Repo,
    local: &str,
    mut on_progress: impl FnMut(PushProgress) + Send,
    cancel: &Cancel,
) -> Result<Outcome, GitError> {
    let upstream = upstream(repo, local).ok_or_else(|| GitError::NoUpstream(local.to_string()))?;
    if current_branch(repo).as_deref() == Some(local) {
        if upstream.remote != "." {
            repo.git()
                .args(["fetch", "--progress", &upstream.remote])
                .run(Some(cancel), progress_reader(&mut on_progress))?;
        }
        let before = head_oid(repo);
        let result = repo
            .git()
            .args(["merge", "--ff-only", "--autostash", &upstream.full])
            .run(Some(cancel), |_| {})
            .map(drop);
        return integration_outcome(repo, before.as_deref(), result);
    }
    let local_ref = format!("refs/heads/{local}");
    let before = rev_parse(repo, &local_ref);
    repo.git()
        .args([
            "fetch",
            "--progress",
            &upstream.remote,
            &format!("{}:{local_ref}", upstream.merge),
        ])
        .run(Some(cancel), progress_reader(&mut on_progress))?;
    let after = rev_parse(repo, &local_ref);
    Ok(if before == after {
        Outcome::UpToDate
    } else {
        Outcome::FastForward
    })
}

/// A branch's upstream: the remote it comes from ("origin", or "." for a local branch), the branch
/// there (`refs/heads/main`), and the ref that follows it here, full and short
/// (`refs/remotes/origin/main`, "origin/main").
struct Upstream {
    remote: String,
    merge: String,
    full: String,
    short: String,
}

fn upstream(repo: &Repo, branch: &str) -> Option<Upstream> {
    let output = repo
        .git()
        .read_only()
        .args([
            "for-each-ref",
            "--format=%(upstream)%00%(upstream:short)",
            &format!("refs/heads/{branch}"),
        ])
        .output_string()
        .ok()?;
    let (full, short) = output.trim_end_matches('\n').split_once('\0')?;
    if full.is_empty() {
        return None;
    }
    let config = |key: &str| {
        repo.git()
            .read_only()
            .args(["config", "--get", &format!("branch.{branch}.{key}")])
            .output_string()
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    Some(Upstream {
        remote: config("remote").unwrap_or_else(|| ".".into()),
        merge: config("merge")?,
        full: full.to_string(),
        short: short.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Sandbox, branch, commit_all, git, head, oid, read, write};

    fn quiet() -> impl FnMut(PushProgress) + Send {
        |_| {}
    }

    #[test]
    fn fetch_reports_new_updated_and_pruned_branches() {
        let sandbox = Sandbox::new();
        let (repo, remote) = sandbox.with_remote();
        git(&repo, &["push", "-q", "origin", "main:old"]);
        git(&repo, &["fetch", "-q", "origin"]);
        let other = sandbox.clone(&remote, "other");
        write(&other, "b.txt", "b\n");
        commit_all(&other, "B1");
        write(&other, "b.txt", "bb\n");
        commit_all(&other, "B2");
        git(&other, &["push", "-q", "origin", "main", "main:new"]);
        git(&other, &["push", "-q", "origin", "--delete", "old"]);
        let mut steps = 0;
        let result = fetch(&repo, None, |_| steps += 1, &Cancel::new()).unwrap();
        assert_eq!(result.updated, [("origin/main".to_string(), 2)]);
        assert_eq!(result.new_branches, ["origin/new"]);
        assert_eq!(result.pruned, ["origin/old"]);
        assert!(
            fetch(&repo, Some("origin"), quiet(), &Cancel::new())
                .unwrap()
                .is_empty()
        );
        let _ = steps;
    }

    #[test]
    fn update_merges_or_rebases_the_upstream() {
        let sandbox = Sandbox::new();
        let (repo, remote) = sandbox.with_remote();
        let other = sandbox.clone(&remote, "other");
        let cancel = Cancel::new();
        assert_eq!(
            update(&repo, UpdateMethod::Merge, quiet(), &cancel)
                .unwrap()
                .outcome,
            Outcome::UpToDate
        );

        // Ours and theirs diverged: a merge commit, with the local change kept (autostash).
        write(&other, "b.txt", "b\n");
        commit_all(&other, "Theirs");
        git(&other, &["push", "-q", "origin", "main"]);
        write(&repo, "c.txt", "c\n");
        commit_all(&repo, "Ours");
        write(&repo, "a.txt", "one\ntwo\nthree\nlocal\n");
        let result = update(&repo, UpdateMethod::Merge, quiet(), &cancel).unwrap();
        assert_eq!(result.outcome, Outcome::Done);
        assert_eq!((result.commits, result.files), (1, 1));
        assert_eq!(
            (result.branch.as_str(), result.upstream.as_str()),
            ("main", "origin/main")
        );
        assert_eq!(read(&repo, "a.txt"), "one\ntwo\nthree\nlocal\n");
        let parents = git(&repo, &["rev-list", "--parents", "-n1", "HEAD"]);
        assert_eq!(parents.split_whitespace().count(), 3);

        // Rebase keeps the history linear.
        git(&repo, &["push", "-q", "origin", "main"]);
        git(&other, &["pull", "-q", "--no-rebase", "origin", "main"]);
        write(&other, "d.txt", "d\n");
        commit_all(&other, "Theirs 2");
        git(&other, &["push", "-q", "origin", "main"]);
        write(&repo, "e.txt", "e\n");
        commit_all(&repo, "Ours 2");
        let result = update(&repo, UpdateMethod::Rebase, quiet(), &cancel).unwrap();
        assert_eq!(result.outcome, Outcome::Done);
        assert_eq!(
            git(&repo, &["log", "--format=%s", "-2"])
                .lines()
                .collect::<Vec<_>>(),
            ["Ours 2", "Theirs 2"]
        );
        // Only theirs: a fast-forward.
        git(&repo, &["push", "-q", "origin", "main"]);
        git(&other, &["pull", "-q", "--no-rebase", "origin", "main"]);
        write(&other, "f.txt", "f\n");
        commit_all(&other, "Theirs 3");
        git(&other, &["push", "-q", "origin", "main"]);
        let result = update(&repo, UpdateMethod::Rebase, quiet(), &cancel).unwrap();
        assert_eq!(result.outcome, Outcome::FastForward);

        // Conflicts leave the merge in progress.
        write(&other, "a.txt", "theirs\n");
        commit_all(&other, "Theirs 4");
        git(&other, &["push", "-q", "origin", "main"]);
        git(&repo, &["checkout", "-q", "--", "a.txt"]);
        write(&repo, "a.txt", "ours\n");
        commit_all(&repo, "Ours 4");
        let result = update(&repo, UpdateMethod::Merge, quiet(), &cancel).unwrap();
        assert_eq!(result.outcome, Outcome::Conflicts);
        assert_eq!(crate::operation(&repo).state, crate::RepoState::Merging);
        crate::ops::abort_operation(&repo, crate::RepoState::Merging).unwrap();

        // No upstream, a detached HEAD.
        git(&repo, &["switch", "-q", "-c", "local-only"]);
        assert!(matches!(
            update(&repo, UpdateMethod::Merge, quiet(), &cancel),
            Err(GitError::NoUpstream(branch)) if branch == "local-only"
        ));
        git(&repo, &["checkout", "-q", "--detach"]);
        assert!(matches!(
            update(&repo, UpdateMethod::Merge, quiet(), &cancel),
            Err(GitError::Failed { .. })
        ));
    }

    #[test]
    fn pull_brings_a_remote_branch_in() {
        let sandbox = Sandbox::new();
        let (repo, remote) = sandbox.with_remote();
        let other = sandbox.clone(&remote, "other");
        git(&other, &["switch", "-q", "-c", "feature"]);
        write(&other, "f.txt", "f\n");
        commit_all(&other, "Feature");
        git(&other, &["push", "-q", "origin", "feature"]);
        let cancel = Cancel::new();
        assert_eq!(
            pull(
                &repo,
                "origin",
                "feature",
                PullMode::FastForwardOnly,
                quiet(),
                &cancel
            )
            .unwrap(),
            Outcome::FastForward
        );
        assert_eq!(read(&repo, "f.txt"), "f\n");
        assert_eq!(
            pull(
                &repo,
                "origin",
                "feature",
                PullMode::Merge,
                quiet(),
                &cancel
            )
            .unwrap(),
            Outcome::UpToDate
        );
        // Diverged: fast-forward only refuses, merge makes a merge commit, rebase replays ours.
        write(&other, "g.txt", "g\n");
        commit_all(&other, "Feature 2");
        git(&other, &["push", "-q", "origin", "feature"]);
        write(&repo, "h.txt", "h\n");
        let ours = commit_all(&repo, "Ours");
        assert!(
            pull(
                &repo,
                "origin",
                "feature",
                PullMode::FastForwardOnly,
                quiet(),
                &cancel
            )
            .is_err()
        );
        assert_eq!(head(&repo), ours);
        assert_eq!(
            pull(
                &repo,
                "origin",
                "feature",
                PullMode::Rebase,
                quiet(),
                &cancel
            )
            .unwrap(),
            Outcome::Done
        );
        assert_eq!(git(&repo, &["log", "--format=%s", "-1"]).trim(), "Ours");
        assert_eq!(read(&repo, "g.txt"), "g\n");
    }

    #[test]
    fn a_branch_that_isnt_checked_out_is_fast_forwarded() {
        let sandbox = Sandbox::new();
        let (repo, remote) = sandbox.with_remote();
        git(&repo, &["branch", "-q", "feature"]);
        git(&repo, &["push", "-q", "-u", "origin", "feature"]);
        let other = sandbox.clone(&remote, "other");
        git(&other, &["switch", "-q", "feature"]);
        write(&other, "f.txt", "f\n");
        commit_all(&other, "Feature");
        git(&other, &["push", "-q", "origin", "feature"]);
        let cancel = Cancel::new();
        assert_eq!(
            fast_forward(&repo, "feature", quiet(), &cancel).unwrap(),
            Outcome::FastForward
        );
        assert_eq!(oid(&repo, "feature"), oid(&other, "HEAD"));
        assert_eq!(branch(&repo), "main", "nothing checked out");
        assert_eq!(
            fast_forward(&repo, "feature", quiet(), &cancel).unwrap(),
            Outcome::UpToDate
        );
        // Diverged: left alone.
        git(&repo, &["switch", "-q", "feature"]);
        write(&repo, "mine.txt", "m\n");
        let mine = commit_all(&repo, "Mine");
        git(&repo, &["switch", "-q", "main"]);
        write(&other, "g.txt", "g\n");
        commit_all(&other, "Feature 2");
        git(&other, &["push", "-q", "origin", "feature"]);
        assert!(fast_forward(&repo, "feature", quiet(), &cancel).is_err());
        assert_eq!(oid(&repo, "feature"), mine);
        // The current branch moves in place.
        write(&other, "m2.txt", "m\n");
        git(&other, &["switch", "-q", "main"]);
        write(&other, "main.txt", "x\n");
        commit_all(&other, "Main");
        git(&other, &["push", "-q", "origin", "main"]);
        assert_eq!(
            fast_forward(&repo, "main", quiet(), &cancel).unwrap(),
            Outcome::FastForward
        );
        assert_eq!(read(&repo, "main.txt"), "x\n");
        assert!(matches!(
            fast_forward(&repo, "no-upstream", quiet(), &cancel),
            Err(GitError::NoUpstream(_))
        ));
    }
}
