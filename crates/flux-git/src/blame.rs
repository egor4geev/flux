//! Annotate (blame): which commit last changed each line of a file — for the editor's gutter. The
//! text may have unsaved edits: git is given the editor's text (`--contents`), and lines that differ
//! from HEAD are "not committed yet".

use std::collections::HashMap;

use crate::cli::GitError;
use crate::repo::Repo;

/// A commit that lines come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameCommit {
    pub oid: String,
    pub author: String,
    pub author_email: String,
    /// Seconds since the epoch.
    pub author_time: i64,
    pub summary: String,
    /// The file's path in that commit (it may have been renamed since).
    pub path: String,
}

/// The annotations of a file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Blame {
    /// The commits, each once.
    pub commits: Vec<BlameCommit>,
    /// For every line of the text (0-based): its commit in [`Self::commits`]; `None` — not
    /// committed yet.
    pub lines: Vec<Option<usize>>,
}

/// Annotates a file (relative path) as `contents` has it (the editor's text; `None` — the file on
/// disk), against HEAD.
pub fn blame(repo: &Repo, path: &str, contents: Option<&str>) -> Result<Blame, GitError> {
    let mut command = repo
        .git()
        .read_only()
        .args(["blame", "--porcelain", "--no-progress"]);
    if let Some(contents) = contents {
        command = command
            .args(["--contents", "-"])
            .stdin(contents.as_bytes().to_vec());
    }
    let output = command.args(["HEAD", "--", path]).output()?;
    Ok(parse_porcelain(&String::from_utf8_lossy(&output)))
}

/// Parses `git blame --porcelain`: a group header (`<oid> <orig line> <final line> [<count>]`),
/// the commit's fields the first time it appears, then the line itself after a tab.
fn parse_porcelain(output: &str) -> Blame {
    let mut blame = Blame::default();
    let mut index: HashMap<String, usize> = HashMap::new();
    // The commit of the line being read and its line number (1-based) in the text.
    let mut current: Option<(String, usize)> = None;
    for line in output.lines() {
        if line.starts_with('\t') {
            if let Some((oid, number)) = current.take() {
                let commit = index.get(&oid).copied().filter(|_| !is_uncommitted(&oid));
                if blame.lines.len() < number {
                    blame.lines.resize(number, None);
                }
                blame.lines[number - 1] = commit;
            }
            continue;
        }
        match &current {
            None => {
                let mut fields = line.split(' ');
                let (Some(oid), Some(_orig), Some(number)) =
                    (fields.next(), fields.next(), fields.next())
                else {
                    continue;
                };
                let Ok(number) = number.parse::<usize>() else {
                    continue;
                };
                if number == 0 {
                    continue;
                }
                if !index.contains_key(oid) {
                    index.insert(oid.to_string(), blame.commits.len());
                    blame.commits.push(BlameCommit {
                        oid: oid.to_string(),
                        author: String::new(),
                        author_email: String::new(),
                        author_time: 0,
                        summary: String::new(),
                        path: String::new(),
                    });
                }
                current = Some((oid.to_string(), number));
            }
            Some((oid, _)) => {
                let commit = &mut blame.commits[index[oid]];
                let (key, value) = line.split_once(' ').unwrap_or((line, ""));
                match key {
                    "author" => commit.author = value.to_string(),
                    "author-mail" => {
                        commit.author_email = value
                            .trim_start_matches('<')
                            .trim_end_matches('>')
                            .to_string()
                    }
                    "author-time" => commit.author_time = value.parse().unwrap_or(0),
                    "summary" => commit.summary = value.to_string(),
                    "filename" => commit.path = value.to_string(),
                    _ => {}
                }
            }
        }
    }
    // The uncommitted pseudo-commit isn't one.
    if let Some(&uncommitted) = index
        .iter()
        .find(|(oid, _)| is_uncommitted(oid))
        .map(|(_, index)| index)
    {
        blame.commits.remove(uncommitted);
        for line in blame.lines.iter_mut().flatten() {
            if *line > uncommitted {
                *line -= 1;
            }
        }
    }
    blame
}

fn is_uncommitted(oid: &str) -> bool {
    oid.bytes().all(|byte| byte == b'0')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Sandbox, commit_all, git, write};

    #[test]
    fn lines_come_from_their_commits() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "a.txt", "one\ntwo\nthree\n");
        let first = commit_all(&repo, "First");
        write(&repo, "a.txt", "one\nTWO\nthree\n");
        let second = commit_all(&repo, "Second");
        let blame = blame(&repo, "a.txt", None).unwrap();
        assert_eq!(blame.lines.len(), 3);
        let oid = |line: usize| blame.commits[blame.lines[line].unwrap()].oid.clone();
        assert_eq!(oid(0), first);
        assert_eq!(oid(1), second);
        assert_eq!(oid(2), first);
        let commit = &blame.commits[blame.lines[1].unwrap()];
        assert_eq!(commit.summary, "Second");
        assert_eq!(commit.author, "Flux Test");
        assert_eq!(commit.author_email, "test@flux.dev");
        assert!(commit.author_time > 0);
        assert_eq!(commit.path, "a.txt");
        assert_eq!(blame.commits.len(), 2);
    }

    #[test]
    fn unsaved_lines_are_not_committed_yet() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "a.txt", "one\ntwo\n");
        let first = commit_all(&repo, "First");
        let blame = blame(&repo, "a.txt", Some("zero\none\ntwo\nnew")).unwrap();
        assert_eq!(blame.lines.len(), 4);
        assert_eq!(blame.lines[0], None);
        assert_eq!(blame.lines[3], None);
        assert_eq!(blame.commits[blame.lines[1].unwrap()].oid, first);
        assert_eq!(blame.commits.len(), 1);
    }

    #[test]
    fn renamed_files_keep_their_history() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("r");
        write(&repo, "old.txt", "one\ntwo\nthree\nfour\n");
        let first = commit_all(&repo, "First");
        git(&repo, &["mv", "old.txt", "new.txt"]);
        write(&repo, "new.txt", "one\ntwo\nthree\nfour\nfive\n");
        commit_all(&repo, "Renamed");
        let blame = blame(&repo, "new.txt", None).unwrap();
        let commit = &blame.commits[blame.lines[0].unwrap()];
        assert_eq!(commit.oid, first);
        assert_eq!(commit.path, "old.txt");
        assert_eq!(blame.commits[blame.lines[4].unwrap()].path, "new.txt");
    }
}
