//! Claude Code in the window (stage 9, wiki ADR-030): the hub of the window's Claude sessions.
//! It finds `claude` and checks the sign-in, starts and closes sessions, keeps the latest
//! subscription limits, and tells the window what needs it: a question for the user, an edit to
//! show in a diff tab, files Claude changed, a session that finished or failed.
//!
//! A session's chat lives in an editor tab ([`crate::claude_chat`]); the console in the left island
//! ([`crate::claude_console`]) lists the sessions (part «Пульт Claude»). Closing a chat's tab doesn't
//! end its session: one that works or waits for the user goes on, an idle one is unloaded — its
//! process stops, the console keeps it, opening it resumes it. Keys, as the Claude Code plugin for
//! JetBrains IDEs: ⌘Esc focuses the console or goes back to the last chat, ⌥⌘K puts the selection
//! into the message.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use gpui::{
    Action, App, AppContext, Context, Entity, EntityId, EventEmitter, KeyBinding, Subscription,
    Task, actions,
};

use flux_claude::{
    Cli, EntryKind, LaunchOptions, Notice, PendingKind, RateLimits, SavedSession, Status,
};

use crate::claude_console::SessionRef;
use crate::claude_history::{self, ConsoleState, OpenSessions, SessionPlace};
use crate::claude_session::{ClaudeSession, SessionEvent};
use crate::i18n::{tr, trf};
use crate::settings;

actions!(
    claude,
    [
        /// ⌘Esc: shows the console and focuses it; from the console, back to the last chat (else the
        /// editor).
        ToggleClaude,
        /// ⌥⌘K: the editor's file and selected lines go into the message as a mention.
        AddSelectionToClaude,
        /// A new session in a new editor tab.
        NewSession,
        /// Stops the running turn of the current session (Esc in the message field).
        Interrupt,
        /// The current session goes on in the terminal CLI (`claude --resume`): for what only the
        /// terminal does (`/config`, `/mcp`).
        OpenInTerminal,
        /// Renames the current session.
        RenameSession,
        /// Closes the current session's tab (the session stays in the console).
        CloseSession,
        /// Signs in to Claude: `claude auth login` in a terminal.
        SignIn,
        /// Looks for `claude` and the sign-in again (after installing or signing in).
        CheckAgain,
        /// Settings → Claude Code.
        OpenSettings,
        /// "Resume Session…" (the history button, `/resume`, the palette): the project's saved
        /// sessions in a popup; the chosen one comes back in a tab (part 9.2).
        ResumeSession,
    ]
);

/// Shows a session's chat: the "Show" of its notification.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = claude, no_json)]
pub struct ShowSession(pub EntityId);

/// Types a command into a new terminal without running it (installing Claude Code).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = claude, no_json)]
pub struct TypeInTerminal(pub String);

/// Why a session's process stopped: its last output in a dialog (an error card's "Details").
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = claude, no_json)]
pub struct ShowExitDetails {
    pub title: String,
    pub output: String,
}

/// The official installer and Homebrew, as the Claude Code documentation gives them.
pub const INSTALL_COMMANDS: [(&str, &str); 2] = [
    (
        "Native installer",
        "curl -fsSL https://claude.ai/install.sh | bash",
    ),
    ("Homebrew", "brew install --cask claude-code"),
];

/// How often the sessions' badges redraw while one works (their clocks, part 10.2).
const BADGE_TICK: Duration = Duration::from_secs(1);

/// How often and how long the sign-in is checked after "Sign In" opened `claude auth login`.
const SIGN_IN_POLL: Duration = Duration::from_secs(2);
const SIGN_IN_WAIT: Duration = Duration::from_secs(300);

pub fn init(cx: &mut App) {
    crate::system_notify::init(cx);
    cx.bind_keys([
        KeyBinding::new("cmd-escape", ToggleClaude, Some("Workspace")),
        KeyBinding::new("alt-cmd-k", AddSelectionToClaude, Some("Editor")),
    ]);
}

/// Whether Claude Code is on (Settings → Claude Code): off hides the launchpad icon and keys.
pub fn enabled(cx: &App) -> bool {
    settings::claude(cx).enabled
}

/// Where `claude` stands for this window.
#[derive(Debug, Clone, PartialEq)]
pub enum CliState {
    /// Looking for it, asking its version and the sign-in.
    Checking,
    /// Not installed (nowhere on the search path).
    Missing,
    /// Installed, signed out.
    SignedOut { cli: Cli, version: Option<String> },
    Ready {
        cli: Cli,
        version: String,
        /// The account's email.
        account: Option<String>,
    },
    /// It is there but doesn't answer as expected.
    Failed(String),
}

impl CliState {
    /// The executable, when one was found.
    pub fn cli(&self) -> Option<&Cli> {
        match self {
            CliState::SignedOut { cli, .. } | CliState::Ready { cli, .. } => Some(cli),
            _ => None,
        }
    }
}

/// The overall state of the window's sessions: the launchpad's dot, the status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    None,
    Idle,
    Working,
    /// A session waits for the user.
    Waiting,
}

/// What the hub tells the window.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaudeStoreEvent {
    SessionAdded(Entity<ClaudeSession>),
    SessionRemoved(Entity<ClaudeSession>),
    /// A session asks the user something (the CLI's request id).
    Attention {
        session: Entity<ClaudeSession>,
        request: String,
    },
    /// The question went (answered, withdrawn).
    Resolved {
        session: Entity<ClaudeSession>,
        request: String,
    },
    /// A session finished its work: nothing runs in it any more after a turn.
    Finished {
        session: Entity<ClaudeSession>,
        is_error: bool,
    },
    /// A session's process stopped by itself with an error.
    Failed {
        session: Entity<ClaudeSession>,
        code: Option<i32>,
        stderr: Vec<String>,
    },
    /// Claude changed these files.
    FilesChanged(Vec<PathBuf>),
    /// The CLI's state, the limits, a session's status or title: redraw.
    Changed,
    /// Claude calls a Flux tool: the window runs it ([`crate::claude_tools::call`]) and answers
    /// the session ([`ClaudeSession::answer_tool`]).
    ToolCall {
        session: Entity<ClaudeSession>,
        request: String,
        tool: String,
        input: serde_json::Value,
    },
    /// An edit of Claude ran: the window tells Claude what language servers now find wrong in the
    /// file ([`crate::claude_tools::after_edit`], [`ClaudeSession::answer_edit_hook`]).
    EditHook {
        session: Entity<ClaudeSession>,
        request: String,
        path: PathBuf,
    },
    /// A session open when the project last closed came back (Settings → Claude Code → "Reopen
    /// sessions with the project"): its chat goes back to an editor tab (`active` — the active one)
    /// without coming into sight.
    SessionRestored {
        session: Entity<ClaudeSession>,
        place: SessionPlace,
        active: bool,
    },
}

/// A session of the console's "Now".
#[derive(Debug, Clone, PartialEq)]
pub enum ConsoleItem {
    /// In this window: working, waiting, idle, its process maybe not started yet.
    Live(Entity<ClaudeSession>),
    /// Unloaded: no process, no chat; opening it resumes it.
    Unloaded(SavedSession),
}

/// Bringing back the sessions open when the project last closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Restore {
    /// Not yet for this root: `claude` isn't ready.
    Pending,
    /// The transcripts are being listed.
    Running,
    /// Done (or nothing to do): from now on the open sessions are saved.
    Done,
}

pub struct ClaudeStore {
    root: Option<PathBuf>,
    cli: CliState,
    sessions: Vec<Entity<ClaudeSession>>,
    limits: Option<RateLimits>,
    /// Sessions that worked since they were last idle: their idle is the end of the work.
    busy: HashSet<EntityId>,
    _check: Option<Task<()>>,
    /// Polls the sign-in while `claude auth login` runs in a terminal.
    _sign_in: Option<Task<()>>,
    subscriptions: Vec<(Entity<ClaudeSession>, Subscription)>,
    /// The sessions of the root open when it last closed: they come back once `claude` is ready;
    /// only then is the window's state saved over them.
    restore: Restore,
    _restore: Option<Task<()>>,
    /// What was last saved for the root: an unchanged state isn't written again.
    saved_open: Option<OpenSessions>,
    /// When each session last finished its work: "Done · 2 min ago" in its status tooltip.
    finished: HashMap<EntityId, Instant>,
    /// Redraws the window each second while a session works: the clocks on the sessions' badges.
    _ticker: Option<Task<()>>,
    // --- The console (part «Пульт Claude») ---
    /// "Now" by CLI id: the sessions of the console that may be unloaded (live ones are found by
    /// their entities). The latest first.
    console_ids: Vec<String>,
    /// The archive: out of "Now", still in the history.
    archived: Vec<String>,
    /// Sessions that finished their work out of sight: "Done" until their chat is shown.
    unseen: HashSet<EntityId>,
    /// When each live session last finished a turn in this window.
    worked: HashMap<EntityId, SystemTime>,
    /// Sessions just unloaded, until the saved list has them.
    placeholders: HashMap<String, SavedSession>,
    /// The root's saved sessions (`None` until first read).
    saved: Option<Vec<SavedSession>>,
    _saved: Option<Task<()>>,
    /// The console as last written for the root: `None` until it was read.
    saved_console: Option<ConsoleState>,
}

impl EventEmitter<ClaudeStoreEvent> for ClaudeStore {}

impl ClaudeStore {
    pub fn new(root: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let mut store = Self {
            root,
            cli: CliState::Checking,
            sessions: Vec::new(),
            limits: None,
            busy: HashSet::new(),
            _check: None,
            _sign_in: None,
            subscriptions: Vec::new(),
            restore: Restore::Pending,
            _restore: None,
            saved_open: None,
            finished: HashMap::new(),
            _ticker: None,
            console_ids: Vec::new(),
            archived: Vec::new(),
            unseen: HashSet::new(),
            worked: HashMap::new(),
            placeholders: HashMap::new(),
            saved: None,
            _saved: None,
            saved_console: None,
        };
        store.load_console();
        store.refresh_saved(cx);
        store.check(cx);
        store
    }

    pub fn cli(&self) -> &CliState {
        &self.cli
    }

    pub fn is_ready(&self) -> bool {
        matches!(self.cli, CliState::Ready { .. })
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// The project root changed: new sessions work there (running ones keep theirs); the sessions
    /// the new project had open come back.
    pub fn set_root(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.root != root {
            self.root = root;
            self.restore = Restore::Pending;
            self._restore = None;
            self.saved_open = None;
            self.load_console();
            self.saved = None;
            self.refresh_saved(cx);
            self.restore(cx);
        }
        cx.notify();
    }

    pub fn sessions(&self) -> &[Entity<ClaudeSession>] {
        &self.sessions
    }

    // --- The console (part «Пульт Claude») ---

    /// The console's "Now": the live sessions and the unloaded ones (closed while idle, or back
    /// after a restart) that aren't archived, the latest activity first.
    pub fn console(&self, cx: &App) -> Vec<ConsoleItem> {
        // A session takes the top when it comes into "Now" and stays where it is: opening it, a
        // turn starting don't move it. Live sessions not in the kept order yet (new ones, resumed
        // from the history) are on top, the latest first; then the kept order.
        let id_of =
            |session: &Entity<ClaudeSession>| session.read(cx).session_id().map(str::to_string);
        let mut items: Vec<ConsoleItem> = self
            .sessions
            .iter()
            .rev()
            .filter(|session| id_of(session).is_none_or(|id| !self.console_ids.contains(&id)))
            .cloned()
            .map(ConsoleItem::Live)
            .collect();
        for id in &self.console_ids {
            if self.archived.contains(id) {
                continue;
            }
            if let Some(session) = self.open_session(id, cx) {
                items.push(ConsoleItem::Live(session));
                continue;
            }
            let saved = self.placeholders.get(id).or_else(|| {
                self.saved
                    .as_deref()
                    .and_then(|saved| saved.iter().find(|saved| &saved.id == id))
            });
            if let Some(saved) = saved {
                items.push(ConsoleItem::Unloaded(saved.clone()));
            }
        }
        items
    }

    /// Whether the session finished its work out of sight and its chat wasn't shown since.
    pub fn is_unseen(&self, id: EntityId) -> bool {
        self.unseen.contains(&id)
    }

    /// The session's chat was shown: its "Done" is seen.
    pub fn mark_seen(&mut self, id: EntityId, cx: &mut Context<Self>) {
        if self.unseen.remove(&id) {
            cx.emit(ClaudeStoreEvent::Changed);
            cx.notify();
        }
    }

    /// Fixes the order of "Now" as shown: the live sessions on top that aren't kept yet take their
    /// places in it, so nothing moves later (their tab opening, a turn starting). Only once the
    /// saved sessions are read: before that the unloaded ones aren't shown and would sink.
    fn keep_order(&mut self, cx: &App) {
        if self.saved.is_none() {
            return;
        }
        let mut order: Vec<String> = Vec::new();
        for item in self.console(cx) {
            let id = match &item {
                ConsoleItem::Live(session) => session.read(cx).session_id().map(str::to_string),
                ConsoleItem::Unloaded(saved) => Some(saved.id.clone()),
            };
            if let Some(id) = id
                && !order.contains(&id)
            {
                order.push(id);
            }
        }
        for id in &self.console_ids {
            if !order.contains(id) {
                order.push(id.clone());
            }
        }
        self.console_ids = order;
    }

    /// When the session last worked: its last turn's end in this window, else when its transcript
    /// was last written.
    pub fn last_worked(&self, session: &Entity<ClaudeSession>, cx: &App) -> Option<SystemTime> {
        if let Some(at) = self.worked.get(&session.entity_id()) {
            return Some(*at);
        }
        let id = session.read(cx).session_id()?;
        self.placeholders
            .get(id)
            .or_else(|| {
                self.saved
                    .as_deref()
                    .and_then(|saved| saved.iter().find(|saved| saved.id == id))
            })
            .map(|saved| saved.modified)
    }

    /// Every saved session of the root (`~/.claude/projects/<root>`), the latest first, archived
    /// ones included; `None` until first read. Kept by the store and re-read in the background
    /// when sessions change ([`Self::refresh_saved`]).
    pub fn saved_sessions(&self) -> Option<&[SavedSession]> {
        self.saved.as_deref()
    }

    /// Reads the saved sessions again (the console opened, a session finished or was unloaded).
    pub fn refresh_saved(&mut self, cx: &mut Context<Self>) {
        if self.root.is_none() {
            self.saved = Some(Vec::new());
            self._saved = None;
            return;
        }
        let list = self.history(cx);
        self._saved = Some(cx.spawn(async move |this, cx| {
            let list = list.await;
            this.update(cx, |this, cx| {
                this.placeholders
                    .retain(|id, _| !list.iter().any(|saved| &saved.id == id));
                this.saved = Some(list);
                this._saved = None;
                cx.emit(ClaudeStoreEvent::Changed);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Whether the session (by its CLI id) is in the archive.
    pub fn is_archived(&self, id: &str) -> bool {
        self.archived.iter().any(|known| known == id)
    }

    /// Moves an unloaded or saved session to the archive (a live one is unloaded by the window
    /// first). Kept in `claude-console.json`.
    pub fn archive(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.is_archived(id) {
            self.archived.insert(0, id.to_string());
        }
        self.console_ids.retain(|known| known != id);
        self.placeholders.remove(id);
        self.persist_console(cx);
        cx.emit(ClaudeStoreEvent::Changed);
        cx.notify();
    }

    /// Brings a session back from the archive into "Now".
    pub fn unarchive(&mut self, id: &str, cx: &mut Context<Self>) {
        self.archived.retain(|known| known != id);
        if !self.console_ids.iter().any(|known| known == id) {
            self.console_ids.insert(0, id.to_string());
        }
        self.persist_console(cx);
        cx.emit(ClaudeStoreEvent::Changed);
        cx.notify();
    }

    /// A session's tab closed while it is idle: its process stops and it stays in "Now" unloaded
    /// (opening it resumes it). One without an id yet (nothing was said) just ends.
    pub fn unload(&mut self, session: &Entity<ClaudeSession>, cx: &mut Context<Self>) {
        let (id, title, cwd) = {
            let session = session.read(cx);
            (
                session.session_id().map(str::to_string),
                session.title().to_string(),
                session.model().info.cwd.clone(),
            )
        };
        // It keeps its place in "Now": closing the session takes it out of the order first.
        self.keep_order(cx);
        let place = id
            .as_ref()
            .and_then(|id| self.console_ids.iter().position(|known| known == id));
        self.close_session(session, cx);
        let Some(id) = id else {
            return;
        };
        let path = flux_claude::transcript::transcript_path(&cwd, &id).unwrap_or_default();
        self.placeholders.insert(
            id.clone(),
            SavedSession {
                id: id.clone(),
                title,
                prompt: None,
                branch: None,
                modified: SystemTime::now(),
                size: 0,
                path,
            },
        );
        self.archived.retain(|known| known != &id);
        self.console_ids.retain(|known| known != &id);
        let place = place.unwrap_or(0).min(self.console_ids.len());
        self.console_ids.insert(place, id);
        self.refresh_saved(cx);
        self.persist_console(cx);
        cx.emit(ClaudeStoreEvent::Changed);
        cx.notify();
    }

    /// The console's Close: the session leaves "Now" — a live one ends, its process stops — and
    /// shows in the history ("Recent", "This Week") by its time. Not archived.
    pub fn close_from_console(&mut self, target: &SessionRef, cx: &mut Context<Self>) {
        match target {
            SessionRef::Live(session) => {
                if let Some(id) = session.read(cx).session_id().map(str::to_string) {
                    self.placeholders.remove(&id);
                }
                self.close_session(session, cx);
            }
            SessionRef::Saved(saved) => {
                self.console_ids.retain(|known| known != &saved.id);
                self.placeholders.remove(&saved.id);
            }
        }
        self.refresh_saved(cx);
        self.persist_console(cx);
        cx.emit(ClaudeStoreEvent::Changed);
        cx.notify();
    }

    /// Whether closing the session's tab keeps it running: Claude works, waits for the user, or a
    /// background task of the session still runs.
    pub fn keeps_running(session: &ClaudeSession) -> bool {
        let model = session.model();
        model.is_working() || crate::claude_status::glance(model).background > 0
    }

    /// The root's console as it was: "Now" (unloaded until opened) and the archive.
    fn load_console(&mut self) {
        self.console_ids.clear();
        self.archived.clear();
        self.placeholders.clear();
        self.saved_console = None;
        let Some(root) = &self.root else {
            return;
        };
        let state = claude_history::load_console(root);
        self.console_ids = state.now.clone();
        self.archived = state.archived.clone();
        self.saved_console = Some(state);
    }

    /// Writes the root's console when it changed: "Now" (the sessions of this root that have an id)
    /// and the archive.
    fn persist_console(&mut self, cx: &App) {
        let Some(root) = self.root.clone() else {
            return;
        };
        if self.saved_console.is_none() {
            return;
        }
        let mut now: Vec<String> = Vec::new();
        for item in self.console(cx) {
            let id = match &item {
                ConsoleItem::Live(session) => {
                    let session = session.read(cx);
                    if session.model().info.cwd != root {
                        continue;
                    }
                    session.session_id().map(str::to_string)
                }
                ConsoleItem::Unloaded(saved) => Some(saved.id.clone()),
            };
            if let Some(id) = id
                && !now.contains(&id)
            {
                now.push(id);
            }
        }
        // Unloaded ones whose transcript isn't listed yet stay.
        for id in &self.console_ids {
            if !now.contains(id) && !self.archived.contains(id) {
                now.push(id.clone());
            }
        }
        let state = ConsoleState {
            now,
            archived: self.archived.clone(),
        };
        if self.saved_console.as_ref() == Some(&state) {
            return;
        }
        claude_history::save_console(&root, &state);
        self.saved_console = Some(state);
    }

    /// A session by its entity id (a notification's "Show").
    pub fn session(&self, id: EntityId) -> Option<&Entity<ClaudeSession>> {
        self.sessions
            .iter()
            .find(|session| session.entity_id() == id)
    }

    /// The sessions that work or wait for the user: closing the window asks about them.
    pub fn working_sessions(&self, cx: &App) -> Vec<Entity<ClaudeSession>> {
        self.sessions
            .iter()
            .filter(|session| session.read(cx).model().is_working())
            .cloned()
            .collect()
    }

    /// When the session last finished its work (since it was opened in this window).
    pub fn finished_at(&self, id: EntityId) -> Option<Instant> {
        self.finished.get(&id).copied()
    }

    /// While a session works, the window redraws each second: the clocks on its pill, its editor
    /// tab and in their tooltips go on (part 10.2).
    fn update_ticker(&mut self, cx: &mut Context<Self>) {
        let live = |store: &Self, cx: &App| {
            store
                .sessions
                .iter()
                .any(|session| crate::claude_status::glance(session.read(cx).model()).is_live())
        };
        if !live(self, cx) {
            self._ticker = None;
            return;
        }
        if self._ticker.is_some() {
            return;
        }
        self._ticker = Some(cx.spawn(async move |store, cx| {
            loop {
                cx.background_executor().timer(BADGE_TICK).await;
                let going = store.update(cx, |store, cx| {
                    cx.emit(ClaudeStoreEvent::Changed);
                    live(store, cx)
                });
                if !matches!(going, Ok(true)) {
                    break;
                }
            }
            store.update(cx, |store, _| store._ticker = None).ok();
        }));
    }

    /// The newest subscription limits any session reported.
    pub fn limits(&self) -> Option<&RateLimits> {
        self.limits.as_ref()
    }

    /// The overall activity of the sessions.
    pub fn activity(&self, cx: &App) -> Activity {
        let mut activity = if self.sessions.is_empty() {
            Activity::None
        } else {
            Activity::Idle
        };
        for session in &self.sessions {
            match session.read(cx).model().status {
                Status::WaitingForUser => return Activity::Waiting,
                Status::Working { .. } | Status::Starting => activity = Activity::Working,
                _ => {}
            }
        }
        activity
    }

    /// Looks for `claude` again (after installing, signing in, changing the path in the settings).
    pub fn check(&mut self, cx: &mut Context<Self>) {
        self.cli = CliState::Checking;
        cx.emit(ClaudeStoreEvent::Changed);
        cx.notify();
        let preferred = settings::claude(cx).path;
        let check = cx.background_spawn(async move { check_cli(preferred.as_deref()) });
        self._check = Some(cx.spawn(async move |this, cx| {
            let state = check.await;
            this.update(cx, |this, cx| this.set_cli(state, cx)).ok();
        }));
    }

    /// "Sign In" started `claude auth login` in a terminal: the sign-in is checked every few
    /// seconds until it is there (or for a few minutes).
    pub fn watch_sign_in(&mut self, cx: &mut Context<Self>) {
        let preferred = settings::claude(cx).path;
        self._sign_in = Some(cx.spawn(async move |this, cx| {
            let started = SystemTime::now();
            loop {
                cx.background_executor().timer(SIGN_IN_POLL).await;
                let preferred = preferred.clone();
                let state = cx
                    .background_spawn(async move { check_cli(preferred.as_deref()) })
                    .await;
                let ready = matches!(state, CliState::Ready { .. });
                let still_watched = this.update(cx, |this, cx| {
                    // Another check found it first, or the user changed the path meanwhile.
                    if ready || !this.is_ready() {
                        this.set_cli(state, cx);
                    }
                });
                let timed_out = started.elapsed().map_or(true, |spent| spent > SIGN_IN_WAIT);
                if ready || still_watched.is_err() || timed_out {
                    break;
                }
            }
        }));
    }

    fn set_cli(&mut self, state: CliState, cx: &mut Context<Self>) {
        if matches!(state, CliState::Ready { .. }) {
            self._sign_in = None;
        }
        if self.cli != state {
            self.cli = state;
            cx.emit(ClaudeStoreEvent::Changed);
            cx.notify();
        }
        self.restore(cx);
    }

    /// Starts a session in the project root (the home folder without a project) with the defaults
    /// of the settings; `None` while `claude` isn't ready.
    pub fn new_session(&mut self, cx: &mut Context<Self>) -> Option<Entity<ClaudeSession>> {
        self.new_session_in_mode(None, cx)
    }

    /// [`Self::new_session`] in `mode` rather than the settings' (part 10.3: a plugin's "Plan with
    /// Claude" starts in plan mode).
    pub fn new_session_in_mode(
        &mut self,
        mode: Option<flux_claude::PermissionMode>,
        cx: &mut Context<Self>,
    ) -> Option<Entity<ClaudeSession>> {
        let CliState::Ready { cli, .. } = &self.cli else {
            return None;
        };
        let cli = cli.clone();
        let mut options = self.launch_options(cx);
        if mode.is_some() {
            options.permission_mode = mode;
        }
        let session = cx.new(|cx| ClaudeSession::new(cli, options, cx));
        self.add(session.clone(), cx);
        Some(session)
    }

    /// How a session of this window starts: in the project root (the home folder without a
    /// project), with the defaults of the settings.
    fn launch_options(&self, cx: &App) -> LaunchOptions {
        let defaults = settings::claude(cx);
        let cwd = self
            .root
            .clone()
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/"));
        LaunchOptions {
            cwd,
            model: defaults.model.clone(),
            permission_mode: defaults
                .permission_mode
                .as_deref()
                .and_then(flux_claude::PermissionMode::from_wire),
            effort: defaults
                .effort
                .as_deref()
                .and_then(flux_claude::Effort::from_wire),
            extra_args: defaults.extra_args.clone(),
            ..LaunchOptions::default()
        }
    }

    fn add(&mut self, session: Entity<ClaudeSession>, cx: &mut Context<Self>) {
        self.track(session.clone(), cx);
        cx.emit(ClaudeStoreEvent::SessionAdded(session));
        cx.notify();
    }

    /// One of the window's sessions from now on.
    fn track(&mut self, session: Entity<ClaudeSession>, cx: &mut Context<Self>) {
        let subscription = cx.subscribe(&session, Self::on_session_event);
        self.subscriptions.push((session.clone(), subscription));
        self.sessions.push(session);
    }

    /// The project's saved sessions, the most recent first (the history popup), read in the
    /// background.
    pub fn history(&self, cx: &mut Context<Self>) -> Task<Vec<SavedSession>> {
        let Some(root) = self.root.clone() else {
            return Task::ready(Vec::new());
        };
        cx.background_spawn(async move {
            flux_claude::transcript::projects_dir()
                .map(|projects| flux_claude::transcript::list(&projects, &root))
                .unwrap_or_default()
        })
    }

    /// A saved session back in a tab (the history, Flux restarting): the open one when it is
    /// already open; `None` while `claude` isn't ready.
    pub fn resume(
        &mut self,
        saved: &SavedSession,
        cx: &mut Context<Self>,
    ) -> Option<Entity<ClaudeSession>> {
        if let Some(open) = self.open_session(&saved.id, cx) {
            return Some(open);
        }
        let session = self.resumed_session(saved, cx)?;
        self.add(session.clone(), cx);
        Some(session)
    }

    /// A saved session back in the editor's tabs, where a context keeps it (part 10.3): as the
    /// sessions a project reopens with — its process starts once its chat is shown. An open one
    /// stays where it is. False while `claude` isn't ready.
    pub(crate) fn reopen(
        &mut self,
        saved: &SavedSession,
        active: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.open_session(&saved.id, cx).is_some() {
            return true;
        }
        let Some(session) = self.resumed_session(saved, cx) else {
            return false;
        };
        self.track(session.clone(), cx);
        cx.emit(ClaudeStoreEvent::SessionRestored {
            session,
            place: SessionPlace::Editor,
            active,
        });
        cx.emit(ClaudeStoreEvent::Changed);
        cx.notify();
        true
    }

    /// The open session with this id.
    fn open_session(&self, id: &str, cx: &App) -> Option<Entity<ClaudeSession>> {
        self.sessions
            .iter()
            .find(|session| session.read(cx).session_id() == Some(id))
            .cloned()
    }

    /// A saved session as a new entity of the window, its title known before the transcript is
    /// read; not yet among the sessions.
    fn resumed_session(
        &self,
        saved: &SavedSession,
        cx: &mut Context<Self>,
    ) -> Option<Entity<ClaudeSession>> {
        let CliState::Ready { cli, .. } = &self.cli else {
            return None;
        };
        let cli = cli.clone();
        let mut options = self.launch_options(cx);
        options.resume = Some(saved.id.clone());
        let path = saved.path.clone();
        let title = saved.title.clone();
        Some(cx.new(|cx| {
            let mut session = ClaudeSession::resumed(cli, options, path, cx);
            session.set_title_hint(title);
            session
        }))
    }

    /// Brings back the sessions the root had open when it last closed, once `claude` is ready
    /// (Settings → Claude Code → "Reopen sessions with the project"). The transcripts are listed
    /// in the background; an id whose transcript is gone is dropped.
    fn restore(&mut self, cx: &mut Context<Self>) {
        if self.restore != Restore::Pending || !self.is_ready() {
            return;
        }
        let Some(root) = self.root.clone() else {
            return;
        };
        self.restore = Restore::Done;
        let open = claude_history::load_open(&root);
        self.saved_open = Some(open.clone());
        if !settings::claude(cx).restore_sessions || open.sessions.is_empty() {
            return;
        }
        self.restore = Restore::Running;
        let list = cx.background_spawn(async move {
            flux_claude::transcript::projects_dir()
                .map(|projects| flux_claude::transcript::list(&projects, &root))
                .unwrap_or_default()
        });
        self._restore = Some(cx.spawn(async move |this, cx| {
            let saved = list.await;
            this.update(cx, |this, cx| this.bring_back(open, saved, cx))
                .ok();
        }));
    }

    fn bring_back(&mut self, open: OpenSessions, saved: Vec<SavedSession>, cx: &mut Context<Self>) {
        self.restore = Restore::Done;
        self._restore = None;
        for (id, place) in &open.sessions {
            let Some(saved) = saved.iter().find(|saved| &saved.id == id) else {
                continue;
            };
            if self.open_session(id, cx).is_some() {
                continue;
            }
            let Some(session) = self.resumed_session(saved, cx) else {
                return;
            };
            self.track(session.clone(), cx);
            cx.emit(ClaudeStoreEvent::SessionRestored {
                session,
                place: *place,
                active: open.active.as_deref() == Some(id.as_str()),
            });
        }
        cx.emit(ClaudeStoreEvent::Changed);
        cx.notify();
    }

    /// The window's open sessions of the project changed (which, their order, where): saved for
    /// the next start — only after the saved ones came back, and only when something changed.
    pub fn persist(&mut self, open: OpenSessions) {
        let Some(root) = &self.root else {
            return;
        };
        if self.restore != Restore::Done || self.saved_open.as_ref() == Some(&open) {
            return;
        }
        claude_history::save_open(root, &open);
        self.saved_open = Some(open);
    }

    /// Ends a session: its process stops, its chat goes.
    pub fn close_session(&mut self, session: &Entity<ClaudeSession>, cx: &mut Context<Self>) {
        session.update(cx, |session, _| session.shutdown());
        self.sessions.retain(|known| known != session);
        self.subscriptions.retain(|(known, _)| known != session);
        self.busy.remove(&session.entity_id());
        self.finished.remove(&session.entity_id());
        self.unseen.remove(&session.entity_id());
        self.worked.remove(&session.entity_id());
        if let Some(id) = session.read(cx).session_id() {
            self.console_ids.retain(|known| known != id);
        }
        crate::system_notify::withdraw(session.entity_id());
        cx.emit(ClaudeStoreEvent::SessionRemoved(session.clone()));
        cx.emit(ClaudeStoreEvent::Changed);
        cx.notify();
    }

    fn on_session_event(
        &mut self,
        session: Entity<ClaudeSession>,
        event: &SessionEvent,
        cx: &mut Context<Self>,
    ) {
        let id = session.entity_id();
        self.keep_order(cx);
        match event {
            SessionEvent::PendingAdded(request) => {
                crate::system_notify::session_waits(&session, cx);
                cx.emit(ClaudeStoreEvent::Attention {
                    session,
                    request: request.clone(),
                })
            }
            SessionEvent::PendingRemoved(request) => {
                crate::system_notify::withdraw(id);
                cx.emit(ClaudeStoreEvent::Resolved {
                    session,
                    request: request.clone(),
                })
            }
            SessionEvent::Status => {
                if session.read(cx).model().is_working() {
                    self.busy.insert(id);
                }
                cx.emit(ClaudeStoreEvent::Changed);
            }
            SessionEvent::Idle => {
                if self.busy.remove(&id) {
                    let is_error = last_turn_failed(&session, cx);
                    self.finished.insert(id, Instant::now());
                    self.worked.insert(id, SystemTime::now());
                    // Seen right away when its chat is in sight (the window says so).
                    self.unseen.insert(id);
                    crate::system_notify::session_finished(&session, is_error, cx);
                    cx.emit(ClaudeStoreEvent::Finished { session, is_error });
                    self.refresh_saved(cx);
                }
                cx.emit(ClaudeStoreEvent::Changed);
            }
            SessionEvent::FilesChanged(paths) => {
                cx.emit(ClaudeStoreEvent::FilesChanged(paths.clone()))
            }
            SessionEvent::Limits => {
                self.limits = session.read(cx).model().limits.clone();
                cx.emit(ClaudeStoreEvent::Changed);
            }
            SessionEvent::Exited => {
                self.busy.remove(&id);
                let model = session.read(cx).model();
                if let Status::Exited { code } = model.status
                    && code != Some(0)
                {
                    let stderr = model
                        .entries
                        .iter()
                        .rev()
                        .find_map(|entry| match &entry.kind {
                            EntryKind::Notice(Notice::Exited { stderr, .. }) => {
                                Some(stderr.clone())
                            }
                            _ => None,
                        })
                        .unwrap_or_default();
                    crate::system_notify::session_finished(&session, true, cx);
                    cx.emit(ClaudeStoreEvent::Failed {
                        session,
                        code,
                        stderr,
                    });
                }
                cx.emit(ClaudeStoreEvent::Changed);
            }
            SessionEvent::Title => cx.emit(ClaudeStoreEvent::Changed),
            SessionEvent::ToolCall {
                request,
                tool,
                input,
            } => cx.emit(ClaudeStoreEvent::ToolCall {
                session,
                request: request.clone(),
                tool: tool.clone(),
                input: input.clone(),
            }),
            SessionEvent::EditHook { request, path } => cx.emit(ClaudeStoreEvent::EditHook {
                session,
                request: request.clone(),
                path: path.clone(),
            }),
            SessionEvent::Changed | SessionEvent::TurnFinished { .. } => {}
        }
        self.update_ticker(cx);
        // A new session got its id, a turn started: "Now" as it is now (not on every streamed
        // change).
        if matches!(
            event,
            SessionEvent::Status | SessionEvent::Idle | SessionEvent::Title
        ) {
            self.persist_console(cx);
        }
        cx.notify();
    }
}

/// Whether the session's last turn ended with an error.
fn last_turn_failed(session: &Entity<ClaudeSession>, cx: &App) -> bool {
    session
        .read(cx)
        .model()
        .entries
        .iter()
        .rev()
        .find_map(|entry| match &entry.kind {
            EntryKind::TurnEnd(summary) => Some(summary.is_error),
            _ => None,
        })
        .unwrap_or(false)
}

/// Finds `claude`, asks its version and the sign-in (on a background thread: each is a process).
pub(crate) fn check_cli(preferred: Option<&Path>) -> CliState {
    // Scenarios can't uninstall `claude`: `FLUX_SCENARIO_CLAUDE_MISSING` shows the window without
    // it.
    #[cfg(feature = "scenario")]
    if std::env::var_os("FLUX_SCENARIO_CLAUDE_MISSING").is_some() {
        return CliState::Missing;
    }
    let Some(cli) = Cli::locate(preferred) else {
        return CliState::Missing;
    };
    let version = match cli.version() {
        Ok(version) => version,
        Err(err) => return CliState::Failed(err),
    };
    match cli.auth_status() {
        Ok(status) if status.logged_in => CliState::Ready {
            cli,
            version,
            account: status.email,
        },
        Ok(_) => CliState::SignedOut {
            cli,
            version: Some(version),
        },
        Err(err) => CliState::Failed(err),
    }
}

/// What a question for the user is, for its notification's title.
pub fn attention_title(kind: &PendingKind) -> &'static str {
    match kind {
        PendingKind::Tool | PendingKind::Edit(_) => tr("Claude needs your permission"),
        PendingKind::Questions(_) => tr("Claude asks a question"),
        PendingKind::Plan { .. } => tr("Claude's plan is ready"),
    }
}

/// A mention of a file and its lines as the CLI reads it: `@src/main.rs`, `@src/main.rs#L12`,
/// `@src/main.rs#L12-20` (1-based, inclusive).
pub fn mention(path: &str, lines: Option<(usize, usize)>) -> String {
    match lines {
        None => format!("@{path}"),
        Some((first, last)) if first == last => format!("@{path}#L{first}"),
        Some((first, last)) => format!("@{path}#L{first}-{last}"),
    }
}

/// How full a limit window is, for its color in the status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitLevel {
    Normal,
    /// 80% and more.
    Warning,
    /// 95% and more.
    Critical,
}

pub fn limit_level(utilization: f32) -> LimitLevel {
    if utilization >= 0.95 {
        LimitLevel::Critical
    } else if utilization >= 0.8 {
        LimitLevel::Warning
    } else {
        LimitLevel::Normal
    }
}

/// A limit window's share in percent, as the CLI's `/usage` rounds it.
pub fn percent(utilization: f32) -> u32 {
    (utilization.clamp(0., 1.) * 100.).round() as u32
}

/// When a limit window resets, relative to `now`: "in 3 h 12 min", "in 4 days".
pub fn resets_in(at: SystemTime, now: SystemTime) -> String {
    let left = at.duration_since(now).unwrap_or_default().as_secs();
    let minutes = left.div_ceil(60);
    if minutes < 60 {
        return trf("in {0} min", &[&minutes.max(1)]);
    }
    let hours = minutes / 60;
    if hours < 48 {
        let rest = minutes % 60;
        return if rest == 0 {
            trf("in {0} h", &[&hours])
        } else {
            trf("in {0} h {1} min", &[&hours, &rest])
        };
    }
    trf("in {0} days", &[&hours.div_ceil(24)])
}

/// Shortens in the middle: "start…end".
pub fn shorten(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(count - tail).collect();
    format!("{start}…{end}")
}

/// A path for a shell command line: as it is when it has nothing special, else in single quotes.
pub fn shell_quote(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+=:@%,".contains(c));
    if plain {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

/// The command that continues a session in the terminal: `claude --resume <id>` (or a new
/// session when it has no id yet).
pub fn resume_command(cli: &Cli, session_id: Option<&str>) -> String {
    let program = shell_quote(&cli.path.to_string_lossy());
    match session_id {
        Some(id) => format!("{program} --resume {}", shell_quote(id)),
        None => program,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_titles_shorten_in_the_middle() {
        assert_eq!(shorten("fix tests", 24), "fix tests");
        let long = "Fix the failing lexer tests in the parser crate";
        let short = shorten(long, 24);
        assert_eq!(short.chars().count(), 24);
        assert!(short.starts_with("Fix the fail"));
        assert!(short.ends_with("arser crate"));
    }

    #[test]
    fn mentions_name_the_lines() {
        assert_eq!(mention("src/main.rs", None), "@src/main.rs");
        assert_eq!(mention("src/main.rs", Some((12, 12))), "@src/main.rs#L12");
        assert_eq!(
            mention("src/main.rs", Some((12, 20))),
            "@src/main.rs#L12-20"
        );
    }

    #[test]
    fn limit_levels_and_percents() {
        assert_eq!(limit_level(0.12), LimitLevel::Normal);
        assert_eq!(limit_level(0.8), LimitLevel::Warning);
        assert_eq!(limit_level(0.95), LimitLevel::Critical);
        assert_eq!(percent(0.494), 49);
        assert_eq!(percent(1.3), 100);
    }

    #[test]
    fn resets_read_in_minutes_hours_days() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let at = |seconds| now + Duration::from_secs(seconds);
        assert_eq!(resets_in(at(30), now), "in 1 min");
        assert_eq!(resets_in(at(42 * 60), now), "in 42 min");
        assert_eq!(resets_in(at(3 * 3600), now), "in 3 h");
        assert_eq!(resets_in(at(3 * 3600 + 12 * 60), now), "in 3 h 12 min");
        assert_eq!(resets_in(at(4 * 86400 - 3600), now), "in 4 days");
        assert_eq!(resets_in(now - Duration::from_secs(5), now), "in 1 min");
    }

    #[test]
    fn shell_quoting() {
        assert_eq!(
            shell_quote("/opt/homebrew/bin/claude"),
            "/opt/homebrew/bin/claude"
        );
        assert_eq!(
            shell_quote("/Users/me/My Tools/claude"),
            "'/Users/me/My Tools/claude'"
        );
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        let cli = Cli {
            path: PathBuf::from("/opt/homebrew/bin/claude"),
        };
        assert_eq!(
            resume_command(&cli, Some("3615c7de-b3bc")),
            "/opt/homebrew/bin/claude --resume 3615c7de-b3bc"
        );
        assert_eq!(resume_command(&cli, None), "/opt/homebrew/bin/claude");
    }
}
