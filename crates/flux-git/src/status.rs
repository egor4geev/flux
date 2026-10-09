//! Working tree status: `git status --porcelain=v2 -z --branch --untracked-files=all`.
//!
//! Like JetBrains IDEs without the staging area, a file's status is its change against HEAD as a
//! whole: whether a modification is staged or not doesn't matter to the commit window, the gutter
//! or the tree ([`StatusEntry::staged`] and [`StatusEntry::unstaged`] keep the details).

use std::path::Path;

use crate::cli::GitError;
use crate::repo::Repo;

/// A file's change against HEAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileStatus {
    Modified,
    /// New in the index (`git add`), not in HEAD.
    Added,
    Deleted,
    /// Renamed in the index (`git mv`); the old path is [`StatusEntry::orig_path`].
    Renamed,
    /// A file became a symlink or the other way round.
    TypeChanged,
    /// Not tracked: "Unversioned Files".
    Untracked,
    /// Unmerged: a merge, rebase, cherry-pick or unstash stopped on a conflict.
    Conflicted,
}

/// A changed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    /// Relative to the working tree, with `/`.
    pub path: String,
    /// The path before a rename or copy.
    pub orig_path: Option<String>,
    pub status: FileStatus,
    /// The index differs from HEAD.
    pub staged: bool,
    /// The working tree differs from the index.
    pub unstaged: bool,
    /// How a conflicted file conflicts ([`FileStatus::Conflicted`] only).
    pub conflict: Option<ConflictKind>,
}

/// How a file conflicts: the two letters of an unmerged entry in `git status` (ours, theirs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConflictKind {
    /// `UU`: both sides changed the file.
    BothModified,
    /// `AA`: both sides added a file at this path.
    BothAdded,
    /// `AU`: added on our side only (the other side has no such file and no base).
    AddedByUs,
    /// `UA`: added on their side only.
    AddedByThem,
    /// `DU`: we deleted it, they changed it.
    DeletedByUs,
    /// `UD`: they deleted it, we changed it.
    DeletedByThem,
    /// `DD`: both deleted it (a rename on both sides).
    BothDeleted,
}

impl ConflictKind {
    /// `UU`, `AA`, … → the kind.
    pub fn from_xy(xy: &str) -> Option<Self> {
        Some(match xy {
            "UU" => ConflictKind::BothModified,
            "AA" => ConflictKind::BothAdded,
            "AU" => ConflictKind::AddedByUs,
            "UA" => ConflictKind::AddedByThem,
            "DU" => ConflictKind::DeletedByUs,
            "UD" => ConflictKind::DeletedByThem,
            "DD" => ConflictKind::BothDeleted,
            _ => return None,
        })
    }

    /// Both sides have a text to merge: the merge tool applies. Otherwise one side is taken whole.
    pub fn mergeable(self) -> bool {
        matches!(self, ConflictKind::BothModified | ConflictKind::BothAdded)
    }

    /// Our side has the file.
    pub fn ours_exists(self) -> bool {
        !matches!(
            self,
            ConflictKind::DeletedByUs | ConflictKind::BothDeleted | ConflictKind::AddedByThem
        )
    }

    /// Their side has the file.
    pub fn theirs_exists(self) -> bool {
        !matches!(
            self,
            ConflictKind::DeletedByThem | ConflictKind::BothDeleted | ConflictKind::AddedByUs
        )
    }
}

/// A file changed between two revisions (a commit's files, a branch comparison, a stash): its
/// path (relative, `/`), the path before a rename or copy, and how it changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub status: FileStatus,
    pub path: String,
    pub orig_path: Option<String>,
}

/// Parses `--name-status -z` output (`git diff`, `git diff-tree`): a status letter (with a score
/// for renames and copies), then the path — two paths, old then new, for a rename or a copy.
pub(crate) fn parse_name_status(output: &str) -> Vec<FileChange> {
    let mut files = Vec::new();
    let mut fields = output.split('\0').filter(|field| !field.is_empty());
    while let Some(code) = fields.next() {
        let letter = code.trim().as_bytes().first().copied();
        let status = match letter {
            Some(b'A' | b'C') => FileStatus::Added,
            Some(b'D') => FileStatus::Deleted,
            Some(b'R') => FileStatus::Renamed,
            Some(b'T') => FileStatus::TypeChanged,
            Some(b'U') => FileStatus::Conflicted,
            _ => FileStatus::Modified,
        };
        let orig_path = match letter {
            Some(b'R' | b'C') => fields.next().map(str::to_string),
            _ => None,
        };
        if let Some(path) = fields.next() {
            files.push(FileChange {
                status,
                path: path.to_string(),
                orig_path,
            });
        }
    }
    files
}

/// The current branch and its upstream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchState {
    /// The commit HEAD points to; `None` before the first commit.
    pub oid: Option<String>,
    /// The checked-out branch; `None` — detached HEAD.
    pub head: Option<String>,
    /// `origin/main`.
    pub upstream: Option<String>,
    /// Commits ahead of and behind the upstream.
    pub ahead: u32,
    pub behind: u32,
}

impl BranchState {
    /// For the title bar: the branch, or the short hash of a detached HEAD.
    pub fn label(&self) -> Option<String> {
        match (&self.head, &self.oid) {
            (Some(head), _) => Some(head.clone()),
            (None, Some(oid)) => Some(oid.chars().take(7).collect()),
            (None, None) => None,
        }
    }
}

/// An operation in progress in the repository.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RepoState {
    #[default]
    Normal,
    Merging,
    Rebasing,
    CherryPicking,
    Reverting,
    Bisecting,
}

/// What `git status` says about a repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoStatus {
    pub branch: BranchState,
    /// Changed files, in git's order (by path).
    pub entries: Vec<StatusEntry>,
    pub state: RepoState,
}

/// Reads the status of a repository: changed and untracked files (each untracked file, not just its
/// directory), the branch, the operation in progress.
pub fn status(repo: &Repo) -> Result<RepoStatus, GitError> {
    let output = repo
        .git()
        .read_only()
        .args([
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=all",
        ])
        .output()?;
    let mut status = parse(&output);
    status.state = repo_state(&repo.git_dir);
    Ok(status)
}

/// Wholly untracked directories (relative, `/`, without the trailing slash): those `git status`
/// would show as one entry without `--untracked-files=all`. The tree colors them as untracked.
pub fn untracked_dirs(repo: &Repo) -> Result<Vec<String>, GitError> {
    let output = repo
        .git()
        .read_only()
        .args([
            "ls-files",
            "-z",
            "--others",
            "--directory",
            "--exclude-standard",
            "--no-empty-directory",
        ])
        .output_string()?;
    Ok(output
        .split('\0')
        .filter_map(|path| path.strip_suffix('/'))
        .map(str::to_string)
        .collect())
}

/// Ignored files and directories (relative, `/`; a directory without its trailing slash, and
/// without its contents): changes inside them don't change the status, so the watcher skips them.
pub fn ignored_paths(repo: &Repo) -> Result<Vec<String>, GitError> {
    let output = repo
        .git()
        .read_only()
        .args([
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ])
        .output_string()?;
    Ok(output
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(|path| path.trim_end_matches('/').to_string())
        .collect())
}

/// Parses `git status --porcelain=v2 -z --branch` output.
pub fn parse(output: &[u8]) -> RepoStatus {
    let mut status = RepoStatus::default();
    let text = String::from_utf8_lossy(output);
    let mut fields = text.split('\0').filter(|field| !field.is_empty());
    while let Some(field) = fields.next() {
        if let Some(header) = field.strip_prefix("# ") {
            parse_header(header, &mut status.branch);
            continue;
        }
        let entry = match field.as_bytes().first() {
            // `1 XY sub mH mI mW hH hI path`
            Some(b'1') => {
                let parts: Vec<&str> = field.splitn(9, ' ').collect();
                (parts.len() == 9).then(|| ordinary(parts[1], parts[8], None))
            }
            // `2 XY sub mH mI mW hH hI Xscore path`, then the original path as the next field.
            Some(b'2') => {
                let parts: Vec<&str> = field.splitn(10, ' ').collect();
                let orig = fields.next().map(str::to_string);
                (parts.len() == 10).then(|| ordinary(parts[1], parts[9], orig))
            }
            // `u XY sub m1 m2 m3 mW h1 h2 h3 path`
            Some(b'u') => {
                let parts: Vec<&str> = field.splitn(11, ' ').collect();
                (parts.len() == 11).then(|| StatusEntry {
                    path: parts[10].to_string(),
                    orig_path: None,
                    status: FileStatus::Conflicted,
                    staged: true,
                    unstaged: true,
                    conflict: ConflictKind::from_xy(parts[1]),
                })
            }
            Some(b'?') => field.get(2..).map(|path| StatusEntry {
                path: path.to_string(),
                orig_path: None,
                status: FileStatus::Untracked,
                staged: false,
                unstaged: true,
                conflict: None,
            }),
            _ => None,
        };
        status.entries.extend(entry);
    }
    status
}

fn parse_header(header: &str, branch: &mut BranchState) {
    let (key, value) = header.split_once(' ').unwrap_or((header, ""));
    match key {
        "branch.oid" if value != "(initial)" => branch.oid = Some(value.to_string()),
        "branch.head" if value != "(detached)" => branch.head = Some(value.to_string()),
        "branch.upstream" => branch.upstream = Some(value.to_string()),
        "branch.ab" => {
            for part in value.split(' ') {
                if let Some(ahead) = part.strip_prefix('+') {
                    branch.ahead = ahead.parse().unwrap_or(0);
                } else if let Some(behind) = part.strip_prefix('-') {
                    branch.behind = behind.parse().unwrap_or(0);
                }
            }
        }
        _ => {}
    }
}

/// A changed tracked file: `xy` is the index (X) and working tree (Y) status letters.
fn ordinary(xy: &str, path: &str, orig_path: Option<String>) -> StatusEntry {
    let mut letters = xy.chars();
    let x = letters.next().unwrap_or('.');
    let y = letters.next().unwrap_or('.');
    let status = match (x, y) {
        ('R', _) => FileStatus::Renamed,
        ('A' | 'C', _) => FileStatus::Added,
        ('D', _) | (_, 'D') => FileStatus::Deleted,
        ('T', _) | (_, 'T') => FileStatus::TypeChanged,
        _ => FileStatus::Modified,
    };
    StatusEntry {
        path: path.to_string(),
        orig_path,
        status,
        staged: x != '.',
        unstaged: y != '.',
        conflict: None,
    }
}

/// The operation in progress, from the marker files git leaves in its directory.
pub fn repo_state(git_dir: &Path) -> RepoState {
    let has = |name: &str| git_dir.join(name).exists();
    if has("rebase-merge") || has("rebase-apply") {
        RepoState::Rebasing
    } else if has("MERGE_HEAD") {
        RepoState::Merging
    } else if has("CHERRY_PICK_HEAD") {
        RepoState::CherryPicking
    } else if has("REVERT_HEAD") {
        RepoState::Reverting
    } else if has("BISECT_LOG") {
        RepoState::Bisecting
    } else {
        RepoState::Normal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::tests::init_repo;
    use std::fs;

    #[test]
    fn porcelain_v2_is_parsed() {
        let output = [
            "# branch.oid 4d7de13a9f00c0ffee",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +2 -1",
            "1 .M N... 100644 100644 100644 aaa bbb src/main.rs",
            "1 A. N... 000000 100644 100644 000 ccc new file.rs",
            "1 .D N... 100644 100644 000000 ddd ddd gone.rs",
            "2 R. N... 100644 100644 100644 eee eee R100 lib/new.rs",
            "lib/old.rs",
            "u UU N... 100644 100644 100644 100644 f1 f2 f3 conflict.rs",
            "? notes/тест.md",
            "",
        ]
        .join("\0");
        let status = parse(output.as_bytes());
        assert_eq!(status.branch.oid.as_deref(), Some("4d7de13a9f00c0ffee"));
        assert_eq!(status.branch.head.as_deref(), Some("main"));
        assert_eq!(status.branch.upstream.as_deref(), Some("origin/main"));
        assert_eq!((status.branch.ahead, status.branch.behind), (2, 1));
        let summary: Vec<(&str, FileStatus)> = status
            .entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.status))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("src/main.rs", FileStatus::Modified),
                ("new file.rs", FileStatus::Added),
                ("gone.rs", FileStatus::Deleted),
                ("lib/new.rs", FileStatus::Renamed),
                ("conflict.rs", FileStatus::Conflicted),
                ("notes/тест.md", FileStatus::Untracked),
            ]
        );
        assert_eq!(status.entries[3].orig_path.as_deref(), Some("lib/old.rs"));
        assert_eq!(status.entries[4].conflict, Some(ConflictKind::BothModified));
        assert!(status.entries[0].unstaged && !status.entries[0].staged);
        assert!(status.entries[1].staged && !status.entries[1].unstaged);
    }

    #[test]
    fn detached_head_and_initial_commit() {
        let status = parse(b"# branch.oid (initial)\0# branch.head (detached)\0");
        assert_eq!(status.branch, BranchState::default());
        assert_eq!(status.branch.label(), None);
        let detached = BranchState {
            oid: Some("4d7de13a9f".into()),
            ..Default::default()
        };
        assert_eq!(detached.label().as_deref(), Some("4d7de13"));
    }

    #[test]
    fn untracked_and_ignored_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let repo = init_repo(&root);
        fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/t.rs"), "t").unwrap();
        repo.git().args(["add", "."]).output().unwrap();
        repo.git()
            .args(["commit", "-q", "-m", "x"])
            .output()
            .unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join("target/debug/a"), "a").unwrap();
        fs::write(root.join("src/b.log"), "b").unwrap();
        fs::create_dir_all(root.join("new/sub")).unwrap();
        fs::write(root.join("new/sub/f.rs"), "f").unwrap();
        fs::write(root.join("src/u.rs"), "u").unwrap();
        assert_eq!(untracked_dirs(&repo).unwrap(), vec!["new".to_string()]);
        assert_eq!(
            ignored_paths(&repo).unwrap(),
            vec!["src/b.log".to_string(), "target".to_string()]
        );
    }

    #[test]
    fn status_of_a_real_repository() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let repo = init_repo(&root);
        fs::write(root.join("a.txt"), "one\n").unwrap();
        repo.git().args(["add", "a.txt"]).output().unwrap();
        repo.git()
            .args(["commit", "-q", "-m", "first"])
            .output()
            .unwrap();
        fs::write(root.join("a.txt"), "two\n").unwrap();
        fs::create_dir(root.join("new")).unwrap();
        fs::write(root.join("new/b.txt"), "b\n").unwrap();
        let status = status(&repo).unwrap();
        assert_eq!(status.branch.head.as_deref(), Some("main"));
        assert_eq!(status.state, RepoState::Normal);
        let summary: Vec<(&str, FileStatus)> = status
            .entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.status))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("a.txt", FileStatus::Modified),
                ("new/b.txt", FileStatus::Untracked)
            ]
        );
    }
}
