//! Push, as the Push dialog of JetBrains IDEs does it: the commits that would go, the target branch
//! on a remote (its upstream, or a new branch of the same name), force only with lease and only on
//! request, progress while it runs.

use crate::cli::{Cancel, GitError};
use crate::repo::Repo;
use crate::status::{FileStatus, parse_name_status};

/// `git log --format` of a [`CommitInfo`]: hash, short hash, subject, author, commit time, each
/// followed by NUL ([`parse_commits`]).
pub(crate) const COMMIT_FORMAT: &str = "--format=%H%x00%h%x00%s%x00%an%x00%ct%x00";

/// A remote: `origin` and where it points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub url: String,
}

/// A commit in a list (outgoing commits, later the log).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    pub oid: String,
    pub short: String,
    pub summary: String,
    pub author: String,
    /// Seconds since the epoch.
    pub time: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRequest {
    pub remote: String,
    /// The local branch to push (`main`).
    pub local_branch: String,
    /// The branch on the remote (`main`, `feature/x`).
    pub remote_branch: String,
    /// Make the remote branch the upstream of the local one (`-u`): a new branch.
    pub set_upstream: bool,
    /// `--force-with-lease`: overwrite the remote branch, unless someone pushed to it since the last
    /// fetch.
    pub force_with_lease: bool,
    /// Push tags too (`--follow-tags`: annotated tags of the pushed commits).
    pub tags: bool,
}

/// Progress of a push, parsed from git's stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushProgress {
    /// "Writing objects", "Compressing objects", …
    pub phase: String,
    pub percent: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushResult {
    /// What was pushed: "main → origin/main"; "Everything up-to-date" when there was nothing to push.
    pub summary: String,
    pub up_to_date: bool,
}

/// The remotes of the repository, in config order.
pub fn remotes(repo: &Repo) -> Result<Vec<Remote>, GitError> {
    let output = repo
        .git()
        .read_only()
        .args(["remote", "-v"])
        .output_string()?;
    let mut remotes: Vec<Remote> = Vec::new();
    for line in output.lines() {
        // `origin\tgit@github.com:x/y.git (push)`
        let Some((name, rest)) = line.split_once('\t') else {
            continue;
        };
        let url = rest
            .trim_end_matches(" (push)")
            .trim_end_matches(" (fetch)")
            .to_string();
        if !remotes.iter().any(|remote| remote.name == name) {
            remotes.push(Remote {
                name: name.to_string(),
                url,
            });
        }
    }
    Ok(remotes)
}

/// The commits a push of HEAD to `remote/remote_branch` would send, newest first. When the remote
/// has no such branch yet, the commits that aren't on any of its branches.
pub fn outgoing(
    repo: &Repo,
    remote: &str,
    remote_branch: &str,
) -> Result<Vec<CommitInfo>, GitError> {
    let target = format!("refs/remotes/{remote}/{remote_branch}");
    let exists = repo
        .git()
        .read_only()
        .args(["rev-parse", "--verify", "-q", &target])
        .output()
        .is_ok();
    let mut command = repo.git().read_only().args(["log", COMMIT_FORMAT, "HEAD"]);
    command = if exists {
        command.arg(format!("^{target}"))
    } else {
        command.args(["--not", &format!("--remotes={remote}")])
    };
    match command.output_string() {
        Ok(output) => Ok(parse_commits(&output)),
        // No commits at all yet.
        Err(GitError::Failed { .. }) => Ok(Vec::new()),
        Err(err) => Err(err),
    }
}

/// `%H %h %s %an %ct`, each followed by NUL.
pub(crate) fn parse_commits(output: &str) -> Vec<CommitInfo> {
    let fields: Vec<&str> = output.split('\0').collect();
    fields
        .chunks(5)
        .filter(|chunk| chunk.len() == 5)
        .map(|chunk| CommitInfo {
            oid: chunk[0].trim().to_string(),
            short: chunk[1].to_string(),
            summary: chunk[2].to_string(),
            author: chunk[3].to_string(),
            time: chunk[4].trim().parse().unwrap_or(0),
        })
        .filter(|commit| !commit.oid.is_empty())
        .collect()
}

/// The files a commit changed, with their status (relative paths, `/`).
pub fn commit_files(repo: &Repo, oid: &str) -> Result<Vec<(FileStatus, String)>, GitError> {
    let output = repo
        .git()
        .read_only()
        .args([
            "diff-tree",
            "--no-commit-id",
            "--name-status",
            "-r",
            "-z",
            "--root",
            "-M",
            oid,
        ])
        .output_string()?;
    Ok(parse_name_status(&output)
        .into_iter()
        .map(|file| (file.status, file.path))
        .collect())
}

/// Pushes. `on_progress` gets the phases as git reports them; `cancel` stops the push.
pub fn push(
    repo: &Repo,
    request: &PushRequest,
    mut on_progress: impl FnMut(PushProgress) + Send,
    cancel: &Cancel,
) -> Result<PushResult, GitError> {
    let mut command = repo.git().args(["push", "--progress", "--porcelain"]);
    if request.set_upstream {
        command = command.arg("--set-upstream");
    }
    if request.force_with_lease {
        command = command.arg("--force-with-lease");
    }
    if request.tags {
        command = command.arg("--follow-tags");
    }
    let refspec = format!(
        "refs/heads/{}:refs/heads/{}",
        request.local_branch, request.remote_branch
    );
    command = command.args([&request.remote, &refspec]);
    let name = command.display_name();
    let output = command.run_unchecked(Some(cancel), progress_reader(&mut on_progress))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.success {
        // `--porcelain` reports a rejected ref on stdout ("!\trefs/heads/main:…\t[rejected]
        // (fetch first)"); git's hints on stderr may be turned off (`advice.pushUpdateRejected`).
        // The rejection lines come first: the window recognizes a rejected push by them.
        let rejected: Vec<&str> = stdout
            .lines()
            .filter(|line| line.starts_with('!'))
            .map(|line| line.rsplit('\t').next().unwrap_or(line))
            .collect();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let mut message = rejected
            .iter()
            .map(|reason| {
                format!(
                    "! {} → {}/{} {reason}",
                    request.local_branch, request.remote, request.remote_branch
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !stderr.is_empty() {
            if !message.is_empty() {
                message.push('\n');
            }
            message.push_str(&stderr);
        }
        return Err(GitError::Failed {
            command: name,
            message,
        });
    }
    let up_to_date = stdout.lines().any(|line| line.starts_with('='));
    let summary = if up_to_date {
        "Everything up-to-date".to_string()
    } else {
        format!(
            "{} → {}/{}",
            request.local_branch, request.remote, request.remote_branch
        )
    };
    Ok(PushResult {
        summary,
        up_to_date,
    })
}

/// Reads git's stderr as it comes and reports its progress lines (push, fetch, pull: "Writing
/// objects:  45% (9/20)", "Receiving objects: …").
pub(crate) fn progress_reader(
    mut on_progress: impl FnMut(PushProgress) + Send,
) -> impl FnMut(&[u8]) + Send {
    let mut pending = String::new();
    move |chunk| {
        pending.push_str(&String::from_utf8_lossy(chunk));
        // Progress lines end with `\r` while they update, with `\n` when done.
        while let Some(end) = pending.find(['\r', '\n']) {
            let line: String = pending.drain(..=end).collect();
            if let Some(progress) = parse_progress(line.trim()) {
                on_progress(progress);
            }
        }
    }
}

/// "Writing objects:  45% (9/20)" → the phase and the percent.
pub(crate) fn parse_progress(line: &str) -> Option<PushProgress> {
    let line = line.strip_prefix("remote: ").unwrap_or(line);
    let (phase, rest) = line.split_once(':')?;
    if phase.is_empty() || phase.contains(' ') && !phase.chars().next()?.is_uppercase() {
        return None;
    }
    let percent = rest
        .split_whitespace()
        .find_map(|word| word.strip_suffix('%'))
        .and_then(|percent| percent.parse().ok());
    percent.map(|percent| PushProgress {
        phase: phase.trim().to_string(),
        percent: Some(percent),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::tests::init_repo;
    use std::fs;

    #[test]
    fn progress_lines_are_parsed() {
        assert_eq!(
            parse_progress("Writing objects:  45% (9/20)"),
            Some(PushProgress {
                phase: "Writing objects".into(),
                percent: Some(45)
            })
        );
        assert_eq!(parse_progress("To github.com:x/y.git"), None);
        assert_eq!(
            parse_progress("remote: Resolving deltas: 100% (3/3)").map(|p| p.percent),
            Some(Some(100))
        );
    }

    #[test]
    fn a_rejected_push_says_why_without_git_hints() {
        let sandbox = crate::testing::Sandbox::new();
        let (repo, remote) = sandbox.with_remote();
        let teammate = sandbox.clone(&remote, "teammate");
        crate::testing::write(&teammate, "b.txt", "theirs\n");
        crate::testing::commit_all(&teammate, "Theirs");
        crate::testing::git(&teammate, &["push", "-q", "origin", "main"]);
        crate::testing::write(&repo, "c.txt", "ours\n");
        crate::testing::commit_all(&repo, "Ours");
        // The hints on stderr are off: the reason must still be in the error.
        crate::testing::git(&repo, &["config", "advice.pushUpdateRejected", "false"]);
        let request = PushRequest {
            remote: "origin".into(),
            local_branch: "main".into(),
            remote_branch: "main".into(),
            set_upstream: false,
            force_with_lease: false,
            tags: false,
        };
        let error = push(&repo, &request, |_| {}, &Cancel::new()).unwrap_err();
        let details = error.details().unwrap();
        assert!(details.contains("[rejected]"), "{details}");
        assert!(details.contains("fetch first"), "{details}");
        assert!(error.to_string().contains("main → origin/main"), "{error}");
    }

    #[test]
    fn push_to_a_local_remote() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let server = root.join("server.git");
        crate::cli::GitCommand::new(&root)
            .args(["init", "-q", "--bare", "-b", "main", "server.git"])
            .output()
            .unwrap();
        let work = root.join("work");
        fs::create_dir(&work).unwrap();
        let repo = init_repo(&work);
        repo.git()
            .args(["remote", "add", "origin", server.to_str().unwrap()])
            .output()
            .unwrap();
        fs::write(work.join("a.txt"), "a\n").unwrap();
        repo.git().args(["add", "."]).output().unwrap();
        repo.git()
            .args(["commit", "-q", "-m", "first"])
            .output()
            .unwrap();
        assert_eq!(remotes(&repo).unwrap()[0].name, "origin");
        let outgoing_commits = outgoing(&repo, "origin", "main").unwrap();
        assert_eq!(outgoing_commits.len(), 1);
        assert_eq!(outgoing_commits[0].summary, "first");
        let files = commit_files(&repo, &outgoing_commits[0].oid).unwrap();
        assert_eq!(files, vec![(FileStatus::Added, "a.txt".to_string())]);
        let request = PushRequest {
            remote: "origin".into(),
            local_branch: "main".into(),
            remote_branch: "main".into(),
            set_upstream: true,
            force_with_lease: false,
            tags: false,
        };
        let result = push(&repo, &request, |_| {}, &Cancel::new()).unwrap();
        assert!(!result.up_to_date);
        assert!(outgoing(&repo, "origin", "main").unwrap().is_empty());
        let again = push(&repo, &request, |_| {}, &Cancel::new()).unwrap();
        assert!(again.up_to_date);
    }
}
