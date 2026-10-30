//! The interfaces whose work the window does: terminals, proposals, Git, the problems, the browser
//! and the clipboard (and `editors.close`, in `host`). Each call becomes a [`WindowCall`]; the
//! plugin's thread waits for the window's [`WindowReply`] to the calls that return something (the
//! wait doesn't count toward its time limit) and doesn't wait for the others. Permissions are
//! checked here, before the window hears of the call.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::api::bindings::flux::plugin::{
    claude, contexts, diagnostics, git, links, picker, review, system, terminal,
};
use crate::api::events::Event;
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

    /// `project = "write"`: changing the project — its Git repositories.
    fn require_project_write(&self) -> Result<(), String> {
        if self.entry.manifest.permissions.project != ProjectAccess::Write {
            return Err(missing("change the project", "project = \"write\""));
        }
        Ok(())
    }

    fn require_claude(&self) -> Result<(), String> {
        if !self.entry.manifest.permissions.claude {
            return Err(missing("open Claude Code sessions", "claude = true"));
        }
        Ok(())
    }

    /// A picker's anchor the manifest declares — a status bar item, a tool window; another is
    /// warned about and becomes the window's.
    fn checked_anchor(&self, anchor: picker::PickerAnchor) -> picker::PickerAnchor {
        let manifest = &self.entry.manifest;
        let known = match &anchor {
            picker::PickerAnchor::Window => return anchor,
            picker::PickerAnchor::StatusItem(item) => {
                manifest.status_items.iter().any(|known| known.id == *item)
            }
            picker::PickerAnchor::Element((window, _)) => manifest.tool_window(window).is_some(),
        };
        if known {
            return anchor;
        }
        let what = match &anchor {
            picker::PickerAnchor::StatusItem(item) => format!("the status bar item \"{item}\""),
            picker::PickerAnchor::Element((window, _)) => format!("the tool window \"{window}\""),
            picker::PickerAnchor::Window => unreachable!(),
        };
        self.warn(&format!(
            "picker.open: {what} isn't in flux-plugin.toml; the picker opens under the title bar"
        ));
        picker::PickerAnchor::Window
    }

    /// A Git operation the window does in the background (`new-branch`, `checkout`): its id now,
    /// the end as `git-done` — at once when the permissions don't allow it.
    fn git_operation(&mut self, call: impl FnOnce(u64) -> WindowCall) -> u64 {
        let id = self.next_id();
        match self.require_project_write() {
            Ok(()) => self.tell(call(id)),
            Err(err) => {
                self.warn(&err);
                self.events.send(Event::GitDone((id, Err(err))));
            }
        }
        id
    }
}

/// The error of a call the permissions don't allow: what the plugin wanted, and what the manifest
/// lacks.
pub(crate) fn missing(what: &str, permission: &str) -> String {
    format!("The plugin may not {what}: no `{permission}` in the permissions of flux-plugin.toml")
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

    fn branches(&mut self, root: String) -> Result<Vec<git::Branch>, String> {
        self.host.require_project()?;
        match self.host.window(WindowCall::GitBranches(root)) {
            Some(WindowReply::Branches(result)) => result,
            _ => Err(no_answer()),
        }
    }

    fn new_branch(&mut self, root: String, name: String, start: Option<String>) -> u64 {
        self.host.git_operation(|id| WindowCall::GitNewBranch {
            id,
            root,
            name,
            start,
        })
    }

    fn checkout(&mut self, root: String, branch: String) -> u64 {
        self.host
            .git_operation(|id| WindowCall::GitCheckout { id, root, branch })
    }

    fn set_commit_template(&mut self, template: Option<git::CommitTemplate>) {
        if let Err(err) = self.host.require_project() {
            self.host.warn(&err);
            return;
        }
        self.host.tell(WindowCall::SetCommitTemplate(template));
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
        let scheme = url
            .split_once(':')
            .map(|(scheme, _)| scheme.to_ascii_lowercase());
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
        flux_platform::paths::home()
            .map(|home| home.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn utc_offset(&mut self, at: i64) -> i32 {
        local_utc_offset(at)
    }

    fn time_zone(&mut self) -> String {
        local_time_zone()
    }

    fn open_settings(&mut self) {
        self.host.tell(WindowCall::OpenSettings);
    }
}

/// How far local time is ahead of UTC at `at` (seconds since 1970), by the system's rules.
pub(crate) fn local_utc_offset(at: i64) -> i32 {
    flux_platform::time::local_utc_offset(at)
}

/// The IANA name of the local time zone: `TZ`, else the target of `/etc/localtime`
/// (`…/zoneinfo/Europe/Moscow`); "UTC" when neither says.
pub(crate) fn local_time_zone() -> String {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim_start_matches(':');
        if !tz.is_empty() && !tz.starts_with('/') {
            return tz.to_string();
        }
    }
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|target| {
            let target = target.to_string_lossy().into_owned();
            target
                .split_once("zoneinfo/")
                .map(|(_, name)| name.to_string())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "UTC".to_string())
}

impl picker::Host for State {
    fn open(&mut self, mut spec: picker::PickerSpec) -> u64 {
        let id = self.host.next_id();
        spec.anchor = self.host.checked_anchor(spec.anchor);
        self.host.call(HostCall::OpenPicker { id, spec });
        id
    }

    fn update(&mut self, picker: u64, items: Vec<picker::PickerItem>, loading: bool) {
        self.host.call(HostCall::UpdatePicker {
            id: picker,
            items,
            loading,
        });
    }

    fn close(&mut self, picker: u64) {
        self.host.call(HostCall::ClosePicker { id: picker });
    }
}

impl links::Host for State {
    fn set_patterns(&mut self, patterns: Vec<links::LinkPattern>) -> Result<(), String> {
        self.host.window_done(WindowCall::SetLinkPatterns(patterns))
    }

    fn describe(&mut self, request: u64, markdown: Option<String>) {
        self.host
            .tell(WindowCall::DescribeLink { request, markdown });
    }
}

impl claude::Host for State {
    fn new_session(&mut self, spec: claude::SessionSpec) -> Result<(), String> {
        self.host.require_claude()?;
        self.host.window_done(WindowCall::NewClaudeSession(spec))
    }
}

impl contexts::Host for State {
    fn switch(
        &mut self,
        save: Option<contexts::ContextRef>,
        load: Option<String>,
    ) -> Result<(), String> {
        self.host.require_project()?;
        self.host
            .window_done(WindowCall::SwitchContext { save, load })
    }

    fn remove(&mut self, key: String) {
        if self.host.require_project().is_ok() {
            self.host.tell(WindowCall::RemoveContext(key));
        }
    }
}
