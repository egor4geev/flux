//! `service` (API 0.5): messages between a plugin's service and its windows' instances
//! ([`crate::services`]); and what the service's calls of window-only interfaces become — it has
//! no window: documents, the project, tool windows, questions, pickers, terminals, proposals,
//! Git, problems, links, Claude, contexts, commands and the status bar fail or do nothing there,
//! with a line in the plugin's log. Its notifications, the `status` lines of its settings page,
//! its settings, and opening a link or copying text go on to the app ([`HostState::call`]).

use crate::api::bindings::flux::plugin::service;
use crate::api::events::Event;
use crate::runtime::{
    EditorCall, EditorReply, HostCall, InstanceRole, State, WindowCall, WindowReply,
};

use super::HostState;

/// Why a window-only call fails in the service.
pub(crate) const NO_WINDOW: &str = "The plugin's service has no window";

impl service::Host for State {
    fn is_service(&mut self) -> bool {
        self.host.role == InstanceRole::Service
    }

    fn send(&mut self, window: Option<u64>, message: String) -> Result<(), String> {
        let host = &self.host;
        if !host.entry.manifest.service {
            return Err("service.send: flux-plugin.toml doesn't say service = true".into());
        }
        match host.role {
            InstanceRole::Window(own) => crate::services::to_service(&host.id, own, message),
            InstanceRole::Service => crate::services::to_windows(&host.id, window, message),
        }
    }

    fn windows(&mut self) -> Vec<u64> {
        match self.host.role {
            InstanceRole::Service => crate::services::windows(&self.host.id),
            InstanceRole::Window(_) => Vec::new(),
        }
    }
}

impl HostState {
    /// A call of the service (it has no window): the ones the app carries out for the whole app
    /// go on; a question is answered as dismissed at once, a call that waits for the window gets
    /// an empty answer or [`NO_WINDOW`]; the rest is dropped. Each but the first kind leaves a
    /// line in the log.
    pub(super) fn service_call(&self, call: HostCall) -> Option<HostCall> {
        let what = match &call {
            HostCall::Notify { .. }
            | HostCall::UpdateNotification { .. }
            | HostCall::SetProgress { .. }
            | HostCall::ExpireNotification { .. }
            | HostCall::RemoveNotification { .. }
            | HostCall::SetSetting { .. }
            | HostCall::SetSettingStatus { .. }
            | HostCall::HelperProgress { .. }
            | HostCall::Window {
                call: WindowCall::OpenUrl(_) | WindowCall::CopyText(_),
                ..
            } => return Some(call),
            HostCall::Ask { id, .. } => {
                self.events.send(Event::DialogAnswered((*id, None)));
                "dialogs.ask"
            }
            HostCall::AskText { id, .. } => {
                self.events.send(Event::TextAnswered((*id, None)));
                "dialogs.ask-text"
            }
            HostCall::AskForm { id, .. } => {
                self.events.send(Event::FormAnswered((*id, None)));
                "dialogs.ask-form"
            }
            HostCall::PickFiles { id, .. } => {
                self.events.send(Event::FilesPicked((*id, None)));
                "dialogs.pick-files"
            }
            HostCall::OpenPicker { id, .. } => {
                self.events.send(Event::PickerChosen((*id, None)));
                "picker.open"
            }
            HostCall::UpdatePicker { .. } | HostCall::ClosePicker { .. } => "picker",
            HostCall::SetCommandEnabled { .. } => "commands.set-enabled",
            HostCall::SetStatusItem { .. } => "status-bar.set",
            HostCall::SetView { .. }
            | HostCall::ShowToolWindow { .. }
            | HostCall::HideToolWindow { .. }
            | HostCall::SetToolWindowTitle { .. }
            | HostCall::UpdateConversation { .. }
            | HostCall::OpenTab { .. }
            | HostCall::SetBadge { .. } => "ui",
            HostCall::ToolRespond { .. } | HostCall::ToolConfirm { .. } => "tools",
            HostCall::Editor { call, reply } => {
                let _ = reply.send(editor_reply(call));
                "editors"
            }
            HostCall::Window { call, reply } => {
                match window_reply(call) {
                    Some(answer) => {
                        let _ = reply.send(answer);
                    }
                    None => {
                        if let WindowCall::GitNewBranch { id, .. }
                        | WindowCall::GitCheckout { id, .. } = call
                        {
                            self.events
                                .send(Event::GitDone((*id, Err(NO_WINDOW.to_string()))));
                        }
                    }
                }
                window_call_name(call)
            }
        };
        self.warn(&format!(
            "{what}: not in the plugin's service — it has no window"
        ));
        None
    }
}

/// The service's answer to a call about documents: there are none.
fn editor_reply(call: &EditorCall) -> EditorReply {
    match call {
        EditorCall::Active => EditorReply::Editor(None),
        EditorCall::List => EditorReply::Editors(Vec::new()),
        EditorCall::Text(_) => EditorReply::Text(None),
        EditorCall::Selections(_) => EditorReply::Selections(Vec::new()),
        EditorCall::SetSelections(..) | EditorCall::Edit(..) | EditorCall::Save(_) => {
            EditorReply::Done(Err(NO_WINDOW.into()))
        }
        EditorCall::Open(..) => EditorReply::Opened(Err(NO_WINDOW.into())),
    }
}

/// The service's answer to a call of the window's interfaces; none for one without an answer.
fn window_reply(call: &WindowCall) -> Option<WindowReply> {
    fn failed<T>() -> Result<T, String> {
        Err(NO_WINDOW.to_string())
    }
    Some(match call {
        WindowCall::CloseEditor(_)
        | WindowCall::SendText { .. }
        | WindowCall::ShowTerminal { .. }
        | WindowCall::Propose { .. }
        | WindowCall::SetLinkPatterns(_)
        | WindowCall::NewClaudeSession(_)
        | WindowCall::SwitchContext { .. } => WindowReply::Done(failed()),
        WindowCall::OpenTerminal(_) => WindowReply::Opened(failed()),
        WindowCall::Repositories => WindowReply::Repositories(Vec::new()),
        WindowCall::GitStatus => WindowReply::Changes(Vec::new()),
        WindowCall::GitDiff(_) => WindowReply::Text(failed()),
        WindowCall::Diagnostics(_) => WindowReply::Diagnostics(Vec::new()),
        WindowCall::GitBranches(_) => WindowReply::Branches(failed()),
        WindowCall::CloseTerminal(_)
        | WindowCall::Withdraw(_)
        | WindowCall::PublishDiagnostics { .. }
        | WindowCall::ClearDiagnostics
        | WindowCall::OpenUrl(_)
        | WindowCall::CopyText(_)
        | WindowCall::OpenSettings
        | WindowCall::GitNewBranch { .. }
        | WindowCall::GitCheckout { .. }
        | WindowCall::SetCommitTemplate(_)
        | WindowCall::DescribeLink { .. }
        | WindowCall::RemoveContext(_) => return None,
    })
}

/// The interface of a window's call, for the log.
fn window_call_name(call: &WindowCall) -> &'static str {
    match call {
        WindowCall::CloseEditor(_) => "editors.close",
        WindowCall::OpenTerminal(_)
        | WindowCall::SendText { .. }
        | WindowCall::ShowTerminal { .. }
        | WindowCall::CloseTerminal(_) => "terminal",
        WindowCall::Propose { .. } | WindowCall::Withdraw(_) => "review",
        WindowCall::Repositories
        | WindowCall::GitStatus
        | WindowCall::GitDiff(_)
        | WindowCall::GitBranches(_)
        | WindowCall::GitNewBranch { .. }
        | WindowCall::GitCheckout { .. }
        | WindowCall::SetCommitTemplate(_) => "git",
        WindowCall::Diagnostics(_)
        | WindowCall::PublishDiagnostics { .. }
        | WindowCall::ClearDiagnostics => "diagnostics",
        WindowCall::OpenUrl(_) | WindowCall::CopyText(_) | WindowCall::OpenSettings => "system",
        WindowCall::SetLinkPatterns(_) | WindowCall::DescribeLink { .. } => "links",
        WindowCall::NewClaudeSession(_) => "claude",
        WindowCall::SwitchContext { .. } | WindowCall::RemoveContext(_) => "contexts",
    }
}
