//! The interfaces whose work the window does: terminals, proposals, Git, the problems, the browser
//! and the clipboard (and `editors.close`, in `host`). Each call becomes a [`WindowCall`]; the
//! plugin's thread waits for the window's [`WindowReply`] to the calls that return something (the
//! wait doesn't count toward its time limit) and doesn't wait for the others. Permissions are
//! checked here, before the window hears of the call.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::api::bindings::flux::plugin::{diagnostics, git, review, system, terminal};
use crate::host::{HostState, no_answer};
use crate::manifest::ProjectAccess;
use crate::runtime::{HostCall, State, WindowCall, WindowReply};

/// How long the plugin waits for the window's reply.
const WINDOW_TIMEOUT: Duration = Duration::from_secs(10);

impl HostState {
    /// Asks the window and waits for its reply; none if the window is gone or doesn't answer in
    /// time.
    pub(crate) fn window(&mut self, call: WindowCall) -> Option<WindowReply> {
        let (reply, answer) = mpsc::channel();
        self.call(HostCall::Window { call, reply });
        let started = Instant::now();
        let answer = answer.recv_timeout(WINDOW_TIMEOUT).ok();
        self.waited += started.elapsed();
        answer
    }

    /// Tells the window without waiting for its reply.
    pub(crate) fn tell(&self, call: WindowCall) {
        let (reply, _) = mpsc::channel();
        self.call(HostCall::Window { call, reply });
    }

    fn window_done(&mut self, call: WindowCall) -> Result<(), String> {
        match self.window(call) {
            Some(WindowReply::Done(result)) => result,
            _ => Err(no_answer()),
        }
    }

    /// The `project` permission (read or write): the project's content.
    pub(crate) fn require_project(&self) -> Result<(), String> {
        if self.entry.manifest.permissions.project == ProjectAccess::None {
            return Err(missing("read the project", "project = \"read\""));
        }
        Ok(())
    }

    fn require_terminal(&self) -> Result<(), String> {
        if !self.entry.manifest.permissions.terminal {
            return Err(missing("use terminals", "terminal = true"));
        }
        Ok(())
    }
}

/// The error of a call the permissions don't allow: what the plugin wanted, and what the manifest
/// lacks.
pub(crate) fn missing(what: &str, permission: &str) -> String {
    format!(
        "The plugin may not {what}: no `{permission}` in the permissions of flux-plugin.toml"
    )
}

impl terminal::Host for State {
    fn open(&mut self, options: terminal::TerminalOptions) -> Result<u64, String> {
        self.host.require_terminal()?;
        match self.host.window(WindowCall::OpenTerminal(options)) {
            Some(WindowReply::Opened(result)) => result,
            _ => Err(no_answer()),
        }
    }

    fn send_text(&mut self, terminal: u64, text: String) -> Result<(), String> {
        self.host.require_terminal()?;
        self.host
            .window_done(WindowCall::SendText { terminal, text })
    }

    fn show(&mut self, terminal: u64, focus: bool) -> Result<(), String> {
        self.host.require_terminal()?;
        self.host
            .window_done(WindowCall::ShowTerminal { terminal, focus })
    }

    fn close(&mut self, terminal: u64) {
        if self.host.require_terminal().is_ok() {
            self.host.tell(WindowCall::CloseTerminal(terminal));
        }
    }
}

impl review::Host for State {
    fn propose(&mut self, proposal: review::Proposal) -> Result<u64, String> {
        self.host.require_project()?;
        let id = self.host.next_id();
        self.host
            .window_done(WindowCall::Propose { id, proposal })
            .map(|()| id)
    }

    fn withdraw(&mut self, proposal: u64) {
        self.host.tell(WindowCall::Withdraw(proposal));
    }
}

impl git::Host for State {
    fn repositories(&mut self) -> Vec<git::Repository> {
        if let Err(err) = self.host.require_project() {
            self.host.warn(&err);
            return Vec::new();
        }
        match self.host.window(WindowCall::Repositories) {
            Some(WindowReply::Repositories(repositories)) => repositories,
            _ => Vec::new(),
        }
    }

    fn status(&mut self) -> Vec<git::Change> {
        if let Err(err) = self.host.require_project() {
            self.host.warn(&err);
            return Vec::new();
        }
        match self.host.window(WindowCall::GitStatus) {
            Some(WindowReply::Changes(changes)) => changes,
            _ => Vec::new(),
        }
    }

    fn diff(&mut self, path: String) -> Result<String, String> {
        self.host.require_project()?;
        match self.host.window(WindowCall::GitDiff(path)) {
            Some(WindowReply::Text(result)) => result,
            _ => Err(no_answer()),
        }
    }
}

impl diagnostics::Host for State {
    fn get(&mut self, path: Option<String>) -> Vec<diagnostics::FileDiagnostics> {
        if let Err(err) = self.host.require_project() {
            self.host.warn(&err);
            return Vec::new();
        }
        match self.host.window(WindowCall::Diagnostics(path)) {
            Some(WindowReply::Diagnostics(files)) => files,
            _ => Vec::new(),
        }
    }

    fn publish(&mut self, path: String, diagnostics: Vec<diagnostics::Diagnostic>) {
        self.host
            .tell(WindowCall::PublishDiagnostics { path, diagnostics });
    }

    fn clear(&mut self) {
        self.host.tell(WindowCall::ClearDiagnostics);
    }
}

impl system::Host for State {
    fn open_url(&mut self, url: String) -> Result<(), String> {
        let scheme = url.split_once(':').map(|(scheme, _)| scheme.to_ascii_lowercase());
        match scheme.as_deref() {
            Some("http" | "https" | "mailto") => {
                self.host.tell(WindowCall::OpenUrl(url));
                Ok(())
            }
            _ => Err(format!(
                "Only http, https and mailto links open in the browser: {url}"
            )),
        }
    }

    fn copy_text(&mut self, text: String) {
        self.host.tell(WindowCall::CopyText(text));
    }

    fn home_dir(&mut self) -> String {
        std::env::var("HOME").unwrap_or_default()
    }
}
