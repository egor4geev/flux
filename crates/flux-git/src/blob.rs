//! File contents at a revision: the base of the gutter markers and of the diff viewer.
//!
//! One long-lived `git cat-file --batch` per repository answers every request: starting git costs
//! ~15–20 ms, so a process per file would make opening the diff of fifty files take a second.
//!
//! The content is as stored, without the working tree's conversions: `--batch --filters` dies on
//! `<rev>:<path>` requests ("missing path", git 2.54). A repository with `core.autocrlf` keeps
//! `\n` in HEAD and `\r\n` on disk — the caller compares lines without the `\r`.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout};
use std::sync::Mutex;

use crate::cli::GitError;
use crate::repo::Repo;

/// How many leading bytes are checked for NUL to call content binary (as git does).
const BINARY_PROBE: usize = 8000;

/// Reads files at revisions of one repository. Blocking; requests from several threads take turns.
pub struct BlobReader {
    repo: Repo,
    process: Mutex<Option<Batch>>,
}

struct Batch {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Drop for Batch {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

impl BlobReader {
    pub fn new(repo: &Repo) -> Self {
        Self {
            repo: repo.clone(),
            process: Mutex::new(None),
        }
    }

    /// The content of `path` (relative to the working tree, `/`) at `rev` ("HEAD", a commit hash,
    /// "" for the index). `Ok(None)` — no such file there (a new file; no commits yet).
    pub fn read(&self, rev: &str, path: &str) -> Result<Option<Vec<u8>>, GitError> {
        // The batch protocol is line-based: a path with a line break goes by a separate command.
        if path.contains('\n') {
            return self.read_once(rev, path);
        }
        let mut process = self.process.lock().unwrap_or_else(|err| err.into_inner());
        // A process that died (or a broken exchange) is started again once.
        for attempt in 0..2 {
            if process.is_none() {
                *process = Some(self.start()?);
            }
            let batch = process.as_mut().expect("started above");
            match request(batch, &format!("{rev}:{path}")) {
                Ok(content) => return Ok(content),
                Err(err) if attempt == 1 => return Err(err),
                Err(_) => *process = None,
            }
        }
        unreachable!("the loop returns on the second attempt")
    }

    fn start(&self) -> Result<Batch, GitError> {
        let mut child = self
            .repo
            .git()
            .read_only()
            .args(["cat-file", "--batch"])
            .spawn_interactive()?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        Ok(Batch {
            child,
            stdin,
            stdout,
        })
    }

    /// One `git cat-file blob <rev>:<path>`.
    fn read_once(&self, rev: &str, path: &str) -> Result<Option<Vec<u8>>, GitError> {
        match self
            .repo
            .git()
            .read_only()
            .args(["cat-file", "blob", &format!("{rev}:{path}")])
            .output()
        {
            Ok(content) => Ok(Some(content)),
            Err(GitError::Failed { .. }) => Ok(None),
            Err(err) => Err(err),
        }
    }
}

/// Asks the batch process for one object: `<oid> <type> <size>\n<content>\n`, or `<name> missing\n`.
fn request(batch: &mut Batch, object: &str) -> Result<Option<Vec<u8>>, GitError> {
    writeln!(batch.stdin, "{object}")?;
    batch.stdin.flush()?;
    let mut header = String::new();
    if batch.stdout.read_line(&mut header)? == 0 {
        return Err(GitError::Io(std::io::Error::other("git cat-file exited")));
    }
    let header = header.trim_end();
    if header.ends_with(" missing") || header.ends_with(" ambiguous") {
        return Ok(None);
    }
    let mut parts = header.rsplitn(3, ' ');
    let size: usize = parts
        .next()
        .and_then(|size| size.parse().ok())
        .ok_or_else(|| GitError::Io(std::io::Error::other(format!("bad header: {header}"))))?;
    let kind = parts.next().unwrap_or("");
    let mut content = vec![0; size];
    batch.stdout.read_exact(&mut content)?;
    let mut newline = [0u8; 1];
    batch.stdout.read_exact(&mut newline)?;
    // A directory or a submodule at that path has no file content.
    Ok((kind == "blob").then_some(content))
}

/// Whether content is binary: a NUL byte among the first 8000 bytes, as git decides.
pub fn is_binary(content: &[u8]) -> bool {
    content[..content.len().min(BINARY_PROBE)].contains(&0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::tests::init_repo;
    use std::fs;

    #[test]
    fn files_are_read_at_head() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let repo = init_repo(&root);
        let reader = BlobReader::new(&repo);
        // No commits yet: nothing at HEAD.
        assert_eq!(reader.read("HEAD", "a.txt").unwrap(), None);
        fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        fs::write(root.join("b.bin"), b"\0\x01").unwrap();
        repo.git().args(["add", "."]).output().unwrap();
        repo.git()
            .args(["commit", "-q", "-m", "first"])
            .output()
            .unwrap();
        fs::write(root.join("a.txt"), "changed\n").unwrap();
        let reader = BlobReader::new(&repo);
        assert_eq!(
            reader.read("HEAD", "a.txt").unwrap().as_deref(),
            Some(&b"one\ntwo\n"[..])
        );
        assert_eq!(reader.read("HEAD", "missing.txt").unwrap(), None);
        let binary = reader.read("HEAD", "b.bin").unwrap().unwrap();
        assert!(is_binary(&binary));
        assert!(!is_binary(b"text"));
        // The same process answers again.
        assert!(reader.read("HEAD", "a.txt").unwrap().is_some());
    }
}
