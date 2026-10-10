//! Programs on this Mac, as the manifest's `processes` permission allows (by name, or any). They
//! run with the user's rights and the `PATH` of the user's login shell, in the project root unless
//! told otherwise, and end when the plugin stops:
//!
//! ```ignore
//! let version = process::run("git", &["--version"])?.stdout_text();
//!
//! // In the background: Event::ProcessOutput(output)…, then Event::ProcessExited((id, code)).
//! self.build = Some(process::command("cargo").args(["build", "--message-format=json"]).spawn()?);
//! ```
//!
//! Waiting in `run` doesn't count toward the plugin's time limit; a long program goes with
//! `spawn`, and the plugin answers other calls meanwhile.

use std::time::Duration;

use crate::host::process as raw;
pub use crate::host::process::{Output, OutputChannel, ProcessOutput};

/// A program to run: [`command`], then the methods add to it.
#[derive(Debug, Clone)]
pub struct Command {
    command: raw::Command,
    stdin: Option<Vec<u8>>,
    timeout_ms: Option<u32>,
}

/// A program by name (looked up in `PATH`) or by absolute path.
pub fn command(program: &str) -> Command {
    Command {
        command: raw::Command {
            program: program.to_string(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
        },
        stdin: None,
        timeout_ms: None,
    }
}

/// Runs `program` with `args` to its end and returns its output: `run("git", &["status"])`.
pub fn run(program: &str, args: &[&str]) -> Result<Output, String> {
    command(program).args(args).run()
}

impl Command {
    pub fn arg(mut self, arg: &str) -> Self {
        self.command.args.push(arg.to_string());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.command
            .args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_string()));
        self
    }

    /// The working folder (the project root by default).
    pub fn cwd(mut self, dir: &str) -> Self {
        self.command.cwd = Some(dir.to_string());
        self
    }

    /// A variable added to the environment.
    pub fn env(mut self, name: &str, value: &str) -> Self {
        self.command.env.push((name.to_string(), value.to_string()));
        self
    }

    /// The program's input, for [`Command::run`].
    pub fn stdin(mut self, input: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(input.into());
        self
    }

    /// How long [`Command::run`] waits before killing the program (60 s by default).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout_ms = Some(timeout.as_millis().min(u32::MAX as u128) as u32);
        self
    }

    /// Runs the program to its end and returns its output. An exit code other than 0 is still
    /// `Ok` — see [`Output::success`]; `Err` is a program that didn't start or didn't end in time.
    pub fn run(&self) -> Result<Output, String> {
        raw::run(&self.command, self.stdin.as_deref(), self.timeout_ms)
    }

    /// Starts the program and returns its id: its output comes as `Event::ProcessOutput`, its end
    /// as `Event::ProcessExited`. Its input: [`write`], [`close_stdin`].
    pub fn spawn(&self) -> Result<u64, String> {
        raw::spawn(&self.command)
    }

    /// The command as the API has it.
    pub fn as_raw(&self) -> &raw::Command {
        &self.command
    }
}

/// Writes to a started program's input.
pub fn write(process: u64, bytes: &[u8]) -> Result<(), String> {
    raw::write(process, bytes)
}

/// Closes a started program's input: the end of its input.
pub fn close_stdin(process: u64) {
    raw::close_stdin(process)
}

/// Ends a started program: SIGTERM, then SIGKILL if it is still running 2 s later.
pub fn kill(process: u64) {
    raw::kill(process)
}

impl Output {
    /// Exited with code 0.
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// The output as text (invalid UTF-8 replaced).
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// The errors as text (invalid UTF-8 replaced).
    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_commands() {
        let command = command("git")
            .args(["log", "--oneline"])
            .arg("-5")
            .cwd("/tmp")
            .env("GIT_PAGER", "cat")
            .stdin("input")
            .timeout(Duration::from_millis(1500));
        assert_eq!(command.as_raw().args, ["log", "--oneline", "-5"]);
        assert_eq!(command.as_raw().cwd.as_deref(), Some("/tmp"));
        assert_eq!(command.as_raw().env, [("GIT_PAGER".into(), "cat".into())]);
        assert_eq!(command.stdin.as_deref(), Some(&b"input"[..]));
        assert_eq!(command.timeout_ms, Some(1500));
        let output = Output {
            exit_code: Some(0),
            stdout: b"git version 2.50\n".to_vec(),
            stderr: Vec::new(),
        };
        assert!(output.success());
        assert_eq!(output.stdout_text(), "git version 2.50\n");
    }
}
