//! `claude-remote` (API 0.6): every Claude session of Flux for a plugin with the `claude-remote`
//! permission. Each call becomes a [`HostCall::Claude`] with a [`RemoteCall`] — from a window's
//! instance and from the service alike — and the plugin's thread waits for the app's
//! [`RemoteReply`] (the wait doesn't count toward its time limit): a new session may open a window
//! first, so it gets longer. `watch` also puts the instance among the watchers
//! ([`crate::claude_remote::broadcast`]); [`event`] turns the app's events into the WIT's.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::api::claude_remote as wit;
use crate::api::events::Event;
use crate::claude_remote::{
    AwakeMode, Limits, NewSession, PermissionRequest, RegionState, RemoteAnswer, RemoteCall,
    RemoteEvent, RemoteMode, RemoteProject, RemoteQuestion, RemoteReply, RemoteRequest,
    RemoteSession, RemoteSteps, RequestKind, RollbackCheck, SessionState, SystemEvent, SystemState,
    TranscriptItem, TurnSummary,
};
use crate::host::{HostState, no_answer};
use crate::runtime::{HostCall, State};

use super::window::missing;

/// How long the plugin waits for the app's reply.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(10);
/// …to `new-session`: the project's window may have to open first.
const NEW_SESSION_TIMEOUT: Duration = Duration::from_secs(30);

impl HostState {
    fn require_remote(&self) -> Result<(), String> {
        if !self.entry.manifest.permissions.claude_remote {
            return Err(missing(
                "work with Claude's sessions",
                "claude-remote = true",
            ));
        }
        Ok(())
    }

    /// Asks the app and waits for its reply; a failure is the app's reason.
    fn remote(&mut self, call: RemoteCall) -> Result<RemoteReply, String> {
        self.require_remote()?;
        let timeout = match call {
            RemoteCall::NewSession(_) => NEW_SESSION_TIMEOUT,
            _ => REMOTE_TIMEOUT,
        };
        let (reply, answer) = mpsc::channel();
        self.call(HostCall::Claude { call, reply });
        let started = Instant::now();
        let answer = answer.recv_timeout(timeout);
        self.waited += started.elapsed();
        match answer {
            Ok(RemoteReply::Failed(error)) => Err(error),
            Ok(reply) => Ok(reply),
            Err(_) => Err(no_answer()),
        }
    }

    fn remote_done(&mut self, call: RemoteCall) -> Result<(), String> {
        match self.remote(call)? {
            RemoteReply::Done => Ok(()),
            _ => Err(no_answer()),
        }
    }
}

impl wit::Host for State {
    fn watch(&mut self, on: bool) -> Result<(), String> {
        let host = &mut self.host;
        host.require_remote()?;
        if on {
            // Among the watchers before the app hears of it: what it posts at once arrives.
            if host.watching.is_none() {
                host.watching = Some(crate::claude_remote::watch(&host.id, &host.events));
            }
        } else {
            host.watching = None;
        }
        let result = host.remote_done(RemoteCall::Watch(on));
        if result.is_err() && on {
            host.watching = None;
        }
        result
    }

    fn sessions(&mut self) -> Result<Vec<wit::Session>, String> {
        match self.host.remote(RemoteCall::Sessions)? {
            RemoteReply::Sessions(sessions) => Ok(sessions.into_iter().map(session).collect()),
            _ => Err(no_answer()),
        }
    }

    fn transcript(
        &mut self,
        session: String,
        count: u32,
    ) -> Result<Vec<wit::TranscriptItem>, String> {
        match self
            .host
            .remote(RemoteCall::Transcript { session, count })?
        {
            RemoteReply::Transcript(items) => Ok(items.into_iter().map(transcript_item).collect()),
            _ => Err(no_answer()),
        }
    }

    fn send(&mut self, session: String, text: String, images: Vec<String>) -> Result<(), String> {
        self.host.remote_done(RemoteCall::Send {
            session,
            text,
            images,
        })
    }

    fn answer(
        &mut self,
        session: String,
        request: String,
        response: wit::Response,
    ) -> Result<(), String> {
        self.host.remote_done(RemoteCall::Answer {
            session,
            request,
            answer: answer(response),
        })
    }

    fn interrupt(&mut self, session: String) -> Result<(), String> {
        self.host.remote_done(RemoteCall::Interrupt { session })
    }

    fn set_mode(&mut self, session: String, mode: wit::Mode) -> Result<(), String> {
        if mode == wit::Mode::Other {
            return Err("claude-remote.set-mode: `other` can't be set".into());
        }
        self.host.remote_done(RemoteCall::SetMode {
            session,
            mode: from_mode(mode),
        })
    }

    fn new_session(&mut self, spec: wit::NewSessionSpec) -> Result<String, String> {
        if spec.mode == wit::Mode::Other {
            return Err("claude-remote.new-session: the mode `other` can't be set".into());
        }
        let call = RemoteCall::NewSession(NewSession {
            project: spec.project,
            title: spec.title,
            prompt: spec.prompt,
            attachments: spec.attachments,
            images: spec.images,
            mode: from_mode(spec.mode),
        });
        match self.host.remote(call)? {
            RemoteReply::Session(id) => Ok(id),
            _ => Err(no_answer()),
        }
    }

    fn turn_diff(&mut self, session: String, turn: Option<u32>) -> Result<String, String> {
        match self.host.remote(RemoteCall::TurnDiff { session, turn })? {
            RemoteReply::Text(diff) => Ok(diff),
            _ => Err(no_answer()),
        }
    }

    fn check_rollback(
        &mut self,
        session: String,
        turn: Option<u32>,
    ) -> Result<wit::RollbackCheck, String> {
        match self
            .host
            .remote(RemoteCall::CheckRollback { session, turn })?
        {
            RemoteReply::Rollback(check) => Ok(rollback_check(check)),
            _ => Err(no_answer()),
        }
    }

    fn rollback(&mut self, session: String, turn: Option<u32>) -> Result<(), String> {
        self.host
            .remote_done(RemoteCall::Rollback { session, turn })
    }

    fn limits(&mut self) -> Result<wit::UsageLimits, String> {
        match self.host.remote(RemoteCall::Limits)? {
            RemoteReply::Limits(value) => Ok(limits(value)),
            _ => Err(no_answer()),
        }
    }

    fn region(&mut self, check_again: bool) -> Result<wit::RegionState, String> {
        match self
            .host
            .remote(RemoteCall::Region { again: check_again })?
        {
            RemoteReply::Region(state) => Ok(region(state)),
            _ => Err(no_answer()),
        }
    }

    fn projects(&mut self) -> Result<Vec<wit::Project>, String> {
        match self.host.remote(RemoteCall::Projects)? {
            RemoteReply::Projects(projects) => Ok(projects.into_iter().map(project).collect()),
            _ => Err(no_answer()),
        }
    }

    fn set_awake(&mut self, mode: wit::Awake) -> Result<(), String> {
        let mode = match mode {
            wit::Awake::Off => AwakeMode::Off,
            wit::Awake::Always => AwakeMode::Always,
            wit::Awake::OnPower => AwakeMode::OnPower,
        };
        self.host.remote_done(RemoteCall::SetAwake(mode))
    }

    fn system_status(&mut self) -> Result<wit::SystemState, String> {
        match self.host.remote(RemoteCall::System)? {
            RemoteReply::System(SystemState {
                screen_locked,
                on_power,
            }) => Ok(wit::SystemState {
                screen_locked,
                on_power,
            }),
            _ => Err(no_answer()),
        }
    }
}

// --- The app's values out, the plugin's in ---

/// An event of the app for the plugin's queue; none for one that goes another way (a pult's
/// action through [`crate::claude_remote::send_to`] — `broadcast` drops it).
pub(crate) fn event(event: RemoteEvent) -> Option<Event> {
    Some(match event {
        RemoteEvent::Session(value) => Event::ClaudeSession(session(value)),
        RemoteEvent::SessionGone(id) => Event::ClaudeSessionGone(id),
        RemoteEvent::Text {
            session,
            message,
            text,
            done,
        } => Event::ClaudeText(wit::TextUpdate {
            session,
            message,
            text,
            done,
        }),
        RemoteEvent::Steps { session, steps } => Event::ClaudeSteps((session, self::steps(steps))),
        RemoteEvent::Request { session, request } => {
            Event::ClaudeRequest((session, self::request(request)))
        }
        RemoteEvent::Resolved { session, request } => Event::ClaudeResolved((session, request)),
        RemoteEvent::TurnEnded { session, summary } => {
            Event::ClaudeTurnEnded((session, turn_summary(summary)))
        }
        RemoteEvent::Failed { session, error } => Event::ClaudeFailed((session, error)),
        RemoteEvent::Limits(value) => Event::ClaudeLimits(limits(value)),
        RemoteEvent::Region(state) => Event::ClaudeRegion(region(state)),
        RemoteEvent::System(value) => Event::SystemChanged(match value {
            SystemEvent::ScreenLocked => wit::SystemEvent::ScreenLocked,
            SystemEvent::ScreenUnlocked => wit::SystemEvent::ScreenUnlocked,
            SystemEvent::WillSleep => wit::SystemEvent::WillSleep,
            SystemEvent::DidWake => wit::SystemEvent::DidWake,
            SystemEvent::Power(true) => wit::SystemEvent::OnPower,
            SystemEvent::Power(false) => wit::SystemEvent::OnBattery,
        }),
        RemoteEvent::Action { session, action } => Event::ClaudeAction((session, action)),
    })
}

/// [`event`] for [`crate::claude_remote::broadcast`]: everything but a pult's action.
pub(crate) fn broadcast_event(remote: RemoteEvent) -> Option<Event> {
    match remote {
        RemoteEvent::Action { .. } => None,
        other => event(other),
    }
}

fn session(session: RemoteSession) -> wit::Session {
    wit::Session {
        id: session.id,
        project: session.project,
        title: session.title,
        state: match session.state {
            SessionState::Starting => wit::SessionState::Starting,
            SessionState::Working => wit::SessionState::Working,
            SessionState::Waiting => wit::SessionState::Waiting,
            SessionState::Idle => wit::SessionState::Idle,
            SessionState::Unloaded => wit::SessionState::Unloaded,
        },
        activity: session.activity,
        mode: mode(session.mode),
        window: session.window,
        updated: session.updated,
        request: session.request.map(request),
        unseen: session.unseen,
    }
}

fn mode(mode: RemoteMode) -> wit::Mode {
    match mode {
        RemoteMode::Ask => wit::Mode::Ask,
        RemoteMode::AcceptEdits => wit::Mode::AcceptEdits,
        RemoteMode::Plan => wit::Mode::Plan,
        RemoteMode::Bypass => wit::Mode::Bypass,
        RemoteMode::Other => wit::Mode::Other,
    }
}

fn from_mode(mode: wit::Mode) -> RemoteMode {
    match mode {
        wit::Mode::Ask => RemoteMode::Ask,
        wit::Mode::AcceptEdits => RemoteMode::AcceptEdits,
        wit::Mode::Plan => RemoteMode::Plan,
        wit::Mode::Bypass => RemoteMode::Bypass,
        wit::Mode::Other => RemoteMode::Other,
    }
}

fn request(request: RemoteRequest) -> wit::Request {
    wit::Request {
        id: request.id,
        kind: match request.kind {
            RequestKind::Permission(PermissionRequest {
                title,
                tool,
                subject,
                diff,
                choices,
            }) => wit::RequestKind::Permission(wit::PermissionRequest {
                title,
                tool,
                subject,
                diff,
                choices,
            }),
            RequestKind::Questions(questions) => {
                wit::RequestKind::Questions(questions.into_iter().map(question).collect())
            }
            RequestKind::Plan(plan) => wit::RequestKind::Plan(plan),
        },
    }
}

fn question(question: RemoteQuestion) -> wit::Question {
    wit::Question {
        question: question.question,
        header: question.header,
        options: question
            .options
            .into_iter()
            .map(|(label, description)| wit::QuestionOption { label, description })
            .collect(),
        multi_select: question.multi_select,
    }
}

fn answer(response: wit::Response) -> RemoteAnswer {
    match response {
        wit::Response::Allow(choice) => RemoteAnswer::Allow { choice },
        wit::Response::Deny(message) => RemoteAnswer::Deny { message },
        wit::Response::Answers(answers) => RemoteAnswer::Answers(answers),
        wit::Response::ApprovePlan(accept_edits) => RemoteAnswer::ApprovePlan { accept_edits },
        wit::Response::KeepPlanning(feedback) => RemoteAnswer::KeepPlanning(feedback),
    }
}

fn turn_summary(summary: TurnSummary) -> wit::TurnSummary {
    wit::TurnSummary {
        turn: summary.turn,
        files: summary
            .files
            .into_iter()
            .map(|file| wit::TurnFile {
                path: file.path,
                added: file.added,
                removed: file.removed,
                created: file.created,
            })
            .collect(),
        failed: summary.failed,
        text: summary.text,
        seconds: summary.seconds,
    }
}

fn steps(steps: RemoteSteps) -> wit::Steps {
    wit::Steps {
        group: steps.group,
        summary: steps.summary,
        lines: steps.lines,
        done: steps.done,
    }
}

fn transcript_item(item: TranscriptItem) -> wit::TranscriptItem {
    wit::TranscriptItem {
        user: item.user,
        text: item.text,
    }
}

fn rollback_check(check: RollbackCheck) -> wit::RollbackCheck {
    wit::RollbackCheck {
        files: check.files,
        changed_since: check.changed_since,
        created: check.created,
    }
}

fn limits(limits: Limits) -> wit::UsageLimits {
    let window = |window: Option<(u8, i64)>| {
        window.map(|(percent, resets_at)| wit::UsageWindow { percent, resets_at })
    };
    wit::UsageLimits {
        five_hour: window(limits.five_hour),
        seven_day: window(limits.seven_day),
        rejected: limits.rejected,
    }
}

fn region(state: RegionState) -> wit::RegionState {
    match state {
        RegionState::Allowed => wit::RegionState::Allowed,
        RegionState::Checking => wit::RegionState::Checking,
        RegionState::Blocked { country, message } => wit::RegionState::Blocked((country, message)),
    }
}

fn project(project: RemoteProject) -> wit::Project {
    wit::Project {
        path: project.path,
        name: project.name,
        open: project.open,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_remote::TurnFile;

    #[test]
    fn events_become_the_wits() {
        let ended = event(RemoteEvent::TurnEnded {
            session: "s1".into(),
            summary: TurnSummary {
                turn: 2,
                files: vec![TurnFile {
                    path: "src/main.rs".into(),
                    added: 3,
                    removed: 1,
                    created: false,
                }],
                failed: false,
                text: "Done".into(),
                seconds: 12,
            },
        });
        match ended {
            Some(Event::ClaudeTurnEnded((session, summary))) => {
                assert_eq!(session, "s1");
                assert_eq!(summary.turn, 2);
                assert_eq!(summary.files[0].added, 3);
            }
            other => panic!("expected a turn's end, got {other:?}"),
        }
        assert_eq!(
            event(RemoteEvent::System(SystemEvent::Power(false))),
            Some(Event::SystemChanged(wit::SystemEvent::OnBattery))
        );
        assert_eq!(
            event(RemoteEvent::Region(RegionState::Blocked {
                country: "XX".into(),
                message: "No".into(),
            })),
            Some(Event::ClaudeRegion(wit::RegionState::Blocked((
                "XX".into(),
                "No".into()
            ))))
        );
        let action = RemoteEvent::Action {
            session: "s1".into(),
            action: "telegram".into(),
        };
        assert!(broadcast_event(action.clone()).is_none());
        assert_eq!(
            event(action),
            Some(Event::ClaudeAction(("s1".into(), "telegram".into())))
        );
    }

    #[test]
    fn a_request_keeps_its_choices() {
        let request = request(RemoteRequest {
            id: "r1".into(),
            kind: RequestKind::Questions(vec![RemoteQuestion {
                question: "Which?".into(),
                header: "Pick".into(),
                options: vec![("A".into(), "the first".into())],
                multi_select: true,
            }]),
        });
        match request.kind {
            wit::RequestKind::Questions(questions) => {
                assert_eq!(questions[0].options[0].label, "A");
                assert!(questions[0].multi_select);
            }
            other => panic!("expected questions, got {other:?}"),
        }
        assert_eq!(
            answer(wit::Response::Allow(Some("session".into()))),
            RemoteAnswer::Allow {
                choice: Some("session".into())
            }
        );
    }
}
