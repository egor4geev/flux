//! History: the log of a repository with filters (the Git window), one commit in detail, the
//! branches that contain a commit, the history of a file and of a range of its lines.
//!
//! Commits come newest first in `--date-order` (a commit before its parents, as the graph needs),
//! page by page (`skip`, `limit`).

use crate::cli::GitError;
use crate::repo::Repo;
use crate::status::{FileChange, FileStatus, parse_name_status};

/// A commit in the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogCommit {
    pub oid: String,
    /// Parents in order: the first one is the branch the commit was made on; a merge has two or
    /// more; a root commit has none.
    pub parents: Vec<String>,
    pub summary: String,
    pub author: String,
    pub author_email: String,
    /// Seconds since the epoch.
    pub author_time: i64,
    pub committer: String,
    pub commit_time: i64,
}

impl LogCommit {
    /// The first 8 characters of the hash, as JetBrains shows it.
    pub fn short(&self) -> &str {
        &self.oid[..self.oid.len().min(8)]
    }
}

/// What the log shows: every filter narrows it down.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogFilter {
    /// Where to start: branch names, tags, commits. Empty — every branch (local and remote), tag
    /// and HEAD, as JetBrains' "All" branches.
    pub revs: Vec<String>,
    /// Authors (any of them): a part of the name or e-mail, case-insensitive.
    pub authors: Vec<String>,
    /// Commits made (committer date) at or after this time, seconds since the epoch.
    pub since: Option<i64>,
    /// …and at or before this one.
    pub until: Option<i64>,
    /// Commits that touched these paths (relative to the working tree, `/`; a directory — anything
    /// in it).
    pub paths: Vec<String>,
    /// A part of the message (case-insensitive, not a pattern), or a hash prefix (4+ hex
    /// characters): commits whose hash starts with it are found too.
    pub text: Option<String>,
}

/// One commit in detail: the commit pane of the Git window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDetails {
    pub commit: LogCommit,
    /// The whole message (subject and body), trailing blank lines trimmed.
    pub message: String,
    pub committer_email: String,
    /// The files changed against the first parent (a root commit: everything it adds), renames
    /// with their old path.
    pub files: Vec<FileChange>,
}

/// A commit in the history of a file: where the file was then (it may have been renamed since) and
/// how the commit changed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRevision {
    pub commit: LogCommit,
    /// The file's path in this commit, relative, `/`.
    pub path: String,
    /// The path before this commit renamed it.
    pub orig_path: Option<String>,
    pub status: FileStatus,
}

/// The fields of a commit for `--format`: a record starts with RS (`\x1e`), fields are separated by
/// US (`\x1f`) — none of them can contain those (`%s` is one line).
const LOG_FORMAT: &str = "--format=%x1e%H%x1f%P%x1f%s%x1f%an%x1f%ae%x1f%at%x1f%cn%x1f%ct";
const RECORD: char = '\x1e';
const FIELD: char = '\x1f';

/// A page of the log: at most `limit` commits after the first `skip`.
pub fn log(
    repo: &Repo,
    filter: &LogFilter,
    skip: usize,
    limit: usize,
) -> Result<Vec<LogCommit>, GitError> {
    let revs = start_revs(repo, filter);
    if revs.is_empty() {
        // No commits yet.
        return Ok(Vec::new());
    }
    let text = filter
        .text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let mut command = log_command(repo, filter)
        .arg(format!("--skip={skip}"))
        .arg(format!("--max-count={limit}"));
    if let Some(text) = text {
        command = command.arg(format!("--grep={text}"));
    }
    let output = command
        .args(&revs)
        .arg("--")
        .args(&filter.paths)
        .output_string()?;
    let mut commits: Vec<LogCommit> = records(&output)
        .filter_map(|(header, _)| parse_commit(header))
        .collect();
    // A hash prefix: the commits it names are found whatever their message — on the first page,
    // and kept off the others.
    let by_hash = match text.filter(|text| looks_like_hash(text)) {
        Some(prefix) => commits_by_prefix(repo, filter, prefix)?,
        None => Vec::new(),
    };
    if !by_hash.is_empty() {
        commits.retain(|commit| !by_hash.iter().any(|found| found.oid == commit.oid));
        if skip == 0 {
            commits.splice(0..0, by_hash);
            commits.truncate(limit);
        }
    }
    Ok(commits)
}

/// `git log` with the format and the filters other than the text and the paths.
fn log_command(repo: &Repo, filter: &LogFilter) -> crate::cli::GitCommand {
    let mut command = repo.git().read_only().literal_pathspecs().args([
        "log",
        "--date-order",
        "--no-show-signature",
        "--no-color",
        LOG_FORMAT,
        // Authors and the text are plain words, not patterns, in any case.
        "--fixed-strings",
        "--regexp-ignore-case",
    ]);
    for author in filter
        .authors
        .iter()
        .filter(|author| !author.trim().is_empty())
    {
        command = command.arg(format!("--author={}", author.trim()));
    }
    if let Some(since) = filter.since {
        command = command.arg(format!("--since=@{since}"));
    }
    if let Some(until) = filter.until {
        command = command.arg(format!("--until=@{until}"));
    }
    command
}

/// The revisions the log starts from: the filter's, or every branch, remote branch and tag and
/// HEAD (left out before the first commit — git would fail on it).
fn start_revs(repo: &Repo, filter: &LogFilter) -> Vec<String> {
    if !filter.revs.is_empty() {
        return filter.revs.clone();
    }
    if crate::ops::head_oid(repo).is_none() {
        return Vec::new();
    }
    ["--branches", "--remotes", "--tags", "HEAD"]
        .map(String::from)
        .to_vec()
}

fn looks_like_hash(text: &str) -> bool {
    (4..=40).contains(&text.len()) && text.chars().all(|c| c.is_ascii_hexdigit())
}

/// The commits whose hash starts with `prefix` that pass the filter's other conditions.
fn commits_by_prefix(
    repo: &Repo,
    filter: &LogFilter,
    prefix: &str,
) -> Result<Vec<LogCommit>, GitError> {
    // Every object with the prefix, then only commits among them.
    let Ok(objects) = repo
        .git()
        .read_only()
        .args([
            "rev-parse",
            &format!("--disambiguate={}", prefix.to_lowercase()),
        ])
        .output_string()
    else {
        return Ok(Vec::new());
    };
    if objects.trim().is_empty() {
        return Ok(Vec::new());
    }
    let types = repo
        .git()
        .read_only()
        .args(["cat-file", "--batch-check=%(objectname) %(objecttype)"])
        .stdin(objects)
        .output_string()?;
    let oids: Vec<&str> = types
        .lines()
        .filter_map(|line| line.strip_suffix(" commit"))
        .collect();
    if oids.is_empty() {
        return Ok(Vec::new());
    }
    let output = log_command(repo, filter)
        .arg("--no-walk")
        .args(&oids)
        .arg("--")
        .args(&filter.paths)
        .output_string()?;
    Ok(records(&output)
        .filter_map(|(header, _)| parse_commit(header))
        .collect())
}

/// The records of `LOG_FORMAT` output: the commit's fields and what follows them (a name-status
/// list after NUL, a patch after a newline).
fn records(output: &str) -> impl Iterator<Item = (&str, &str)> {
    output
        .split(RECORD)
        .filter(|record| !record.is_empty())
        .map(|record| {
            let end = record.find(['\0', '\n']).unwrap_or(record.len());
            (&record[..end], &record[end..])
        })
}

/// A commit from the fields of `LOG_FORMAT`.
fn parse_commit(header: &str) -> Option<LogCommit> {
    let fields: Vec<&str> = header.split(FIELD).collect();
    let [
        oid,
        parents,
        summary,
        author,
        author_email,
        author_time,
        committer,
        commit_time,
    ] = fields[..]
    else {
        return None;
    };
    if oid.is_empty() {
        return None;
    }
    Some(LogCommit {
        oid: oid.to_string(),
        parents: parents.split_whitespace().map(str::to_string).collect(),
        summary: summary.to_string(),
        author: author.to_string(),
        author_email: author_email.to_string(),
        author_time: author_time.trim().parse().unwrap_or(0),
        committer: committer.to_string(),
        commit_time: commit_time.trim().parse().unwrap_or(0),
    })
}

/// A commit with its whole message and changed files.
pub fn commit_details(repo: &Repo, oid: &str) -> Result<CommitDetails, GitError> {
    let output = repo
        .git()
        .read_only()
        .args([
            "show",
            "-s",
            "--no-show-signature",
            "--no-color",
            // The header, then the committer's e-mail and the whole message after NULs.
            &format!("{LOG_FORMAT}%x00%ce%x00%B"),
            oid,
            "--",
        ])
        .output_string()?;
    let (header, rest) = records(&output).next().ok_or_else(|| GitError::Failed {
        command: "git show".into(),
        message: format!("No commit {oid}"),
    })?;
    let commit = parse_commit(header).ok_or_else(|| GitError::Failed {
        command: "git show".into(),
        message: format!("Unexpected output for {oid}"),
    })?;
    let mut rest = rest.strip_prefix('\0').unwrap_or(rest).splitn(2, '\0');
    let committer_email = rest.next().unwrap_or_default().to_string();
    let message = rest.next().unwrap_or_default().trim_end().to_string();
    let mut command = repo.git().read_only().args([
        "diff-tree",
        "-r",
        "-z",
        "-M",
        "--name-status",
        "--no-commit-id",
    ]);
    command = match commit.parents.first() {
        Some(parent) => command.args([parent.as_str(), commit.oid.as_str()]),
        None => command.args(["--root", commit.oid.as_str()]),
    };
    let files = parse_name_status(&command.output_string()?);
    Ok(CommitDetails {
        commit,
        message,
        committer_email,
        files,
    })
}

/// The branches that contain a commit, local then remote ("main", "origin/main"): "In 3 branches"
/// of the commit pane.
pub fn branches_containing(repo: &Repo, oid: &str) -> Result<Vec<String>, GitError> {
    let output = repo
        .git()
        .read_only()
        .args([
            "for-each-ref",
            "--format=%(refname)%00%(symref)",
            "--contains",
            oid,
            "refs/heads",
            "refs/remotes",
        ])
        .output_string()?;
    let mut local = Vec::new();
    let mut remote = Vec::new();
    for line in output.lines() {
        let (name, symref) = line.split_once('\0').unwrap_or((line, ""));
        // A remote's HEAD only points at one of its branches.
        if !symref.is_empty() {
            continue;
        }
        if let Some(name) = name.strip_prefix("refs/heads/") {
            local.push(name.to_string());
        } else if let Some(name) = name.strip_prefix("refs/remotes/") {
            remote.push(name.to_string());
        }
    }
    local.extend(remote);
    Ok(local)
}

/// Whether a commit is on a remote already (reachable from a remote branch): rewriting it
/// (reword, squash, drop) would need a force push — the window asks first.
pub fn is_pushed(repo: &Repo, oid: &str) -> Result<bool, GitError> {
    let output = repo
        .git()
        .read_only()
        .args([
            "for-each-ref",
            "--count=1",
            "--format=%(refname)",
            "--contains",
            oid,
            "refs/remotes",
        ])
        .output_string()?;
    Ok(!output.trim().is_empty())
}

/// The history of a file (relative path), through renames (`--follow`), newest first; a page.
pub fn file_history(
    repo: &Repo,
    path: &str,
    skip: usize,
    limit: usize,
) -> Result<Vec<FileRevision>, GitError> {
    if crate::ops::head_oid(repo).is_none() {
        return Ok(Vec::new());
    }
    let output = repo
        .git()
        .read_only()
        .literal_pathspecs()
        .args([
            "log",
            "--follow",
            "-M",
            "--name-status",
            "-z",
            "--no-show-signature",
            "--no-color",
            LOG_FORMAT,
            &format!("--skip={skip}"),
            &format!("--max-count={limit}"),
            "HEAD",
            "--",
            path,
        ])
        .output_string()?;
    // A commit without a file list (a merge) keeps the path of the newer one.
    let mut current = path.to_string();
    let mut revisions = Vec::new();
    for (header, files) in records(&output) {
        let Some(commit) = parse_commit(header) else {
            continue;
        };
        let change = parse_name_status(files).into_iter().next();
        let revision = match change {
            Some(change) => FileRevision {
                commit,
                path: change.path,
                orig_path: change.orig_path,
                status: change.status,
            },
            None => FileRevision {
                commit,
                path: current.clone(),
                orig_path: None,
                status: FileStatus::Modified,
            },
        };
        current = revision
            .orig_path
            .clone()
            .unwrap_or_else(|| revision.path.clone());
        revisions.push(revision);
    }
    Ok(revisions)
}

/// The commits that changed lines `start..=end` (1-based) of a file as HEAD has them
/// (`git log -L`), newest first, at most `limit`.
pub fn line_history(
    repo: &Repo,
    path: &str,
    start: u32,
    end: u32,
    limit: usize,
) -> Result<Vec<FileRevision>, GitError> {
    // `-L` always prints the patch: the file's path in each commit is read from its header.
    let output = repo
        .git()
        .read_only()
        .args([
            "log",
            "--no-show-signature",
            "--no-color",
            LOG_FORMAT,
            &format!("--max-count={limit}"),
            &format!("-L{start},{end}:{path}"),
            "HEAD",
        ])
        .output_string()?;
    let mut current = path.to_string();
    let mut revisions = Vec::new();
    for (header, patch) in records(&output) {
        let Some(commit) = parse_commit(header) else {
            continue;
        };
        let (mut old, mut new) = (None, None);
        for line in patch.lines() {
            if line.starts_with("@@") {
                break;
            }
            if let Some(name) = line.strip_prefix("--- ") {
                old = patch_path(name, "a/");
            } else if let Some(name) = line.strip_prefix("+++ ") {
                new = patch_path(name, "b/");
            }
        }
        let (path, orig_path, status) = match (old, new) {
            (None, Some(new)) => (new, None, FileStatus::Added),
            (Some(old), Some(new)) if old != new => (new, Some(old), FileStatus::Renamed),
            (_, Some(new)) => (new, None, FileStatus::Modified),
            (Some(old), None) => (old, None, FileStatus::Deleted),
            (None, None) => (current.clone(), None, FileStatus::Modified),
        };
        current = orig_path.clone().unwrap_or_else(|| path.clone());
        revisions.push(FileRevision {
            commit,
            path,
            orig_path,
            status,
        });
    }
    Ok(revisions)
}

/// A path from a patch header (`a/src/x.rs`, `"b/odd\tname"`); `None` — `/dev/null`.
fn patch_path(name: &str, prefix: &str) -> Option<String> {
    let name = name.trim_end();
    if name == "/dev/null" {
        return None;
    }
    let name = name
        .strip_prefix('"')
        .and_then(|name| name.strip_suffix('"'))
        .unwrap_or(name);
    Some(name.strip_prefix(prefix).unwrap_or(name).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Sandbox, commit_all, git, write};

    const T0: i64 = 1_700_000_000;

    fn commit_as(repo: &Repo, name: &str, email: &str, time: i64, message: &str) -> String {
        git(repo, &["add", "-A"]);
        // Seconds since the epoch must look like one to git (small numbers don't).
        let date = format!("@{} +0000", T0 + time);
        crate::cli::GitCommand::new(&repo.work_dir)
            .env("GIT_AUTHOR_NAME", name)
            .env("GIT_AUTHOR_EMAIL", email)
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .args(["commit", "-q", "--allow-empty", "-m", message])
            .output()
            .unwrap();
        crate::testing::head(repo)
    }

    fn summaries(commits: &[LogCommit]) -> Vec<&str> {
        commits
            .iter()
            .map(|commit| commit.summary.as_str())
            .collect()
    }

    #[test]
    fn records_parse() {
        let output = "\x1eaaa\x1fbbb ccc\x1fSubject\x1fAnn\x1fann@x\x1f100\x1fCom\x1f200\n\
                      \x1eddd\x1f\x1fRoot\x1fAnn\x1fann@x\x1f1\x1fCom\x1f2\n";
        let commits: Vec<LogCommit> = records(output)
            .filter_map(|(header, _)| parse_commit(header))
            .collect();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].parents, ["bbb", "ccc"]);
        assert_eq!(commits[0].author_time, 100);
        assert_eq!(commits[0].commit_time, 200);
        assert_eq!(commits[0].committer, "Com");
        assert!(commits[1].parents.is_empty());
        assert_eq!(commits[1].short(), "ddd");
    }

    #[test]
    fn an_empty_repository_has_no_log() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        assert!(log(&repo, &LogFilter::default(), 0, 10).unwrap().is_empty());
        assert!(file_history(&repo, "a.txt", 0, 10).unwrap().is_empty());
    }

    #[test]
    fn the_log_shows_every_branch_and_filters() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "a.txt", "1\n");
        let base = commit_as(&repo, "Alice", "alice@x.dev", 1_000, "Base commit");
        git(&repo, &["checkout", "-q", "-b", "feature"]);
        write(&repo, "src/b.txt", "b\n");
        let feature = commit_as(&repo, "Bob", "bob@y.dev", 2_000, "Feature: add b");
        git(&repo, &["checkout", "-q", "main"]);
        write(&repo, "a.txt", "2\n");
        let fix = commit_as(&repo, "Alice", "alice@x.dev", 3_000, "Fix the bug");
        git(&repo, &["merge", "-q", "--no-edit", "feature"]);
        let merge = crate::testing::head(&repo);
        git(&repo, &["checkout", "-q", "-b", "other", &base]);
        let other = commit_as(&repo, "Carol", "carol@z.dev", 500, "Old other");
        git(&repo, &["checkout", "-q", "main"]);

        let all = log(&repo, &LogFilter::default(), 0, 100).unwrap();
        assert_eq!(all.len(), 5);
        assert_eq!(all[0].oid, merge);
        assert_eq!(all[0].parents, [fix.clone(), feature.clone()]);
        // A commit comes before its parents, whatever the dates.
        let position = |oid: &str| all.iter().position(|commit| commit.oid == oid).unwrap();
        assert!(position(&other) < position(&base));
        assert_eq!(all.last().unwrap().oid, base);
        assert_eq!(all[position(&feature)].author, "Bob");
        assert_eq!(all[position(&feature)].author_email, "bob@y.dev");
        assert_eq!(all[position(&feature)].author_time, T0 + 2_000);

        // Pages.
        let first = log(&repo, &LogFilter::default(), 0, 2).unwrap();
        let second = log(&repo, &LogFilter::default(), 2, 2).unwrap();
        let rest = log(&repo, &LogFilter::default(), 4, 2).unwrap();
        let paged: Vec<LogCommit> = first.into_iter().chain(second).chain(rest).collect();
        assert_eq!(paged, all);

        let only = |filter: LogFilter| {
            let commits = log(&repo, &filter, 0, 100).unwrap();
            summaries(&commits)
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        // A branch.
        assert_eq!(
            only(LogFilter {
                revs: vec!["other".into()],
                ..Default::default()
            }),
            ["Old other", "Base commit"]
        );
        // Authors: any of them, by a part of the name or e-mail, in any case.
        assert_eq!(
            only(LogFilter {
                authors: vec!["BOB".into(), "carol@".into()],
                ..Default::default()
            }),
            ["Feature: add b", "Old other"]
        );
        // Dates.
        assert_eq!(
            only(LogFilter {
                since: Some(T0 + 1_500),
                until: Some(T0 + 2_500),
                ..Default::default()
            }),
            ["Feature: add b"]
        );
        // A path: a directory.
        assert_eq!(
            only(LogFilter {
                paths: vec!["src".into()],
                ..Default::default()
            }),
            ["Feature: add b"]
        );
        // Text: a part of the message, not a pattern; with an author.
        assert_eq!(
            only(LogFilter {
                text: Some("fix THE".into()),
                ..Default::default()
            }),
            ["Fix the bug"]
        );
        assert_eq!(
            only(LogFilter {
                text: Some("feature: add".into()),
                authors: vec!["alice".into()],
                ..Default::default()
            }),
            Vec::<String>::new()
        );
        assert_eq!(
            only(LogFilter {
                text: Some("b.*".into()),
                ..Default::default()
            }),
            Vec::<String>::new()
        );
        // A hash prefix.
        assert_eq!(
            only(LogFilter {
                text: Some(feature[..7].to_uppercase()),
                ..Default::default()
            }),
            ["Feature: add b"]
        );
        let by_hash = LogFilter {
            text: Some(fix[..10].to_string()),
            ..Default::default()
        };
        assert_eq!(only(by_hash.clone()), ["Fix the bug"]);
        assert!(log(&repo, &by_hash, 1, 10).unwrap().is_empty());
    }

    #[test]
    fn details_of_a_commit() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "a.txt", "1\n");
        write(&repo, "old.txt", "a\nb\nc\nd\n");
        let root = commit_all(&repo, "Root");
        git(&repo, &["mv", "old.txt", "new.txt"]);
        write(&repo, "a.txt", "2\n");
        git(&repo, &["add", "-A"]);
        git(
            &repo,
            &[
                "commit",
                "-q",
                "-m",
                "Subject line",
                "-m",
                "Body text.\nMore.",
            ],
        );
        let second = crate::testing::head(&repo);

        let details = commit_details(&repo, &second).unwrap();
        assert_eq!(details.commit.oid, second);
        assert_eq!(details.commit.parents, std::slice::from_ref(&root));
        assert_eq!(details.commit.summary, "Subject line");
        assert_eq!(details.message, "Subject line\n\nBody text.\nMore.");
        assert_eq!(details.committer_email, "test@flux.dev");
        let mut files = details.files.clone();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "a.txt");
        assert_eq!(files[0].status, FileStatus::Modified);
        assert_eq!(files[1].path, "new.txt");
        assert_eq!(files[1].status, FileStatus::Renamed);
        assert_eq!(files[1].orig_path.as_deref(), Some("old.txt"));

        let details = commit_details(&repo, &root).unwrap();
        assert_eq!(details.files.len(), 2);
        assert!(
            details
                .files
                .iter()
                .all(|file| file.status == FileStatus::Added)
        );
        assert!(commit_details(&repo, "nope").is_err());
    }

    #[test]
    fn branches_containing_and_pushed() {
        let sandbox = Sandbox::new();
        let (repo, _) = sandbox.with_remote();
        let first = crate::testing::head(&repo);
        git(&repo, &["checkout", "-q", "-b", "feature"]);
        write(&repo, "b.txt", "b\n");
        let local = commit_all(&repo, "Local only");
        assert_eq!(
            branches_containing(&repo, &first).unwrap(),
            ["feature", "main", "origin/main"]
        );
        assert_eq!(branches_containing(&repo, &local).unwrap(), ["feature"]);
        assert!(is_pushed(&repo, &first).unwrap());
        assert!(!is_pushed(&repo, &local).unwrap());
    }

    #[test]
    fn file_history_follows_renames() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "old.txt", "one\ntwo\nthree\nfour\n");
        let added = commit_all(&repo, "Add");
        write(&repo, "other.txt", "x\n");
        commit_all(&repo, "Unrelated");
        write(&repo, "old.txt", "one\nTWO\nthree\nfour\n");
        let changed = commit_all(&repo, "Change");
        git(&repo, &["mv", "old.txt", "new.txt"]);
        let renamed = commit_all(&repo, "Rename");
        write(&repo, "new.txt", "one\nTWO\nthree\nFOUR\n");
        let last = commit_all(&repo, "Last");

        let history = file_history(&repo, "new.txt", 0, 10).unwrap();
        let oids: Vec<&str> = history.iter().map(|rev| rev.commit.oid.as_str()).collect();
        assert_eq!(oids, [&last, &renamed, &changed, &added]);
        assert_eq!(history[0].path, "new.txt");
        assert_eq!(history[0].status, FileStatus::Modified);
        assert_eq!(history[1].status, FileStatus::Renamed);
        assert_eq!(history[1].orig_path.as_deref(), Some("old.txt"));
        assert_eq!(history[2].path, "old.txt");
        assert_eq!(history[3].status, FileStatus::Added);
        let page = file_history(&repo, "new.txt", 1, 2).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].commit.oid, renamed);
    }

    #[test]
    fn line_history_finds_the_commits_of_lines() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "old.txt", "one\ntwo\nthree\nfour\nfive\n");
        let added = commit_all(&repo, "Add");
        write(&repo, "old.txt", "one\nTWO\nthree\nfour\nfive\n");
        let second = commit_all(&repo, "Second line");
        write(&repo, "old.txt", "one\nTWO\nthree\nfour\nFIVE\n");
        commit_all(&repo, "Fifth line");
        git(&repo, &["mv", "old.txt", "new.txt"]);
        commit_all(&repo, "Rename");

        let history = line_history(&repo, "new.txt", 1, 2, 10).unwrap();
        let oids: Vec<&str> = history.iter().map(|rev| rev.commit.oid.as_str()).collect();
        assert_eq!(oids, [&second, &added]);
        assert_eq!(history[0].path, "old.txt");
        assert_eq!(history[1].status, FileStatus::Added);
        assert_eq!(history[0].commit.summary, "Second line");
    }
}
