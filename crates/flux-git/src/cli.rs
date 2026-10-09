//! Running git: which binary, with what environment, how errors and cancellation look.
//!
//! Every command runs in the repository's working tree (`git -C <dir>`) with:
//! - `core.quotepath=false` and no colors: paths and output come as they are;
//! - `GIT_TERMINAL_PROMPT=0` and a new session (`setsid`): git, ssh and credential helpers can't
//!   ask for a password on the terminal Flux was started from — the command fails instead of
//!   hanging;
//! - `LANGUAGE=en`: git's messages are English whatever the user's locale — the few errors Flux
//!   reads (local changes in the way, a branch that isn't merged) are recognized by their words;
//! - stdin closed unless the command gets input ([`GitCommand::stdin`]).
//!
//! Read-only commands ([`GitCommand::read_only`]) don't take optional locks
//! (`--no-optional-locks`): `git status` then doesn't rewrite the index, which would wake the
//! watcher and start another refresh.

use std::ffi::OsStr;
use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// How often a cancellable command checks the flag while it runs.
const CANCEL_POLL: Duration = Duration::from_millis(50);

/// A cancellation flag shared with a running operation: [`Cancel::cancel`] kills the git process.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_canceled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Why a git command failed. `Display` is a short message for the status bar; [`GitError::details`]
/// is git's full output for a dialog (hook output, push rejections).
#[derive(Debug)]
pub enum GitError {
    /// No git on the machine.
    NotInstalled,
    /// git exited with an error: the command (`git commit`) and its stderr (or stdout, if stderr is
    /// empty), trimmed.
    Failed {
        command: String,
        message: String,
    },
    Canceled,
    /// The branch has no upstream to update from or pull (`main` without `origin/main`).
    NoUpstream(String),
    Io(io::Error),
}

impl GitError {
    /// git's whole message, for a dialog: hook output, the reason of a rejected push.
    pub fn details(&self) -> Option<&str> {
        match self {
            GitError::Failed { message, .. } if !message.is_empty() => Some(message),
            _ => None,
        }
    }
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GitError::NotInstalled => write!(f, "Git is not installed"),
            GitError::Failed { command, message } => {
                // The first meaningful line: "fatal: …", "error: …", "! [rejected] …".
                let line = message
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .unwrap_or("failed");
                write!(f, "{command}: {line}")
            }
            GitError::Canceled => write!(f, "Canceled"),
            GitError::NoUpstream(branch) => write!(f, "No tracked branch for {branch}"),
            GitError::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for GitError {}

impl From<io::Error> for GitError {
    fn from(err: io::Error) -> Self {
        GitError::Io(err)
    }
}

/// The git binary: the first `git` on `PATH`, otherwise the usual places (an app started from the
/// Finder gets a short `PATH`). `None` — git isn't installed.
pub fn git_binary() -> Option<&'static Path> {
    static BINARY: OnceLock<Option<PathBuf>> = OnceLock::new();
    BINARY
        .get_or_init(|| {
            let from_path = std::env::var_os("PATH")
                .into_iter()
                .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
                .map(|dir| dir.join("git"));
            let known = [
                "/opt/homebrew/bin/git",
                "/usr/local/bin/git",
                "/usr/bin/git",
            ]
            .into_iter()
            .map(PathBuf::from);
            from_path.chain(known).find(|path| path.is_file())
        })
        .as_deref()
}

/// One git command: `git -C <dir> <global options> <args>`.
pub struct GitCommand {
    dir: PathBuf,
    globals: Vec<String>,
    args: Vec<String>,
    envs: Vec<(String, String)>,
    stdin: Option<Vec<u8>>,
    /// A long-lived process the caller talks to: stdin stays open, stderr is dropped.
    interactive: bool,
}

impl GitCommand {
    /// A command that runs in `dir` (a working tree, or a directory inside one).
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            globals: vec![
                "-c".into(),
                "core.quotepath=false".into(),
                "-c".into(),
                "color.ui=never".into(),
            ],
            args: Vec::new(),
            envs: Vec::new(),
            stdin: None,
            interactive: false,
        }
    }

    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_string_lossy().into_owned());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for arg in args {
            self = self.arg(arg);
        }
        self
    }

    pub fn env(mut self, key: &str, value: impl AsRef<OsStr>) -> Self {
        self.envs
            .push((key.into(), value.as_ref().to_string_lossy().into_owned()));
        self
    }

    /// A command that only reads: no optional locks (`git status` won't refresh the index on disk).
    pub fn read_only(mut self) -> Self {
        self.globals.insert(0, "--no-optional-locks".into());
        self
    }

    /// Paths are taken as they are, not as patterns (`--literal-pathspecs`): a file named `a*b`
    /// or `:x` is just that file.
    pub fn literal_pathspecs(mut self) -> Self {
        self.globals.push("--literal-pathspecs".into());
        self
    }

    /// A command that would open an editor for a message (a merge commit, `rebase --continue`)
    /// takes the prepared message as it is: there is no terminal to edit it in.
    pub fn no_editor(self) -> Self {
        self.env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", "true")
            .env("GIT_MERGE_AUTOEDIT", "no")
    }

    /// Bytes for the command's stdin (a commit message with `-F -`, paths, a blob).
    pub fn stdin(mut self, input: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(input.into());
        self
    }

    /// "git commit": for error messages.
    pub fn display_name(&self) -> String {
        match self.args.first() {
            Some(subcommand) => format!("git {subcommand}"),
            None => "git".into(),
        }
    }

    /// Runs the command and returns its stdout; a non-zero exit is [`GitError::Failed`].
    pub fn output(self) -> Result<Vec<u8>, GitError> {
        self.run(None, |_| {}).map(|output| output.stdout)
    }

    /// The same, as a UTF-8 string (lossy).
    pub fn output_string(self) -> Result<String, GitError> {
        self.output()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Runs the command until it exits or `cancel` is set (then the process is killed and the
    /// result is [`GitError::Canceled`]). `on_stderr` gets stderr as it arrives, in chunks (git
    /// reports progress there: "Writing objects:  45% (9/20)\r").
    pub fn run(
        self,
        cancel: Option<&Cancel>,
        on_stderr: impl FnMut(&[u8]) + Send,
    ) -> Result<Output, GitError> {
        let name = self.display_name();
        let output = self.run_unchecked(cancel, on_stderr)?;
        if output.success {
            Ok(output)
        } else {
            let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).trim().to_string();
            let stderr = text(&output.stderr);
            let message = if stderr.is_empty() {
                text(&output.stdout)
            } else {
                stderr
            };
            Err(GitError::Failed {
                command: name,
                message,
            })
        }
    }

    /// [`Self::run`] that leaves a non-zero exit to the caller (`Output::success`): push reads
    /// why refs were rejected from its stdout.
    pub fn run_unchecked(
        self,
        cancel: Option<&Cancel>,
        on_stderr: impl FnMut(&[u8]) + Send,
    ) -> Result<Output, GitError> {
        let mut child = self.spawn()?;
        match collect(&mut child, cancel, on_stderr) {
            Ok(output) => Ok(output),
            Err(err) => {
                child.kill().ok();
                child.wait().ok();
                Err(err)
            }
        }
    }

    /// Starts the process with pipes for stdin (if there is input), stdout and stderr; the input is
    /// written by a separate thread so that a large stdout can't deadlock against it.
    pub fn spawn(self) -> Result<Child, GitError> {
        let binary = git_binary().ok_or(GitError::NotInstalled)?;
        let mut command = Command::new(binary);
        command
            .arg("-C")
            .arg(&self.dir)
            .args(&self.globals)
            .args(&self.args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_PAGER", "cat")
            .env("PAGER", "cat")
            // English messages whatever the locale: the errors Flux recognizes are matched by
            // their words (gettext's LANGUAGE wins over LANG / LC_ALL, unless the locale is C,
            // which is English anyway).
            .env("LANGUAGE", "en")
            .stdin(if self.stdin.is_some() || self.interactive {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(if self.interactive {
                Stdio::null()
            } else {
                Stdio::piped()
            });
        for (key, value) in &self.envs {
            command.env(key, value);
        }
        new_session(&mut command);
        let mut child = command.spawn().map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => GitError::NotInstalled,
            _ => GitError::Io(err),
        })?;
        if let Some(input) = self.stdin {
            let mut stdin = child.stdin.take().expect("stdin is piped");
            std::thread::spawn(move || {
                stdin.write_all(&input).ok();
            });
        }
        Ok(child)
    }
}

impl GitCommand {
    /// Starts a long-lived process to talk to (`git cat-file --batch`): the caller owns its stdin
    /// and stdout; stderr is dropped, so that nobody has to drain it.
    pub fn spawn_interactive(mut self) -> Result<Child, GitError> {
        self.interactive = true;
        self.stdin = None;
        self.spawn()
    }
}

/// What a finished command printed.
#[derive(Debug, Default)]
pub struct Output {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Reads stdout and stderr to the end (stderr on its own thread, passed to `on_stderr` as it comes)
/// and waits for the exit, checking `cancel` meanwhile.
fn collect(
    child: &mut Child,
    cancel: Option<&Cancel>,
    mut on_stderr: impl FnMut(&[u8]) + Send,
) -> Result<Output, GitError> {
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");
    std::thread::scope(|scope| {
        let reader = scope.spawn(move || {
            let mut out = Vec::new();
            stdout.read_to_end(&mut out).map(|_| out)
        });
        let errors = scope.spawn(move || {
            let mut all = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                match stderr.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        on_stderr(&buf[..n]);
                        all.extend_from_slice(&buf[..n]);
                    }
                }
            }
            all
        });
        let status = loop {
            if cancel.is_some_and(Cancel::is_canceled) {
                child.kill().ok();
                child.wait().ok();
                return Err(GitError::Canceled);
            }
            match child.try_wait()? {
                Some(status) => break status,
                None if cancel.is_some() => std::thread::sleep(CANCEL_POLL),
                None => break child.wait()?,
            }
        };
        let stdout = reader.join().expect("stdout reader")?;
        let stderr = errors.join().expect("stderr reader");
        Ok(Output {
            success: status.success(),
            stdout,
            stderr,
        })
    })
}

/// The child starts its own session: without a controlling terminal, ssh and credential helpers
/// fail instead of prompting on the terminal Flux was started from.
fn new_session(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: `setsid` is async-signal-safe; nothing else runs between fork and exec.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_show_the_first_meaningful_line() {
        let error = GitError::Failed {
            command: "git push".into(),
            message: "\nTo github.com:x/y.git\n ! [rejected] main -> main (fetch first)\n".into(),
        };
        assert_eq!(error.to_string(), "git push: To github.com:x/y.git");
        assert!(error.details().unwrap().contains("[rejected]"));
    }

    #[test]
    fn git_runs_and_reports_failures() {
        let dir = tempfile::tempdir().unwrap();
        let version = GitCommand::new(dir.path())
            .arg("--version")
            .output_string()
            .unwrap();
        assert!(version.starts_with("git version"), "{version}");
        let error = GitCommand::new(dir.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap_err();
        assert!(matches!(error, GitError::Failed { .. }), "{error:?}");
    }

    #[test]
    fn git_speaks_english() {
        let dir = tempfile::tempdir().unwrap();
        // An alias runs a shell with git's environment.
        let language = GitCommand::new(dir.path())
            .args(["-c", "alias.lang=!printf %s \"$LANGUAGE\"", "lang"])
            .output_string()
            .unwrap();
        assert_eq!(language, "en");
    }

    #[test]
    fn a_canceled_command_stops() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = Cancel::new();
        cancel.cancel();
        // `git hash-object --stdin` waits for input that never ends: only cancellation stops it.
        let mut command = GitCommand::new(dir.path()).args(["hash-object", "--stdin"]);
        command.stdin = None;
        let result = command.run(Some(&cancel), |_| {});
        assert!(matches!(result, Err(GitError::Canceled)), "{result:?}");
    }
}
