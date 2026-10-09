//! Branches and tags, as the Branches popup of JetBrains IDEs works with them: the local and remote
//! branches with their upstreams and how far apart they are, the recently checked out ones, tags;
//! checkout (a branch, a remote branch as a new tracking branch, a tag or a commit), new branch,
//! rename, delete (and restore), merge and rebase, comparing two revisions.
//!
//! Operations that local changes would get in the way of fail with git's error: [`crate::blocked_by`]
//! tells which files, and the window decides (smart checkout, force, cancel).

use crate::cli::{Cancel, GitError};
use crate::ops::{Outcome, head_oid, integration_outcome, rev_parse};
use crate::push::{COMMIT_FORMAT, CommitInfo, PushProgress, parse_commits, progress_reader};
use crate::repo::Repo;
use crate::status::{FileChange, FileStatus, parse_name_status};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefKind {
    Local,
    Remote,
    Tag,
}

/// A branch or a tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ref {
    pub kind: RefKind,
    /// The short name: "main", "feature/x"; a remote branch with its remote: "origin/feature/x";
    /// a tag: "v1.0".
    pub name: String,
    /// The remote of a remote branch ("origin").
    pub remote: Option<String>,
    /// The commit (of a tag: the commit it points to).
    pub oid: String,
    /// A local branch's upstream ("origin/main") and how many commits it is ahead of it (outgoing)
    /// and behind it (incoming, as of the last fetch).
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// The upstream is configured but gone (deleted on the remote and pruned).
    pub upstream_gone: bool,
    /// HEAD is this branch.
    pub current: bool,
    /// The tip commit: its committer date (seconds since the epoch) and subject.
    pub time: i64,
    pub subject: String,
}

impl Ref {
    /// The name without the remote: "origin/feature/x" → "feature/x" (a local branch or a tag — as
    /// is).
    pub fn short_name(&self) -> &str {
        match &self.remote {
            Some(remote) => self
                .name
                .strip_prefix(remote.as_str())
                .and_then(|rest| rest.strip_prefix('/'))
                .unwrap_or(&self.name),
            None => &self.name,
        }
    }
}

/// The branches and tags of a repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Refs {
    /// Sorted by name; a remote's `HEAD` symref is left out.
    pub local: Vec<Ref>,
    pub remote: Vec<Ref>,
    pub tags: Vec<Ref>,
    /// Local branches checked out recently, newest first, without the current one (from the HEAD
    /// reflog; at most [`RECENT_LIMIT`]).
    pub recent: Vec<String>,
    /// The remotes, in config order.
    pub remotes: Vec<String>,
}

/// How many recent branches [`refs`] lists.
pub const RECENT_LIMIT: usize = 5;

impl Refs {
    pub fn current(&self) -> Option<&Ref> {
        self.local.iter().find(|branch| branch.current)
    }

    pub fn local(&self, name: &str) -> Option<&Ref> {
        self.local.iter().find(|branch| branch.name == name)
    }

    pub fn remote(&self, name: &str) -> Option<&Ref> {
        self.remote.iter().find(|branch| branch.name == name)
    }

    /// A branch or tag pointing at `oid` (local branches first): to name a commit ("the rebase is
    /// onto origin/main").
    pub fn name_of(&self, oid: &str) -> Option<&str> {
        self.local
            .iter()
            .chain(&self.remote)
            .chain(&self.tags)
            .find(|reference| reference.oid == oid)
            .map(|reference| reference.name.as_str())
    }
}

/// The fields of a ref for `for-each-ref`, NUL-separated (a line per ref): the full name, the
/// object, the commit an annotated tag points to, the symref target (a remote's `HEAD`), the
/// upstream and how far apart it is, the HEAD mark, the date and the subject.
const REF_FORMAT: &str = "--format=%(refname)%00%(objectname)%00%(*objectname)%00%(symref)%00\
    %(upstream:short)%00%(upstream:track,nobracket)%00%(HEAD)%00%(creatordate:unix)%00%(subject)";

/// Reads the branches, tags, remotes and recent branches (`for-each-ref`, the HEAD reflog).
pub fn refs(repo: &Repo) -> Result<Refs, GitError> {
    let remotes = remote_names(repo)?;
    let output = repo
        .git()
        .read_only()
        .args([
            "for-each-ref",
            "--sort=refname",
            REF_FORMAT,
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ])
        .output_string()?;
    let mut refs = parse_refs(&output, &remotes);
    let current = refs.current().map(|branch| branch.name.clone());
    refs.recent = recent_branches(repo, &refs.local, current.as_deref());
    refs.remotes = remotes;
    Ok(refs)
}

fn remote_names(repo: &Repo) -> Result<Vec<String>, GitError> {
    let output = repo.git().read_only().arg("remote").output_string()?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect())
}

/// Parses `for-each-ref` output in [`REF_FORMAT`]; the remote of a remote branch is the longest
/// remote name it starts with (a branch name may have slashes).
fn parse_refs(output: &str, remotes: &[String]) -> Refs {
    let mut refs = Refs::default();
    for line in output.lines() {
        let fields: Vec<&str> = line.split('\0').collect();
        let [
            refname,
            oid,
            peeled,
            symref,
            upstream,
            track,
            head,
            time,
            subject,
        ] = fields[..]
        else {
            continue;
        };
        if !symref.is_empty() {
            continue;
        }
        let (ahead, behind, gone) = parse_track(track);
        let (kind, name, remote) = if let Some(name) = refname.strip_prefix("refs/heads/") {
            (RefKind::Local, name, None)
        } else if let Some(name) = refname.strip_prefix("refs/remotes/") {
            let remote = remotes
                .iter()
                .filter(|remote| {
                    name.strip_prefix(remote.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
                })
                .max_by_key(|remote| remote.len())
                .cloned()
                .or_else(|| name.split_once('/').map(|(remote, _)| remote.to_string()));
            (RefKind::Remote, name, remote)
        } else if let Some(name) = refname.strip_prefix("refs/tags/") {
            (RefKind::Tag, name, None)
        } else {
            continue;
        };
        let reference = Ref {
            kind,
            name: name.to_string(),
            remote,
            oid: if peeled.is_empty() { oid } else { peeled }.to_string(),
            upstream: (!upstream.is_empty()).then(|| upstream.to_string()),
            ahead,
            behind,
            upstream_gone: gone,
            current: head == "*",
            time: time.trim().parse().unwrap_or(0),
            subject: subject.to_string(),
        };
        match kind {
            RefKind::Local => refs.local.push(reference),
            RefKind::Remote => refs.remote.push(reference),
            RefKind::Tag => refs.tags.push(reference),
        }
    }
    refs
}

/// "ahead 2, behind 1", "gone", "" → (ahead, behind, gone).
fn parse_track(track: &str) -> (u32, u32, bool) {
    let mut ahead = 0;
    let mut behind = 0;
    let mut gone = false;
    for part in track.split(',').map(str::trim) {
        if let Some(count) = part.strip_prefix("ahead ") {
            ahead = count.parse().unwrap_or(0);
        } else if let Some(count) = part.strip_prefix("behind ") {
            behind = count.parse().unwrap_or(0);
        } else if part == "gone" {
            gone = true;
        }
    }
    (ahead, behind, gone)
}

/// Local branches checked out recently, newest first: the targets of "checkout: moving from A to
/// B" in the HEAD reflog (git writes these messages in English whatever the locale).
fn recent_branches(repo: &Repo, local: &[Ref], current: Option<&str>) -> Vec<String> {
    let Ok(log) = repo
        .git()
        .read_only()
        .args(["reflog", "show", "--format=%gs", "-n", "300", "HEAD"])
        .output_string()
    else {
        return Vec::new();
    };
    let mut recent: Vec<String> = Vec::new();
    for line in log.lines() {
        let Some(moving) = line.strip_prefix("checkout: moving from ") else {
            continue;
        };
        // Branch names have no spaces: the first " to " separates the two.
        let Some((_, target)) = moving.split_once(" to ") else {
            continue;
        };
        let target = target.trim();
        if Some(target) == current
            || recent.iter().any(|name| name == target)
            || !local.iter().any(|branch| branch.name == target)
        {
            continue;
        }
        recent.push(target.to_string());
        if recent.len() == RECENT_LIMIT {
            break;
        }
    }
    recent
}

/// Whether `name` can be a new branch's name (the rules of `git check-ref-format --branch`); the
/// reason why not, in English, for the dialog.
pub fn check_branch_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("Enter a branch name");
    }
    if name == "@" || name == "HEAD" {
        return Err("This name is reserved");
    }
    if name.starts_with('-') {
        return Err("A branch name can't start with “-”");
    }
    if name.starts_with('/') || name.ends_with('/') || name.contains("//") {
        return Err("Slashes can't start or end a name or come twice");
    }
    if name.ends_with('.') || name.contains("..") || name.contains("@{") {
        return Err("A branch name can't contain “..”, “@{” or end with “.”");
    }
    if name
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c))
    {
        return Err("A branch name can't contain spaces or ~ ^ : ? * [ \\");
    }
    if name
        .split('/')
        .any(|part| part.starts_with('.') || part.ends_with(".lock"))
    {
        return Err("A part of the name can't start with “.” or end with “.lock”");
    }
    Ok(())
}

/// Checks out a local branch, a tag or a commit (the last two detach HEAD). `force` throws away the
/// local changes in the way (`checkout --force`).
pub fn checkout(repo: &Repo, target: &str, force: bool) -> Result<(), GitError> {
    let mut command = repo.git().arg("checkout");
    if force {
        command = command.arg("--force");
    }
    // `--`: the target is a revision even if a file has its name.
    command.args([target, "--"]).output().map(drop)
}

/// Checks out a remote branch ("origin/feature/x") as a new local branch `local` tracking it.
pub fn checkout_remote(
    repo: &Repo,
    remote_branch: &str,
    local: &str,
    force: bool,
) -> Result<(), GitError> {
    let mut command = repo.git().arg("checkout");
    if force {
        command = command.arg("--force");
    }
    command
        .args(["-b", local, "--track", remote_branch])
        .output()
        .map(drop)
}

/// A new branch `name` at `start` (a branch, a tag, a commit, "HEAD"); `checkout` — switch to it
/// (local changes come along); `overwrite` — reset an existing branch of that name (`-B`).
pub fn create_branch(
    repo: &Repo,
    name: &str,
    start: &str,
    checkout: bool,
    overwrite: bool,
) -> Result<(), GitError> {
    let command = match (checkout, overwrite) {
        (true, false) => repo.git().args(["checkout", "-b"]),
        (true, true) => repo.git().args(["checkout", "-B"]),
        (false, false) => repo.git().arg("branch"),
        (false, true) => repo.git().args(["branch", "--force"]),
    };
    command.args([name, start]).output().map(drop)
}

/// Renames a local branch; `unset_upstream` — it stops tracking (the remote branch keeps the old
/// name).
pub fn rename_branch(
    repo: &Repo,
    old: &str,
    new: &str,
    unset_upstream: bool,
) -> Result<(), GitError> {
    repo.git().args(["branch", "-m", old, new]).output()?;
    if unset_upstream && upstream_of(repo, new).is_some() {
        repo.git()
            .args(["branch", "--unset-upstream", new])
            .output()?;
    }
    Ok(())
}

/// The upstream of a local branch ("origin/main"), if it has one.
fn upstream_of(repo: &Repo, branch: &str) -> Option<String> {
    repo.git()
        .read_only()
        .args([
            "for-each-ref",
            "--format=%(upstream:short)",
            &format!("refs/heads/{branch}"),
        ])
        .output_string()
        .ok()
        .map(|upstream| upstream.trim().to_string())
        .filter(|upstream| !upstream.is_empty())
}

/// A deleted branch: what it takes to restore it, and its upstream (to offer deleting that too).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedBranch {
    pub name: String,
    pub oid: String,
    pub upstream: Option<String>,
}

/// Deletes a local branch. Without `force`, a branch that isn't merged into HEAD (or its upstream)
/// fails — [`is_not_fully_merged`] tells this error apart.
pub fn delete_branch(repo: &Repo, name: &str, force: bool) -> Result<DeletedBranch, GitError> {
    let oid = rev_parse(repo, &format!("refs/heads/{name}")).ok_or_else(|| GitError::Failed {
        command: "git branch -d".into(),
        message: format!("error: branch '{name}' not found"),
    })?;
    let upstream = upstream_of(repo, name);
    repo.git()
        .args(["branch", if force { "-D" } else { "-d" }, name])
        .output()?;
    Ok(DeletedBranch {
        name: name.to_string(),
        oid,
        upstream,
    })
}

/// The error of [`delete_branch`] for a branch with commits found nowhere else.
pub fn is_not_fully_merged(error: &GitError) -> bool {
    matches!(error, GitError::Failed { message, .. } if message.contains("not fully merged"))
}

/// Commits of `branch` that aren't in `into`, newest first (at most `limit`): what deleting it
/// would lose.
pub fn unmerged_commits(
    repo: &Repo,
    branch: &str,
    into: &str,
    limit: usize,
) -> Result<Vec<CommitInfo>, GitError> {
    log_range(repo, branch, into, limit)
}

/// `git log <from> ^<not>` in the [`CommitInfo`] format.
fn log_range(
    repo: &Repo,
    from: &str,
    not: &str,
    limit: usize,
) -> Result<Vec<CommitInfo>, GitError> {
    let output = repo
        .git()
        .read_only()
        .args([
            "log",
            &format!("-n{limit}"),
            COMMIT_FORMAT,
            from,
            &format!("^{not}"),
            "--",
        ])
        .output_string()?;
    Ok(parse_commits(&output))
}

/// Brings a deleted branch back at its commit.
pub fn restore_branch(repo: &Repo, name: &str, oid: &str) -> Result<(), GitError> {
    repo.git().args(["branch", name, oid]).output().map(drop)
}

/// Deletes a tag locally; returns what the tag pointed to — the tag object of an annotated tag, a
/// commit of a lightweight one — for [`restore_tag`].
pub fn delete_tag(repo: &Repo, name: &str) -> Result<String, GitError> {
    let target = repo
        .git()
        .read_only()
        .args(["rev-parse", "--verify", "-q", &format!("refs/tags/{name}")])
        .output_string()
        .map(|oid| oid.trim().to_string())
        .map_err(|_| GitError::Failed {
            command: "git tag -d".into(),
            message: format!("error: tag '{name}' not found"),
        })?;
    repo.git().args(["tag", "-d", name]).output()?;
    Ok(target)
}

/// Brings a deleted tag back exactly as it was (`target` from [`delete_tag`]).
pub fn restore_tag(repo: &Repo, name: &str, target: &str) -> Result<(), GitError> {
    repo.git()
        .args(["update-ref", &format!("refs/tags/{name}"), target, ""])
        .output()
        .map(drop)
}

/// Deletes a branch on its remote (`git push <remote> --delete <branch>`); `branch` — without the
/// remote.
pub fn delete_remote_branch(
    repo: &Repo,
    remote: &str,
    branch: &str,
    mut on_progress: impl FnMut(PushProgress) + Send,
    cancel: &Cancel,
) -> Result<(), GitError> {
    repo.git()
        .args([
            "push",
            "--progress",
            "--delete",
            remote,
            &format!("refs/heads/{branch}"),
        ])
        .run(Some(cancel), progress_reader(&mut on_progress))
        .map(drop)
}

/// Merges `rev` into the current branch (`git merge --no-edit`; with `autostash`, local changes are
/// stashed for the merge and come back after it — also after a merge that stops on conflicts is
/// committed or aborted).
pub fn merge(repo: &Repo, rev: &str, autostash: bool) -> Result<Outcome, GitError> {
    let before = head_oid(repo);
    let mut command = repo.git().no_editor().args(["merge", "--no-edit"]);
    if autostash {
        command = command.arg("--autostash");
    }
    let result = command.arg(rev).output().map(drop);
    integration_outcome(repo, before.as_deref(), result)
}

/// Rebases the current branch onto `onto` — or first checks out `branch` ("Checkout and Rebase
/// onto…"); `autostash` as in [`merge`].
pub fn rebase(
    repo: &Repo,
    onto: &str,
    branch: Option<&str>,
    autostash: bool,
) -> Result<Outcome, GitError> {
    let before = match branch {
        Some(branch) => rev_parse(repo, &format!("refs/heads/{branch}")),
        None => head_oid(repo),
    };
    let mut command = repo.git().no_editor().arg("rebase");
    if autostash {
        command = command.arg("--autostash");
    }
    command = command.arg(onto);
    if let Some(branch) = branch {
        command = command.arg(branch);
    }
    let result = command.output().map(drop);
    integration_outcome(repo, before.as_deref(), result)
}

/// The commit a revision names (`rev-parse --verify <rev>^{commit}`); `None` — no such commit.
pub fn resolve(repo: &Repo, rev: &str) -> Result<Option<String>, GitError> {
    match repo
        .git()
        .read_only()
        .args(["rev-parse", "--verify", "-q", &format!("{rev}^{{commit}}")])
        .output_string()
    {
        Ok(oid) => Ok(Some(oid.trim().to_string()).filter(|oid| !oid.is_empty())),
        Err(GitError::Failed { .. }) => Ok(None),
        Err(err) => Err(err),
    }
}

/// The commits of `a` that aren't in `b` and those of `b` that aren't in `a`, newest first (at most
/// `limit` each): "Compare with Current".
pub fn compare_commits(
    repo: &Repo,
    a: &str,
    b: &str,
    limit: usize,
) -> Result<(Vec<CommitInfo>, Vec<CommitInfo>), GitError> {
    Ok((log_range(repo, a, b, limit)?, log_range(repo, b, a, limit)?))
}

/// The files that differ between `from` and `to` (`None` — the working tree, uncommitted changes
/// included), with their status as a change from `from` to `to`; relative paths, `/`.
pub fn diff_files(
    repo: &Repo,
    from: &str,
    to: Option<&str>,
) -> Result<Vec<(FileStatus, String)>, GitError> {
    Ok(diff_changes(repo, from, to)?
        .into_iter()
        .map(|file| (file.status, file.path))
        .collect())
}

/// [`diff_files`] with the old path of a renamed file (to read the file at `from`).
pub fn diff_changes(
    repo: &Repo,
    from: &str,
    to: Option<&str>,
) -> Result<Vec<FileChange>, GitError> {
    let mut command = repo
        .git()
        .read_only()
        .args(["diff", "--name-status", "-z", "-M", from]);
    if let Some(to) = to {
        command = command.arg(to);
    }
    let output = command.arg("--").output_string()?;
    Ok(parse_name_status(&output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{Blocked, abort_operation, blocked_by, operation};
    use crate::status::RepoState;
    use crate::testing::{Sandbox, branch, commit_all, git, head, oid, read, write};

    #[test]
    fn branch_names_follow_git_rules() {
        for good in ["main", "feature/x", "fix-1.2", "jd/2026.1", "ветка"] {
            assert_eq!(check_branch_name(good), Ok(()), "{good}");
        }
        for bad in [
            "", "-x", "a b", "a..b", "a~1", "a^", "a:b", "a?", "a*", "a[", "a\\b", "/a", "a/",
            "a//b", "a.", ".a", "a/.b", "a.lock", "a@{1}", "@", "HEAD",
        ] {
            assert!(check_branch_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn remote_branches_have_short_names() {
        let reference = Ref {
            kind: RefKind::Remote,
            name: "origin/feature/x".into(),
            remote: Some("origin".into()),
            oid: String::new(),
            upstream: None,
            ahead: 0,
            behind: 0,
            upstream_gone: false,
            current: false,
            time: 0,
            subject: String::new(),
        };
        assert_eq!(reference.short_name(), "feature/x");
    }

    #[test]
    fn refs_list_branches_remotes_tags_and_recent_ones() {
        let sandbox = Sandbox::new();
        let (repo, remote) = sandbox.with_remote();
        let first = head(&repo);
        git(&repo, &["switch", "-q", "-c", "feature/x"]);
        write(&repo, "x.txt", "x\n");
        commit_all(&repo, "X");
        git(&repo, &["push", "-q", "-u", "origin", "feature/x"]);
        git(&repo, &["switch", "-q", "main"]);
        git(&repo, &["switch", "-q", "-c", "bugfix/y"]);
        git(&repo, &["switch", "-q", "main"]);
        write(&repo, "a.txt", "one\ntwo\nthree\nfour\n");
        let second = commit_all(&repo, "Four");
        git(&repo, &["tag", "v1", &first]);
        git(&repo, &["tag", "-a", "v2", "-m", "Release 2"]);
        // A teammate pushes to main, adds a branch and deletes feature/x.
        let other = sandbox.clone(&remote, "other");
        write(&other, "b.txt", "b\n");
        commit_all(&other, "B");
        git(&other, &["push", "-q", "origin", "main"]);
        git(&other, &["push", "-q", "origin", "main:remote-only"]);
        git(&other, &["push", "-q", "origin", "--delete", "feature/x"]);
        git(&repo, &["fetch", "-q", "--prune", "origin"]);
        git(&repo, &["remote", "set-head", "origin", "main"]);

        let refs = refs(&repo).unwrap();
        let names = |list: &[Ref]| list.iter().map(|r| r.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&refs.local), ["bugfix/y", "feature/x", "main"]);
        // The remote's HEAD is a symref: left out.
        assert_eq!(names(&refs.remote), ["origin/main", "origin/remote-only"]);
        assert_eq!(names(&refs.tags), ["v1", "v2"]);
        let main = refs.current().unwrap();
        assert_eq!(main.name, "main");
        assert_eq!(main.oid, second);
        assert_eq!(main.upstream.as_deref(), Some("origin/main"));
        assert_eq!((main.ahead, main.behind, main.upstream_gone), (1, 1, false));
        assert_eq!(main.subject, "Four");
        assert!(main.time > 0);
        let feature = refs.local("feature/x").unwrap();
        assert!(feature.upstream_gone);
        assert!(!feature.current);
        let remote_only = refs.remote("origin/remote-only").unwrap();
        assert_eq!(remote_only.remote.as_deref(), Some("origin"));
        assert_eq!(remote_only.short_name(), "remote-only");
        // Tags point at commits, the annotated one too.
        assert_eq!(refs.tags[0].oid, first);
        assert_eq!(refs.tags[1].oid, second);
        assert_eq!(refs.recent, ["bugfix/y", "feature/x"]);
        assert_eq!(refs.remotes, ["origin"]);
        assert_eq!(refs.name_of(&second), Some("main"));
    }

    #[test]
    fn a_fresh_repository_has_no_refs() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("empty");
        let refs = refs(&repo).unwrap();
        assert!(refs.local.is_empty() && refs.recent.is_empty() && refs.remotes.is_empty());
    }

    #[test]
    fn checkout_of_branches_tags_and_remote_branches() {
        let sandbox = Sandbox::new();
        let (repo, remote) = sandbox.with_remote();
        let first = head(&repo);
        git(&repo, &["switch", "-q", "-c", "other"]);
        write(&repo, "a.txt", "one\nTWO\nthree\n");
        write(&repo, "new.txt", "committed\n");
        commit_all(&repo, "Other");
        git(&repo, &["switch", "-q", "main"]);
        git(&repo, &["tag", "v1"]);

        checkout(&repo, "other", false).unwrap();
        assert_eq!(branch(&repo), "other");
        checkout(&repo, "v1", false).unwrap();
        assert_eq!(branch(&repo), "", "a tag detaches HEAD");
        assert_eq!(head(&repo), first);
        checkout(&repo, "main", false).unwrap();

        // Local changes in the way: the error names them; Force throws them away.
        write(&repo, "a.txt", "one\nmine\nthree\n");
        let error = checkout(&repo, "other", false).unwrap_err();
        assert_eq!(
            blocked_by(&error),
            Some(Blocked {
                local: vec!["a.txt".into()],
                ..Default::default()
            })
        );
        checkout(&repo, "other", true).unwrap();
        assert_eq!(read(&repo, "a.txt"), "one\nTWO\nthree\n");
        checkout(&repo, "main", false).unwrap();

        // An untracked file in the way.
        write(&repo, "new.txt", "mine\n");
        let error = checkout(&repo, "other", false).unwrap_err();
        assert_eq!(
            blocked_by(&error).map(|blocked| blocked.untracked),
            Some(vec!["new.txt".to_string()])
        );
        std::fs::remove_file(repo.absolute("new.txt")).unwrap();

        // A remote branch becomes a local one tracking it.
        let teammate = sandbox.clone(&remote, "teammate");
        git(&teammate, &["switch", "-q", "-c", "feature/remote"]);
        write(&teammate, "r.txt", "r\n");
        commit_all(&teammate, "Remote");
        git(&teammate, &["push", "-q", "origin", "feature/remote"]);
        git(&repo, &["fetch", "-q", "origin"]);
        checkout_remote(&repo, "origin/feature/remote", "feature/remote", false).unwrap();
        assert_eq!(branch(&repo), "feature/remote");
        let refs = refs(&repo).unwrap();
        assert_eq!(
            refs.current().unwrap().upstream.as_deref(),
            Some("origin/feature/remote")
        );
    }

    #[test]
    fn smart_checkout_stashes_and_brings_changes_back() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "a.txt", "one\ntwo\nthree\nfour\nfive\n");
        write(&repo, "b.txt", "b\n");
        commit_all(&repo, "First");
        git(&repo, &["switch", "-q", "-c", "other"]);
        write(&repo, "a.txt", "one\nTWO\nthree\nfour\nfive\n");
        commit_all(&repo, "Other");
        git(&repo, &["switch", "-q", "main"]);
        // A change that doesn't conflict with the other branch comes back cleanly (changes on
        // adjacent lines would: git merges them as one).
        write(&repo, "a.txt", "one\ntwo\nthree\nfour\nFIVE\n");
        assert!(checkout(&repo, "other", false).is_err());
        let request = crate::StashRequest::default();
        let stash = crate::stash::stash_push_oid(&repo, &request)
            .unwrap()
            .unwrap();
        checkout(&repo, "other", false).unwrap();
        let outcome = crate::stash::stash_apply(&repo, &stash, true, false).unwrap();
        assert_eq!(outcome, Outcome::Done);
        assert_eq!(read(&repo, "a.txt"), "one\nTWO\nthree\nfour\nFIVE\n");
        assert!(crate::stashes(&repo).unwrap().is_empty());
        // One that does conflict: the stash stays, the file is to resolve.
        git(&repo, &["checkout", "-q", "--", "a.txt"]);
        write(&repo, "a.txt", "one\nmine\nthree\nfour\nfive\n");
        let stash = crate::stash::stash_push_oid(&repo, &request)
            .unwrap()
            .unwrap();
        checkout(&repo, "main", false).unwrap();
        let outcome = crate::stash::stash_apply(&repo, &stash, true, false).unwrap();
        assert_eq!(outcome, Outcome::Conflicts);
        assert_eq!(crate::stashes(&repo).unwrap()[0].oid, stash);
    }

    #[test]
    fn branches_are_created_renamed_deleted_and_restored() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "a.txt", "a\n");
        let first = commit_all(&repo, "First");
        write(&repo, "a.txt", "b\n");
        let second = commit_all(&repo, "Second");

        create_branch(&repo, "x", "HEAD", false, false).unwrap();
        assert_eq!(branch(&repo), "main");
        assert_eq!(oid(&repo, "x"), second);
        create_branch(&repo, "y", &first, true, false).unwrap();
        assert_eq!(branch(&repo), "y");
        assert_eq!(head(&repo), first);
        assert!(create_branch(&repo, "x", &first, false, false).is_err());
        create_branch(&repo, "x", &first, false, true).unwrap();
        assert_eq!(oid(&repo, "x"), first);

        // Rename, and stop tracking.
        git(&repo, &["branch", "-q", "-u", "main", "y"]);
        rename_branch(&repo, "y", "z", true).unwrap();
        assert_eq!(branch(&repo), "z");
        assert_eq!(upstream_of(&repo, "z"), None);
        git(&repo, &["branch", "-q", "-u", "main", "z"]);
        rename_branch(&repo, "z", "w", false).unwrap();
        assert_eq!(upstream_of(&repo, "w").as_deref(), Some("main"));

        // A branch with a commit of its own isn't fully merged.
        write(&repo, "w.txt", "w\n");
        let own = commit_all(&repo, "Own");
        git(&repo, &["switch", "-q", "main"]);
        git(&repo, &["branch", "-q", "--unset-upstream", "w"]);
        let error = delete_branch(&repo, "w", false).unwrap_err();
        assert!(is_not_fully_merged(&error), "{error:?}");
        let lost = unmerged_commits(&repo, "w", "main", 10).unwrap();
        assert_eq!(lost.len(), 1);
        assert_eq!(lost[0].summary, "Own");
        let deleted = delete_branch(&repo, "w", true).unwrap();
        assert_eq!(deleted.oid, own);
        assert_eq!(deleted.upstream, None);
        assert!(resolve(&repo, "w").unwrap().is_none());
        restore_branch(&repo, "w", &deleted.oid).unwrap();
        assert_eq!(oid(&repo, "w"), own);
        // A merged branch goes without force.
        assert_eq!(delete_branch(&repo, "x", false).unwrap().oid, first);

        // Tags come back as they were: an annotated tag stays annotated.
        git(&repo, &["tag", "-a", "v1", "-m", "One"]);
        let target = delete_tag(&repo, "v1").unwrap();
        assert!(resolve(&repo, "v1").unwrap().is_none());
        restore_tag(&repo, "v1", &target).unwrap();
        assert_eq!(git(&repo, &["cat-file", "-t", "v1"]).trim(), "tag");
        assert!(
            restore_tag(&repo, "v1", &target).is_err(),
            "never overwrites"
        );
    }

    #[test]
    fn merges_tell_how_they_ended() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "a.txt", "one\ntwo\nthree\n");
        commit_all(&repo, "First");
        git(&repo, &["switch", "-q", "-c", "feature"]);
        write(&repo, "f.txt", "f\n");
        commit_all(&repo, "Feature");
        git(&repo, &["switch", "-q", "main"]);

        assert_eq!(merge(&repo, "main", false).unwrap(), Outcome::UpToDate);
        assert_eq!(
            merge(&repo, "feature", false).unwrap(),
            Outcome::FastForward
        );
        // Diverged, different files: a merge commit.
        git(&repo, &["switch", "-q", "-c", "side", "HEAD~1"]);
        write(&repo, "s.txt", "s\n");
        commit_all(&repo, "Side");
        git(&repo, &["switch", "-q", "main"]);
        assert_eq!(merge(&repo, "side", false).unwrap(), Outcome::Done);
        assert_eq!(
            git(&repo, &["rev-list", "--parents", "-n1", "HEAD"])
                .split_whitespace()
                .count(),
            3
        );

        // The same line changed on both sides: conflicts, the merge in progress.
        git(&repo, &["switch", "-q", "-c", "clash"]);
        write(&repo, "a.txt", "one\nclash\nthree\n");
        commit_all(&repo, "Clash");
        git(&repo, &["switch", "-q", "main"]);
        write(&repo, "a.txt", "one\nmain\nthree\n");
        commit_all(&repo, "Main");
        let before = head(&repo);
        assert_eq!(merge(&repo, "clash", false).unwrap(), Outcome::Conflicts);
        let op = operation(&repo);
        assert_eq!(op.state, RepoState::Merging);
        assert_eq!(op.incoming_name.as_deref(), Some("clash"));
        assert_eq!(op.incoming, Some(oid(&repo, "clash")));
        assert!(op.message.unwrap().starts_with("Merge branch 'clash'"));
        abort_operation(&repo, RepoState::Merging).unwrap();
        assert_eq!(operation(&repo).state, RepoState::Normal);
        assert_eq!(head(&repo), before);

        // An untracked file in the way stops a merge; local changes come along with the autostash.
        git(&repo, &["switch", "-q", "-c", "more", "feature"]);
        write(&repo, "m.txt", "m\n");
        commit_all(&repo, "More");
        git(&repo, &["switch", "-q", "main"]);
        write(&repo, "a.txt", "one\nlocal\nthree\n");
        assert_eq!(merge(&repo, "feature", false).unwrap(), Outcome::UpToDate);
        write(&repo, "m.txt", "untracked\n");
        let error = merge(&repo, "more", true).unwrap_err();
        assert_eq!(
            blocked_by(&error).map(|blocked| blocked.untracked),
            Some(vec!["m.txt".to_string()])
        );
        std::fs::remove_file(repo.absolute("m.txt")).unwrap();
        assert_eq!(merge(&repo, "more", true).unwrap(), Outcome::Done);
        assert_eq!(read(&repo, "a.txt"), "one\nlocal\nthree\n");
    }

    #[test]
    fn rebases_continue_skip_and_abort() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "a.txt", "one\ntwo\nthree\n");
        let first = commit_all(&repo, "First");
        git(&repo, &["switch", "-q", "-c", "feature"]);
        write(&repo, "a.txt", "one\nfeature\nthree\n");
        commit_all(&repo, "Feature 1");
        write(&repo, "f.txt", "f\n");
        commit_all(&repo, "Feature 2");
        git(&repo, &["switch", "-q", "main"]);
        assert_eq!(
            rebase(&repo, "feature", None, false).unwrap(),
            Outcome::FastForward
        );
        git(&repo, &["reset", "-q", "--hard", "HEAD~2"]);
        write(&repo, "a.txt", "one\nmain\nthree\n");
        let main = commit_all(&repo, "Main");
        assert_eq!(
            rebase(&repo, "main", None, false).unwrap(),
            Outcome::UpToDate
        );

        // Checkout and rebase: feature onto main, conflicting in its first commit.
        assert_eq!(
            rebase(&repo, "main", Some("feature"), false).unwrap(),
            Outcome::Conflicts
        );
        let op = operation(&repo);
        assert_eq!(op.state, RepoState::Rebasing);
        assert_eq!(op.rebase_branch.as_deref(), Some("feature"));
        assert_eq!(op.rebase_onto.as_deref(), Some(main.as_str()));
        assert_eq!(op.step, Some((1, 2)));
        assert!(op.stopped_at.is_some());
        crate::conflict::mark_resolved(&repo, "a.txt", Some(b"one\nboth\nthree\n")).unwrap();
        assert_eq!(
            crate::ops::continue_operation(&repo, RepoState::Rebasing).unwrap(),
            Outcome::Done
        );
        assert_eq!(operation(&repo).state, RepoState::Normal);
        assert_eq!(branch(&repo), "feature");
        assert_eq!(read(&repo, "a.txt"), "one\nboth\nthree\n");
        assert_eq!(
            git(&repo, &["log", "--format=%s", "main.."])
                .lines()
                .collect::<Vec<_>>(),
            ["Feature 2", "Feature 1"]
        );

        // Skip and abort.
        git(&repo, &["switch", "-q", "-c", "again", &first]);
        let again = head(&repo);
        write(&repo, "a.txt", "one\nagain\nthree\n");
        commit_all(&repo, "Again");
        assert_eq!(
            rebase(&repo, "main", None, false).unwrap(),
            Outcome::Conflicts
        );
        assert_eq!(crate::ops::skip_commit(&repo).unwrap(), Outcome::Done);
        assert_eq!(head(&repo), main, "the only commit was skipped");
        git(&repo, &["reset", "-q", "--hard", &again]);
        write(&repo, "a.txt", "one\nagain\nthree\n");
        let tip = commit_all(&repo, "Again");
        assert_eq!(
            rebase(&repo, "main", None, false).unwrap(),
            Outcome::Conflicts
        );
        abort_operation(&repo, RepoState::Rebasing).unwrap();
        assert_eq!(head(&repo), tip);
        assert_eq!(branch(&repo), "again");

        // A dirty working tree stops a rebase without the autostash.
        write(&repo, "f2.txt", "x\n");
        git(&repo, &["add", "f2.txt"]);
        let error = rebase(&repo, "main", None, false).unwrap_err();
        assert!(
            blocked_by(&error).is_some_and(|blocked| blocked.dirty),
            "{error:?}"
        );
        assert_eq!(
            rebase(&repo, "main", None, true).unwrap(),
            Outcome::Conflicts
        );
        abort_operation(&repo, RepoState::Rebasing).unwrap();
        assert!(
            std::path::Path::new(&repo.absolute("f2.txt")).exists(),
            "the autostash is back"
        );
    }

    #[test]
    fn revisions_are_compared() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "a.txt", "a\n");
        write(
            &repo,
            "old.txt",
            "a file that is renamed\nwith several lines\nof text\n",
        );
        commit_all(&repo, "First");
        git(&repo, &["switch", "-q", "-c", "feature"]);
        git(&repo, &["mv", "old.txt", "new.txt"]);
        write(&repo, "f.txt", "f\n");
        commit_all(&repo, "Feature");
        git(&repo, &["switch", "-q", "main"]);
        write(&repo, "a.txt", "main\n");
        commit_all(&repo, "Main 1");
        write(&repo, "b.txt", "b\n");
        commit_all(&repo, "Main 2");

        let (theirs, ours) = compare_commits(&repo, "feature", "main", 100).unwrap();
        let summaries = |commits: &[CommitInfo]| {
            commits
                .iter()
                .map(|c| c.summary.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(summaries(&theirs), ["Feature"]);
        assert_eq!(summaries(&ours), ["Main 2", "Main 1"]);

        let changes = diff_changes(&repo, "main", Some("feature")).unwrap();
        let renamed = changes
            .iter()
            .find(|c| c.status == FileStatus::Renamed)
            .unwrap();
        assert_eq!(renamed.path, "new.txt");
        assert_eq!(renamed.orig_path.as_deref(), Some("old.txt"));
        let mut files = diff_files(&repo, "main", Some("feature")).unwrap();
        files.sort();
        assert!(files.contains(&(FileStatus::Added, "f.txt".to_string())));
        assert!(files.contains(&(FileStatus::Modified, "a.txt".to_string())));
        // Against the working tree, uncommitted changes count.
        write(&repo, "b.txt", "changed\n");
        let files = diff_files(&repo, "HEAD", None).unwrap();
        assert_eq!(files, [(FileStatus::Modified, "b.txt".to_string())]);

        assert_eq!(
            resolve(&repo, "feature").unwrap(),
            Some(oid(&repo, "feature"))
        );
        assert_eq!(resolve(&repo, "no-such-thing").unwrap(), None);
    }

    #[test]
    fn remote_branches_are_deleted_on_the_remote() {
        let sandbox = Sandbox::new();
        let (repo, remote) = sandbox.with_remote();
        git(&repo, &["push", "-q", "origin", "main:gone"]);
        git(&repo, &["fetch", "-q", "origin"]);
        let mut steps = Vec::new();
        delete_remote_branch(
            &repo,
            "origin",
            "gone",
            |step| steps.push(step),
            &Cancel::new(),
        )
        .unwrap();
        let listed = crate::cli::GitCommand::new(&sandbox.root)
            .args(["ls-remote", "--heads", remote.to_str().unwrap()])
            .output_string()
            .unwrap();
        assert!(!listed.contains("refs/heads/gone"), "{listed}");
        assert!(
            resolve(&repo, "refs/remotes/origin/gone")
                .unwrap()
                .is_none()
        );
    }
}
