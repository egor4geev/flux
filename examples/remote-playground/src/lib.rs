//! The Remote Playground: the manual check of the plugin API 0.6 — Claude for remote work and
//! tasks between plugins — with no messenger, and an example to copy from.
//!
//! The service watches every Claude session of Flux (`remote::watch`), as a bot would: it keeps
//! the sessions, a readable log of each one's events (the last fifty), the limits, the country
//! check and the computer's state, and sends all of it to the windows' instances as JSON after
//! each change — nothing runs while nothing happens. A window shows it on the left island: the
//! status at the top (limits, country, the computer and the awake mode), tasks, and the sessions
//! of every window; «+» opens a new session in a project Flux knows in a tab; a session opens in a
//! tab of its own
//! (`session/<id>`): its request with the answers it allows, the mode, a field to write,
//! Interrupt, the turn's diff and transcript, the rollback's check and the rollback (on a second
//! press), the events. The windows make their calls themselves: the calls work in any instance.
//!
//! The plugin gives a few made-up tasks too (`task-source = true`): the service answers the
//! queries, and the window asks for them (`tasks::mine`, `search`, `get`) as another plugin would.
//! Its action in the Claude console («Show in the Plugin's Console») opens the session's tab and
//! shows the island.

use std::collections::{BTreeMap, HashMap};

use flux_plugin_api::host::ui;
use flux_plugin_api::remote::{
    self, Awake, Mode, Project, RegionState, Request, RequestKind, Response, Session, SessionState,
    SystemEvent, SystemState, TurnSummary, UsageLimits,
};
use flux_plugin_api::tasks::{self, Query, Task};
use flux_plugin_api::view::{self, *};
use flux_plugin_api::{CommandContext, Event, Plugin, UiEvent, register_plugin, service, tr, trf};
use serde::{Deserialize, Serialize};

/// The tool window on the left island.
const WINDOW: &str = "remote";
/// The window with a tab per session: `session/<id>`.
const TAB: &str = "session";
const TAB_PREFIX: &str = "session/";
/// The key of the new session's tab (sessions' ids are UUIDs).
const NEW: &str = "new";
/// The log lines kept per session.
const LOG_LINES: usize = 50;

/// What the service knows, sent to the windows as JSON.
#[derive(Default, Clone, Serialize, Deserialize)]
struct Shared {
    sessions: Vec<Session>,
    /// A readable log by session, the newest last.
    logs: BTreeMap<String, Vec<LogLine>>,
    limits: Option<UsageLimits>,
    region: Option<RegionState>,
    system: Option<SystemState>,
    /// The awake mode the service asked for; none — off.
    awake: Option<Awake>,
    /// The computer's events and the console's actions, the newest last.
    notes: Vec<String>,
    /// The session the console's action asked to show, with a count: a window shows it once.
    show: Option<(u32, String)>,
    /// Why watching failed.
    error: Option<String>,
}

/// A line of a session's log; a line with the same key (a message being written, a group of
/// steps) is replaced.
#[derive(Clone, Serialize, Deserialize)]
struct LogLine {
    key: String,
    text: String,
}

/// A window's message to the service.
#[derive(Serialize, Deserialize)]
enum ToService {
    /// Send me what you know.
    Hello,
    SetAwake(Awake),
}

/// The made-up tasks the plugin gives.
const TASKS: [(&str, &str, &str); 3] = [
    ("PLAY-1", "Speed up the project search", "In progress"),
    ("PLAY-2", "A dark icon for the console", "Open"),
    ("PLAY-3", "Explain the rollback in the guide", "Open"),
];

#[derive(Default)]
struct RemotePlayground {
    service: bool,
    shared: Shared,
    /// In a window: the island is shown.
    shown: bool,
    /// The console's action shown last.
    shown_action: u32,
    /// The island's fields.
    prompt: String,
    query: String,
    projects: Vec<Project>,
    project: Option<String>,
    /// The island's last call's result.
    result: String,
    tasks: Vec<Task>,
    /// The sessions' open tabs.
    tabs: BTreeMap<String, Tab>,
    /// The tab of a new session (`session/new`) is open.
    new_tab: bool,
    /// Its last call's result.
    new_result: String,
}

impl Plugin for RemotePlayground {
    fn new() -> Self {
        RemotePlayground {
            service: service::is_service(),
            ..Default::default()
        }
    }

    fn activate(&mut self) {
        if self.service {
            self.shared.error = remote::watch().err();
            self.refresh_all();
            self.publish();
        } else {
            let _ = service::send_json(&ToService::Hello);
        }
    }

    fn run_command(&mut self, command: &str, _context: &CommandContext) {
        if command == "open" {
            ui::show(WINDOW);
        }
    }

    fn on_event(&mut self, event: Event) {
        if self.service {
            self.service_event(event);
        } else {
            self.window_event(event);
        }
    }
}

// --- The service: watches and tells the windows ---

impl RemotePlayground {
    fn refresh_all(&mut self) {
        if let Ok(sessions) = remote::sessions() {
            self.shared.sessions = sessions;
        }
        self.shared.limits = remote::limits().ok();
        self.shared.region = remote::region(false).ok();
        self.shared.system = remote::system_status().ok();
    }

    fn publish(&self) {
        let _ = service::broadcast_json(&self.shared);
    }

    fn log(&mut self, session: &str, key: &str, text: String) {
        let lines = self.shared.logs.entry(session.to_string()).or_default();
        match lines
            .iter_mut()
            .find(|line| !key.is_empty() && line.key == key)
        {
            Some(line) => line.text = text,
            None => lines.push(LogLine {
                key: key.to_string(),
                text,
            }),
        }
        let extra = lines.len().saturating_sub(LOG_LINES);
        lines.drain(..extra);
    }

    fn note(&mut self, text: String) {
        self.shared.notes.push(text);
        let extra = self.shared.notes.len().saturating_sub(5);
        self.shared.notes.drain(..extra);
    }

    fn service_event(&mut self, event: Event) {
        match event {
            Event::ClaudeSession(session) => {
                let old = self.shared.sessions.iter().position(|s| s.id == session.id);
                let changed = old.is_none_or(|at| self.shared.sessions[at].state != session.state);
                if changed {
                    let state = state_word(session.state);
                    self.log(&session.id, "", trf("State: {0}", &[&state]));
                }
                match old {
                    Some(at) => self.shared.sessions[at] = session,
                    None => self.shared.sessions.insert(0, session),
                }
            }
            Event::ClaudeSessionGone(id) => {
                self.shared.sessions.retain(|session| session.id != id);
                self.shared.logs.remove(&id);
            }
            Event::ClaudeText(text) => {
                let mark = if text.done { "" } else { " …" };
                let line = format!("Claude: {}{mark}", text.text);
                self.log(&text.session, &format!("text-{}", text.message), line);
            }
            Event::ClaudeSteps((session, steps)) => {
                let count = steps.lines.len().to_string();
                let mut line = trf("Steps: {0} ({1})", &[&steps.summary, &count]);
                if steps.done {
                    line.push_str(" ✓");
                }
                self.log(&session, &format!("steps-{}", steps.group), line);
            }
            Event::ClaudeRequest((session, request)) => {
                let line = trf("Asks: {0}", &[&request_line(&request)]);
                self.log(&session, &format!("request-{}", request.id), line);
            }
            Event::ClaudeResolved((session, request)) => {
                self.log(
                    &session,
                    &format!("request-{request}"),
                    tr("The request is answered"),
                );
            }
            Event::ClaudeTurnEnded((session, summary)) => {
                self.log(&session, "", turn_line(&summary));
            }
            Event::ClaudeFailed((session, error)) => {
                self.log(&session, "", trf("Failed: {0}", &[&error]));
            }
            Event::ClaudeLimits(limits) => self.shared.limits = Some(limits),
            Event::ClaudeRegion(region) => self.shared.region = Some(region),
            Event::SystemChanged(change) => {
                let system = self.shared.system.get_or_insert(SystemState {
                    screen_locked: false,
                    on_power: true,
                });
                match change {
                    SystemEvent::ScreenLocked => system.screen_locked = true,
                    SystemEvent::ScreenUnlocked => system.screen_locked = false,
                    SystemEvent::OnPower => system.on_power = true,
                    SystemEvent::OnBattery => system.on_power = false,
                    SystemEvent::WillSleep | SystemEvent::DidWake => {}
                }
                self.note(system_word(change));
            }
            Event::ClaudeAction((session, action)) => {
                let count = self.shared.show.as_ref().map_or(0, |(count, _)| *count) + 1;
                self.shared.show = Some((count, session.clone()));
                let title = self.title_of(&session);
                self.note(trf("The console asked «{0}» for «{1}»", &[&action, &title]));
            }
            Event::TaskQuery(query) => {
                tasks::respond(query.id, Ok(made_up_tasks(&query.query)));
                return;
            }
            Event::ServiceMessage((Some(window), text)) => {
                match service::parse::<ToService>(&text) {
                    Some(ToService::Hello) => {
                        let _ = service::send_json_to(window, &self.shared);
                        return;
                    }
                    Some(ToService::SetAwake(awake)) => match remote::set_awake(awake) {
                        Ok(()) => self.shared.awake = Some(awake),
                        Err(err) => self.note(err),
                    },
                    None => return,
                }
            }
            Event::WindowOpened(window) => {
                let _ = service::send_json_to(window, &self.shared);
                return;
            }
            _ => return,
        }
        self.publish();
    }

    fn title_of(&self, session: &str) -> String {
        self.shared
            .sessions
            .iter()
            .find(|known| known.id == session)
            .map_or_else(|| session.to_string(), |known| title(known))
    }
}

/// The plugin's tasks for a query: all for `mine`, the matching ones for a search, one with its
/// text for `get`.
fn made_up_tasks(query: &Query) -> Vec<Task> {
    let all = TASKS
        .iter()
        .map(|(key, title, status)| tasks::task(key, &tr(title)).status(&tr(status)));
    match query {
        Query::Mine => all.collect(),
        Query::Search(text) => {
            let text = text.to_lowercase();
            all.filter(|task| {
                task.title.to_lowercase().contains(&text) || task.key.to_lowercase() == text
            })
            .collect()
        }
        Query::Get(key) => all
            .filter(|task| task.key.eq_ignore_ascii_case(key))
            .map(|task| {
                let text = format!(
                    "# {}\n\n{}",
                    task.title,
                    tr("A made-up task of the playground.")
                );
                task.markdown(&text)
            })
            .collect(),
    }
}

// --- A window: shows and calls ---

/// What a session's tab keeps: its fields, the last call's result, a diff or a check.
#[derive(Default)]
struct Tab {
    message: String,
    feedback: String,
    result: String,
    /// A diff, a transcript or a rollback's check: Markdown.
    detail: Option<String>,
    /// The rollback was checked: the next press of Rollback rolls the turn back.
    armed: bool,
    /// The chosen options of the request's questions, by question.
    answers: HashMap<usize, String>,
}

impl RemotePlayground {
    fn window_event(&mut self, event: Event) {
        match event {
            Event::ServiceStarted => {
                let _ = service::send_json(&ToService::Hello);
            }
            Event::ServiceMessage((None, text)) => {
                let Some(shared) = service::parse::<Shared>(&text) else {
                    return;
                };
                self.shared = shared;
                if let Some((count, session)) = self.shared.show.clone()
                    && count > self.shown_action
                {
                    self.shown_action = count;
                    ui::show(WINDOW);
                    self.open_session(&session);
                }
                self.render();
            }
            Event::ToolWindowShown(window) if window == WINDOW => {
                self.shown = true;
                if self.projects.is_empty() {
                    match remote::projects() {
                        Ok(projects) => {
                            self.project = projects.first().map(|p| p.path.clone());
                            self.projects = projects;
                        }
                        Err(err) => self.result = trf("Failed: {0}", &[&err]),
                    }
                }
                self.render();
            }
            Event::ToolWindowHidden(window) if window == WINDOW => self.shown = false,
            Event::TabClosed(window) => match window.strip_prefix(TAB_PREFIX) {
                Some(NEW) => self.new_tab = false,
                Some(session) => {
                    self.tabs.remove(session);
                }
                None => {}
            },
            Event::Ui(input) if input.window == WINDOW => {
                match input.event {
                    UiEvent::Clicked => self.clicked(&input.element),
                    UiEvent::Changed(text) if input.element == "query" => self.query = text,
                    UiEvent::Submitted(text) if input.element == "query" => {
                        self.query = text;
                        self.clicked("tasks-search");
                    }
                    UiEvent::Selected(key) | UiEvent::Activated(key)
                        if input.element == "sessions" =>
                    {
                        self.open_session(&key)
                    }
                    _ => return,
                }
                self.render();
            }
            Event::Ui(input) if input.window == format!("{TAB_PREFIX}{NEW}") => {
                match input.event {
                    UiEvent::Clicked => self.clicked(&input.element),
                    UiEvent::Changed(text) if input.element == "prompt" => self.prompt = text,
                    UiEvent::Submitted(text) if input.element == "prompt" => {
                        self.prompt = text;
                        self.clicked("new-session");
                    }
                    _ => return,
                }
                self.render();
            }
            Event::Ui(input) => {
                let Some(session) = input.window.strip_prefix(TAB_PREFIX).map(str::to_string)
                else {
                    return;
                };
                let tab = self.tabs.entry(session.clone()).or_default();
                match input.event {
                    UiEvent::Clicked => self.tab_clicked(&session, &input.element),
                    // The view holds the field's text: emptying it after Send is a change.
                    UiEvent::Changed(text) => match input.element.as_str() {
                        "message" => tab.message = text,
                        "feedback" => tab.feedback = text,
                        _ => return,
                    },
                    UiEvent::Submitted(text) if input.element == "message" => {
                        tab.message = text;
                        self.tab_clicked(&session, "send");
                    }
                    _ => return,
                }
                self.render_tab(&session);
            }
            _ => {}
        }
    }

    /// Opens (or brings forward) the session's tab.
    fn open_session(&mut self, session: &str) {
        let title = self
            .shared
            .sessions
            .iter()
            .find(|known| known.id == session)
            .map_or_else(|| session.to_string(), title);
        view::open_tab(TAB, session, &title, true);
        self.tabs.entry(session.to_string()).or_default();
        self.render_tab(session);
    }

    /// The island's buttons, and the new session's tab's.
    fn clicked(&mut self, element: &str) {
        match element {
            "new" => {
                view::open_tab(TAB, NEW, &tr("New session"), true);
                self.new_tab = true;
            }
            "limits" => match remote::limits() {
                Ok(limits) => {
                    self.shared.limits = Some(limits);
                    self.result = tr("The limits are fresh");
                }
                Err(err) => self.result = trf("Failed: {0}", &[&err]),
            },
            "region" => match remote::region(true) {
                Ok(region) => {
                    self.result = trf("Country: {0}", &[&region_word(&region)]);
                    self.shared.region = Some(region);
                }
                Err(err) => self.result = trf("Failed: {0}", &[&err]),
            },
            "system" => match remote::system_status() {
                Ok(system) => {
                    self.shared.system = Some(system);
                    self.result = tr("The computer's state is fresh");
                }
                Err(err) => self.result = trf("Failed: {0}", &[&err]),
            },
            "awake-off" | "awake-always" | "awake-power" => {
                let awake = match element {
                    "awake-always" => Awake::Always,
                    "awake-power" => Awake::OnPower,
                    _ => Awake::Off,
                };
                self.result = match service::send_json(&ToService::SetAwake(awake)) {
                    Ok(()) => tr("Sent to the service"),
                    Err(err) => trf("Failed: {0}", &[&err]),
                };
            }
            "tasks-mine" | "tasks-search" | "tasks-get" => {
                let found = match element {
                    "tasks-mine" => tasks::mine(),
                    "tasks-search" => tasks::search(&self.query),
                    _ => tasks::get(&self.query).map(|task| task.into_iter().collect()),
                };
                match found {
                    Ok(found) => {
                        let count = found.len().to_string();
                        self.result = trf("Tasks found: {0}", &[&count]);
                        self.tasks = found;
                    }
                    Err(err) => self.result = trf("Failed: {0}", &[&err]),
                }
            }
            "new-session" => {
                let Some(project) = self.project.clone() else {
                    self.new_result = tr("Choose a project");
                    return;
                };
                let mut spec = remote::new_session(&project);
                if !self.prompt.trim().is_empty() {
                    spec = spec.prompt(self.prompt.trim());
                }
                match spec.start() {
                    Ok(id) => {
                        self.prompt.clear();
                        self.new_result.clear();
                        if self.new_tab {
                            ui::hide(&format!("{TAB_PREFIX}{NEW}"));
                            self.new_tab = false;
                        }
                        self.open_session(&id);
                    }
                    Err(err) => self.new_result = trf("Failed: {0}", &[&err]),
                }
            }
            other => {
                if let Some(index) = other.strip_prefix("project-") {
                    let index: usize = index.parse().unwrap_or_default();
                    self.project = self.projects.get(index).map(|p| p.path.clone());
                }
            }
        }
    }

    /// A session tab's buttons.
    fn tab_clicked(&mut self, session: &str, element: &str) {
        let request = self
            .shared
            .sessions
            .iter()
            .find(|known| known.id == session)
            .and_then(|known| known.request.clone());
        let tab = self.tabs.entry(session.to_string()).or_default();
        let report = |tab: &mut Tab, ok: &str, result: Result<(), String>| {
            tab.result = match result {
                Ok(()) => tr(ok),
                Err(err) => trf("Failed: {0}", &[&err]),
            };
        };
        match element {
            "send" => {
                let text = std::mem::take(&mut tab.message);
                let sent = remote::send(session, &text);
                if sent.is_err() {
                    tab.message = text;
                }
                report(tab, "Sent", sent);
            }
            "interrupt" => report(tab, "Interrupted", remote::interrupt(session)),
            "mode-ask" | "mode-edits" | "mode-plan" | "mode-bypass" => {
                let mode = match element {
                    "mode-edits" => Mode::AcceptEdits,
                    "mode-plan" => Mode::Plan,
                    "mode-bypass" => Mode::Bypass,
                    _ => Mode::Ask,
                };
                report(tab, "The mode is set", remote::set_mode(session, mode));
            }
            "diff" => match remote::turn_diff(session, None) {
                Ok(diff) => tab.detail = Some(format!("```diff\n{diff}\n```")),
                Err(err) => tab.result = trf("Failed: {0}", &[&err]),
            },
            "transcript" => match remote::transcript(session, 6) {
                Ok(items) => {
                    let lines: Vec<String> = items
                        .iter()
                        .map(|item| {
                            let who = if item.user {
                                tr("You")
                            } else {
                                "Claude".to_string()
                            };
                            format!("**{who}:** {}", item.text)
                        })
                        .collect();
                    tab.detail = Some(lines.join("\n\n"));
                }
                Err(err) => tab.result = trf("Failed: {0}", &[&err]),
            },
            "check-rollback" => check_rollback(tab, session),
            "rollback" => {
                if tab.armed {
                    tab.armed = false;
                    tab.detail = None;
                    report(
                        tab,
                        "The turn is rolled back",
                        remote::rollback(session, None),
                    );
                } else {
                    check_rollback(tab, session);
                    if tab.armed {
                        tab.result = tr("Press Rollback again to roll the turn back");
                    }
                }
            }
            other => {
                if let Some(request) = request {
                    answer(tab, other, session, &request);
                }
            }
        }
    }
}

/// Shows what rolling the session's last turn back would do, and arms Rollback.
fn check_rollback(tab: &mut Tab, session: &str) {
    match remote::check_rollback(session, None) {
        Ok(check) => {
            let mut lines = vec![tr("**Rolling the turn back returns:**")];
            lines.extend(check.files.iter().map(|file| {
                if check.changed_since.contains(file) {
                    trf("- {0} — changed since, that goes too", &[file])
                } else {
                    format!("- {file}")
                }
            }));
            lines.extend(
                check
                    .created
                    .iter()
                    .map(|file| trf("- {0} — created, goes to the Trash", &[file])),
            );
            tab.detail = Some(lines.join("\n"));
            tab.armed = true;
        }
        Err(err) => tab.result = trf("Failed: {0}", &[&err]),
    }
}

/// A button of the request: an answer to a permission, a question's option, the plan.
fn answer(tab: &mut Tab, element: &str, session: &str, request: &Request) {
    let response = match (element, &request.kind) {
        ("allow", RequestKind::Permission(_)) => Response::Allow(None),
        ("deny", _) => Response::Deny(String::new()),
        ("plan-edits", RequestKind::Plan(_)) => Response::ApprovePlan(true),
        ("plan-ask", RequestKind::Plan(_)) => Response::ApprovePlan(false),
        ("plan-keep", RequestKind::Plan(_)) => {
            Response::KeepPlanning(std::mem::take(&mut tab.feedback))
        }
        ("answer", RequestKind::Questions(questions)) => Response::Answers(
            (0..questions.len())
                .map(|index| tab.answers.get(&index).cloned().into_iter().collect())
                .collect(),
        ),
        (other, RequestKind::Permission(permission)) => {
            let Some(choice) = other.strip_prefix("choice-") else {
                return;
            };
            let Some((id, _)) = permission.choices.iter().find(|(id, _)| id == choice) else {
                return;
            };
            Response::Allow(Some(id.clone()))
        }
        (other, RequestKind::Questions(questions)) => {
            // "option-<question>-<option>": chosen, answered together by Answer.
            let Some(rest) = other.strip_prefix("option-") else {
                return;
            };
            let (question, option) = rest.split_once('-').unwrap_or((rest, ""));
            let question: usize = question.parse().unwrap_or(0);
            let option: usize = option.parse().unwrap_or(0);
            if let Some(label) = questions
                .get(question)
                .and_then(|q| q.options.get(option))
                .map(|o| o.label.clone())
            {
                tab.answers.insert(question, label);
            }
            return;
        }
        _ => return,
    };
    tab.answers.clear();
    tab.result = match remote::answer(session, &request.id, &response) {
        Ok(()) => tr("Answered"),
        Err(err) => trf("Failed: {0}", &[&err]),
    };
}

// --- The views ---

impl RemotePlayground {
    fn render(&self) {
        for session in self.tabs.keys() {
            self.render_tab(session);
        }
        if self.new_tab {
            let mut children = self.new_session();
            if !self.new_result.is_empty() {
                children.push(text("result", [span(&self.new_result).tone(Tone::Dim)]));
            }
            set_view(&format!("{TAB_PREFIX}{NEW}"), column("root", children));
        }
        if !self.shown {
            return;
        }
        let mut children: Vec<Element> = Vec::new();
        children.extend(self.status());
        if !self.result.is_empty() {
            children.push(text("result", [span(&self.result).tone(Tone::Dim)]));
        }
        children.push(divider("divider-sessions"));
        children.push(label("sessions-title", &tr("Sessions of all windows")));
        // Last: the list takes the remaining height and scrolls.
        children.push(self.sessions());
        set_view(WINDOW, column("root", children));
    }

    fn status(&self) -> Vec<Element> {
        let shared = &self.shared;
        let limits = match &shared.limits {
            Some(limits) => limits_line(limits),
            None => "—".to_string(),
        };
        let region = shared.region.as_ref().map_or("—".into(), region_word);
        let system = match &shared.system {
            Some(system) => {
                let screen = if system.screen_locked {
                    tr("screen locked")
                } else {
                    tr("screen unlocked")
                };
                let power = if system.on_power {
                    tr("on the adapter")
                } else {
                    tr("on the battery")
                };
                format!("{screen} · {power}")
            }
            None => "—".to_string(),
        };
        let mut properties_list = vec![
            property("limits-value", &tr("Limits"), vec![span(&limits)]),
            property("region-value", &tr("Country"), vec![span(&region)]),
            property("system-value", &tr("Computer"), vec![span(&system)]),
        ];
        if let Some(error) = &shared.error {
            let error = trf("Doesn't watch: {0}", &[error]);
            properties_list.push(property(
                "error",
                &tr("Watch"),
                vec![span(&error).tone(Tone::Error)],
            ));
        }
        let awake = |id: &str, label: &str, mode: Awake| -> Element {
            let mut button = button(id, &tr(label));
            if shared.awake.unwrap_or(Awake::Off) == mode {
                button = button.primary();
            }
            button.into()
        };
        let mut elements = vec![
            toolbar(
                "toolbar",
                [
                    icon_button("new", "plus", &tr("New session")),
                    icon_button("limits", "refresh", &tr("Refresh the limits")),
                    icon_button("region", "globe", &tr("Check the country again")),
                    icon_button("system", "eye", &tr("The computer's state")),
                ],
            ),
            properties("status", properties_list),
            label("awake-title", &tr("Keep the computer awake")),
            row(
                "awake",
                [
                    awake("awake-off", "Off", Awake::Off),
                    awake("awake-always", "Always", Awake::Always),
                    awake("awake-power", "On the Adapter", Awake::OnPower),
                ],
            ),
        ];
        // The newest of the computer's events and the console's actions.
        if let Some(note) = shared.notes.last() {
            elements.push(text("notes", [span(note).tone(Tone::Dim)]));
        }
        elements.push(divider("divider-tasks"));
        elements.push(label("tasks-title", &tr("Tasks of other plugins")));
        elements.push(text_field(
            "query",
            &self.query,
            Some(&tr("Words or a key")),
        ));
        elements.push(row(
            "tasks",
            [
                button("tasks-mine", &tr("My Tasks")).into(),
                button("tasks-search", &tr("Find")).into(),
                button("tasks-get", &tr("Get")).into(),
            ],
        ));
        if !self.tasks.is_empty() {
            let lines: Vec<String> = self
                .tasks
                .iter()
                .map(|task| {
                    let mut line = format!("- **{}** {} · {}", task.key, task.title, task.status);
                    if !task.markdown.is_empty() {
                        line.push_str(&format!("\n\n{}", task.markdown));
                    }
                    line
                })
                .collect();
            elements.push(markdown("tasks-list", &lines.join("\n")));
        }
        elements
    }

    fn sessions(&self) -> Element {
        let mut tree = Tree::new().two_line().empty_text(&tr("No Claude sessions"));
        for session in &self.shared.sessions {
            let project = project_name(&session.project);
            let mut subtitle = vec![span(&project).tone(Tone::Dim)];
            let line = match &session.request {
                Some(request) => request_line(request),
                None => session.activity.clone(),
            };
            if !line.is_empty() {
                subtitle.push(span(&format!(" · {line}")).tone(Tone::Dim));
            }
            let (badge, tone) = state_badge(session);
            let row = RowSpec::new(&session.id, &title(session))
                .subtitle(subtitle)
                .badge(&badge)
                .badge_tone(tone);
            tree.add(row);
        }
        tree.into_element("sessions")
    }

    fn new_session(&self) -> Vec<Element> {
        let projects = self
            .projects
            .iter()
            .enumerate()
            .map(|(index, project)| -> Element {
                let mut button = button(&format!("project-{index}"), &project.name);
                if self.project.as_deref() == Some(project.path.as_str()) {
                    button = button.primary();
                }
                button.into()
            });
        vec![
            header("new-title", &tr("New session")).into(),
            label("projects-title", &tr("Project")),
            row("projects", projects),
            text_field("prompt", &self.prompt, Some(&tr("The first message"))),
            row(
                "start",
                [button("new-session", &tr("Start")).primary().into()],
            ),
        ]
    }

    /// A session's tab: its request with the answers it allows, the mode, a field to write,
    /// Interrupt, the turn's diff and rollback, the events.
    fn render_tab(&self, id: &str) {
        let Some(tab) = self.tabs.get(id) else {
            return;
        };
        let window = format!("{TAB_PREFIX}{id}");
        let Some(session) = self.shared.sessions.iter().find(|known| known.id == id) else {
            let gone = label("gone", &tr("The session is gone"));
            set_view(&window, column("root", [gone]));
            return;
        };
        let state = state_word(session.state);
        let mode = mode_word(session.mode);
        let mut elements = vec![
            header("session-title", &title(session))
                .subtitle([span(&format!(
                    "{} · {mode} · {state}",
                    project_name(&session.project)
                ))
                .tone(Tone::Dim)])
                .into(),
        ];
        if let Some(request) = &session.request {
            elements.extend(request_elements(tab, request));
        }
        let mode_button = |id: &str, label: &str, value: Mode| -> Element {
            let mut button = button(id, &tr(label));
            if session.mode == value {
                button = button.primary();
            }
            button.into()
        };
        elements.push(row(
            "modes",
            [
                mode_button("mode-ask", "Ask", Mode::Ask),
                mode_button("mode-edits", "Accept Edits", Mode::AcceptEdits),
                mode_button("mode-plan", "Plan", Mode::Plan),
                mode_button("mode-bypass", "No Checks", Mode::Bypass),
            ],
        ));
        elements.push(text_field(
            "message",
            &tab.message,
            Some(&tr("A message to Claude")),
        ));
        let working = matches!(
            session.state,
            SessionState::Working | SessionState::Waiting | SessionState::Starting
        );
        let mut rollback = button("rollback", &tr("Rollback"));
        if tab.armed {
            rollback = rollback.primary();
        }
        elements.push(row(
            "buttons",
            [
                button("send", &tr("Send")).primary().into(),
                button("interrupt", &tr("Interrupt"))
                    .enabled(working)
                    .into(),
                button("diff", &tr("Diff")).into(),
                button("transcript", &tr("Transcript")).into(),
                button("check-rollback", &tr("Check Rollback")).into(),
                rollback.into(),
            ],
        ));
        if !tab.result.is_empty() {
            elements.push(text("result", [span(&tab.result).tone(Tone::Dim)]));
        }
        if let Some(detail) = &tab.detail {
            elements.push(markdown("detail", detail));
        }
        let lines: Vec<String> = self
            .shared
            .logs
            .get(id)
            .map(|lines| lines.iter().map(|line| line.text.clone()).collect())
            .unwrap_or_default();
        let log = if lines.is_empty() {
            tr("No events yet")
        } else {
            lines.join("\n")
        };
        elements.push(label("log-title", &tr("Events")));
        elements.push(markdown("log", &format!("```\n{log}\n```")));
        set_view(&window, column("root", elements));
    }
}

/// The request's text and its answers.
fn request_elements(tab: &Tab, request: &Request) -> Vec<Element> {
    let mut elements = Vec::new();
    match &request.kind {
        RequestKind::Permission(permission) => {
            let mut what = permission.title.clone();
            if let Some(subject) = &permission.subject {
                // «Выполнить команду? `ls`», «Изменить: `src/main.rs`».
                let separator = if what.ends_with('?') { " " } else { ": " };
                what.push_str(&format!("{separator}`{subject}`"));
            }
            elements.push(markdown("request-text", &what));
            if let Some(diff) = &permission.diff {
                elements.push(markdown("request-diff", &format!("```diff\n{diff}\n```")));
            }
            let mut buttons: Vec<Element> =
                vec![button("allow", &tr("Allow Once")).primary().into()];
            for (id, label) in &permission.choices {
                buttons.push(button(&format!("choice-{id}"), label).into());
            }
            buttons.push(button("deny", &tr("Deny")).into());
            elements.push(row("request-buttons", buttons));
        }
        RequestKind::Questions(questions) => {
            for (index, question) in questions.iter().enumerate() {
                elements.push(markdown(
                    &format!("question-{index}"),
                    &format!("**{}** {}", question.header, question.question),
                ));
                let chosen = tab.answers.get(&index);
                let options = question
                    .options
                    .iter()
                    .enumerate()
                    .map(|(at, option)| -> Element {
                        let mut button = button(&format!("option-{index}-{at}"), &option.label)
                            .tooltip(&option.description);
                        if chosen == Some(&option.label) {
                            button = button.primary();
                        }
                        button.into()
                    });
                elements.push(row(&format!("options-{index}"), options));
            }
            elements.push(row(
                "request-buttons",
                [
                    button("answer", &tr("Answer"))
                        .enabled(tab.answers.len() == questions.len())
                        .primary()
                        .into(),
                    button("deny", &tr("Deny")).into(),
                ],
            ));
        }
        RequestKind::Plan(plan) => {
            elements.push(markdown("request-plan", plan));
            elements.push(row(
                "request-buttons",
                [
                    button("plan-edits", &tr("Run, Accepting Edits"))
                        .primary()
                        .into(),
                    button("plan-ask", &tr("Run, Asking")).into(),
                ],
            ));
            elements.push(text_field(
                "feedback",
                &tab.feedback,
                Some(&tr("What to change")),
            ));
            elements.push(row(
                "request-feedback",
                [button("plan-keep", &tr("Keep Planning")).into()],
            ));
        }
    }
    elements
}

// --- Words ---

fn title(session: &Session) -> String {
    if session.title.is_empty() {
        tr("Untitled")
    } else {
        session.title.clone()
    }
}

fn project_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

fn state_word(state: SessionState) -> String {
    tr(match state {
        SessionState::Starting => "starting",
        SessionState::Working => "working",
        SessionState::Waiting => "waiting for you",
        SessionState::Idle => "idle",
        SessionState::Unloaded => "unloaded",
    })
}

fn state_badge(session: &Session) -> (String, Tone) {
    match session.state {
        SessionState::Waiting => (tr("Asks"), Tone::Warning),
        SessionState::Working | SessionState::Starting => (tr("Works"), Tone::Accent),
        SessionState::Idle if session.unseen => (tr("Done"), Tone::Success),
        SessionState::Idle => (tr("Idle"), Tone::Muted),
        SessionState::Unloaded => (tr("Unloaded"), Tone::Dim),
    }
}

fn mode_word(mode: Mode) -> String {
    tr(match mode {
        Mode::Ask => "Ask",
        Mode::AcceptEdits => "Accept Edits",
        Mode::Plan => "Plan",
        Mode::Bypass => "No Checks",
        Mode::Other => "Another Mode",
    })
}

fn request_line(request: &Request) -> String {
    match &request.kind {
        RequestKind::Permission(permission) => match &permission.subject {
            Some(subject) => format!("{} — {subject}", permission.title),
            None => permission.title.clone(),
        },
        RequestKind::Questions(questions) => {
            let count = questions.len().to_string();
            trf("Claude's questions: {0}", &[&count])
        }
        RequestKind::Plan(_) => tr("The plan, for approval"),
    }
}

fn turn_line(summary: &TurnSummary) -> String {
    let files: Vec<String> = summary
        .files
        .iter()
        .map(|file| format!("{} +{} −{}", file.path, file.added, file.removed))
        .collect();
    let seconds = summary.seconds.to_string();
    let turn = summary.turn.to_string();
    if summary.failed {
        trf("Turn {0} ended with an error in {1} s", &[&turn, &seconds])
    } else if files.is_empty() {
        trf("Turn {0} ended in {1} s", &[&turn, &seconds])
    } else {
        trf(
            "Turn {0} ended in {1} s: {2}",
            &[&turn, &seconds, &files.join(", ")],
        )
    }
}

fn limits_line(limits: &UsageLimits) -> String {
    let window = |name: &str, window: &Option<remote::UsageWindow>| match window {
        Some(window) => format!("{} {}%", tr(name), window.percent),
        None => format!("{} —", tr(name)),
    };
    let mut line = format!(
        "{} · {}",
        window("5 h", &limits.five_hour),
        window("7 days", &limits.seven_day)
    );
    if limits.rejected {
        line.push_str(&format!(" · {}", tr("the limit is reached")));
    }
    line
}

fn region_word(region: &RegionState) -> String {
    match region {
        RegionState::Allowed => tr("Claude is available"),
        RegionState::Checking => tr("checking…"),
        RegionState::Blocked((country, _)) if !country.is_empty() => {
            trf("Claude is blocked ({0})", &[country])
        }
        RegionState::Blocked(_) => tr("Claude is blocked"),
    }
}

fn system_word(change: SystemEvent) -> String {
    tr(match change {
        SystemEvent::ScreenLocked => "The screen is locked",
        SystemEvent::ScreenUnlocked => "The screen is unlocked",
        SystemEvent::WillSleep => "The computer goes to sleep",
        SystemEvent::DidWake => "The computer woke up",
        SystemEvent::OnPower => "On the adapter",
        SystemEvent::OnBattery => "On the battery",
    })
}

register_plugin!(RemotePlayground);
