//! The hub of Claude for remote work (API 0.6, the bot's part 1): every Claude session of every
//! window — and the unloaded ones of the pults — under the CLI's session id, for the plugins with
//! the `claude-remote` permission ([`flux_plugin::claude_remote`]). It carries out their calls
//! ([`handle`]) with the sessions' own methods, the ones the chat and the pult use, and tells the
//! plugins that watch what happens in the sessions ([`remote::broadcast`]).
//!
//! The hub is one for the app (a global entity). It follows every window's [`ClaudeStore`] (made
//! and gone with its window) and every session in it. What a plugin sees of a session
//! ([`RemoteSession`]) is posted again only when it changed — compared with what was posted last,
//! at the session's and the store's events, never per frame. Claude's text and the groups of its
//! steps go out as they grow, at most [`TEXT_PACE`] apart per session; requests, answers, turn ends
//! and failures at once. Nothing polls: with nothing happening the hub does nothing.
//!
//! Requests read as the chat's cards (their titles, «allow …» rows, the edit's diff) and answers go
//! the cards' way ([`crate::claude_cards`]); a turn's rollback is the chat's without its dialog
//! ([`crate::claude_turns`]): the plugin asks its user first. A session a plugin starts or resumes
//! gets its chat tab out of sight, in its project's window — opened in the background when no window
//! has the project. The computer's power, sleep and screen lock are [`crate::power`]'s.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant, SystemTime};

use flux_claude::{
    Answer, Cli, EntryId, EntryKind, HostRequest, ImageAttachment, LaunchOptions, LimitStatus,
    Notice, Pending, PendingKind, PermissionMode, Process, RateLimits, SavedSession, Session,
    Status, ToolEntry, UserInput,
};
use flux_plugin::claude_remote::{
    self as remote, AwakeMode, Limits, NewSession, PermissionRequest, RegionState, RemoteAnswer,
    RemoteCall, RemoteEvent, RemoteMode, RemoteProject, RemoteQuestion, RemoteReply, RemoteRequest,
    RemoteSession, RemoteSteps, RequestKind, RollbackCheck, SessionState, TranscriptItem, TurnFile,
    TurnSummary,
};
use gpui::{
    App, AppContext, AsyncApp, Context, Entity, EntityId, Global, Subscription, Task, WeakEntity,
};

use crate::claude::{ClaudeStore, ClaudeStoreEvent, CliState, ConsoleItem};
use crate::claude_session::{Attachment, ClaudeSession, SessionDraft, SessionEvent};
use crate::claude_transcript::Item;
use crate::plugins::{PluginStore, PluginStoreEvent};
use crate::region_guard::{self, Verdict};
use crate::workspace::Workspace;

/// Claude's text and steps go out at most this often per session while they grow: about four a
/// second.
const TEXT_PACE: Duration = Duration::from_millis(250);
/// The limits the CLI was asked for without a session are good this long.
const LIMITS_FRESH: Duration = Duration::from_secs(60);
/// How long the probe of the limits waits for the CLI, and how often it asks again while the CLI
/// hasn't fetched them yet (it answers `rate_limits: null` right after it starts).
const PROBE_WAIT: Duration = Duration::from_secs(20);
const PROBE_ATTEMPTS: usize = 4;
const PROBE_RETRY: Duration = Duration::from_secs(3);
/// An edit's diff in a request, at most: a phone shows the start, the chat has the rest.
const REQUEST_DIFF_LINES: usize = 40;
/// A picture larger than this isn't sent (as the chat's field: the CLI scales the rest).
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

struct GlobalHub(Entity<ClaudeHub>);

impl Global for GlobalHub {}

pub(crate) struct ClaudeHub {
    stores: Vec<FollowedStore>,
    /// The sessions followed, by entity.
    sessions: HashMap<EntityId, Watched>,
    /// What was last posted of each session, by its id, with the store it is in: a change posts it
    /// again, a session no store lists any more is gone.
    posted: HashMap<String, (EntityId, RemoteSession)>,
    /// The limits last posted.
    limits: Option<Limits>,
    /// The limits the probe got, and when.
    probed: Option<(Instant, Limits)>,
    probe: Option<Task<()>>,
    probe_waiters: Vec<Sender<RemoteReply>>,
    /// New sessions waiting for their window's `claude` check (a window just opened).
    pending_new: Vec<(EntityId, NewSession, Sender<RemoteReply>)>,
    /// The country check as last posted.
    region: Option<RegionState>,
    _region: Option<Subscription>,
}

struct FollowedStore {
    store: WeakEntity<ClaudeStore>,
    _subscriptions: [Subscription; 2],
}

/// The numbers of Claude's messages and step groups for the plugins: one counter for the app, so a
/// session resumed in a new entity (whose entries count from 1 again) never repeats one.
static NEXT_NUMBER: AtomicU64 = AtomicU64::new(1);

/// A session the hub follows.
struct Watched {
    session: WeakEntity<ClaudeSession>,
    store: EntityId,
    _subscription: Subscription,
    /// Claude's message last posted (or there when the session came: not news).
    text: Option<PostedText>,
    /// The group of steps last posted, and the entry it starts at.
    steps: Option<(EntryId, RemoteSteps)>,
    /// What the session had before the hub saw it isn't news: set once its transcript is read.
    baseline: bool,
    /// A post of the text and steps waits for the pace.
    pace: Option<Task<()>>,
    streamed: Option<Instant>,
    /// The session's request as last made: an edit's diff isn't worked out again.
    request: Option<RemoteRequest>,
    /// The plugins' numbers of the entries posted (messages, the first steps of groups).
    numbers: HashMap<EntryId, u64>,
}

impl Watched {
    /// The plugins' number of an entry: the same each time.
    fn number(&mut self, entry: EntryId) -> u64 {
        *self
            .numbers
            .entry(entry)
            .or_insert_with(|| NEXT_NUMBER.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, PartialEq)]
struct PostedText {
    message: EntryId,
    len: usize,
    done: bool,
}

/// A session a call names.
enum Found {
    Live(Entity<ClaudeSession>),
    Unloaded(SavedSession, Entity<ClaudeStore>),
}

/// Starts the hub: it follows the windows' Claude stores, the country check, the power and the
/// screen lock.
pub(crate) fn init(cx: &mut App) {
    let hub = cx.new(ClaudeHub::new);
    cx.set_global(GlobalHub(hub.clone()));
    cx.observe_new({
        let hub = hub.clone();
        move |_: &mut ClaudeStore, _, cx: &mut Context<ClaudeStore>| {
            let store = cx.entity();
            hub.update(cx, |hub, cx| hub.follow(store, cx));
        }
    })
    .detach();
    // A plugin that stops (turned off, removed, failed) no longer keeps the computer awake.
    cx.observe_new(
        move |_: &mut PluginStore, _, cx: &mut Context<PluginStore>| {
            let plugins = cx.entity();
            hub.update(cx, |_, cx| {
                cx.subscribe(&plugins, |_, _, event: &PluginStoreEvent, cx| {
                    if let PluginStoreEvent::PluginStopped(plugin) = event {
                        crate::power::set_awake(plugin, AwakeMode::Off, cx);
                    }
                })
                .detach()
            });
        },
    )
    .detach();
    crate::power::init(cx);
}

fn global(cx: &App) -> Option<Entity<ClaudeHub>> {
    cx.try_global::<GlobalHub>().map(|global| global.0.clone())
}

/// Carries out a plugin's call (the host has checked the permission) and replies.
pub(crate) fn handle(plugin: &str, call: RemoteCall, reply: Sender<RemoteReply>, cx: &mut App) {
    #[cfg(feature = "scenario")]
    if crate::claude_hub_fake::enabled() {
        return crate::claude_hub_fake::handle(plugin, call, reply, cx);
    }
    match global(cx) {
        Some(hub) => hub.update(cx, |hub, cx| hub.call(plugin, call, reply, cx)),
        None => {
            let _ = reply.send(RemoteReply::Failed("Flux isn't ready".into()));
        }
    }
}

/// The user chose a plugin's action at a session in the pult: only that plugin hears it.
pub(crate) fn run_action(plugin: &str, session: String, action: String) {
    post_to(plugin, RemoteEvent::Action { session, action });
}

/// Posts an event to the plugins that watch (the power's too, [`crate::power`]).
pub(crate) fn post(event: RemoteEvent) {
    #[cfg(feature = "scenario")]
    log_event(None, &event);
    remote::broadcast(event);
}

fn post_to(plugin: &str, event: RemoteEvent) {
    #[cfg(feature = "scenario")]
    log_event(Some(plugin), &event);
    remote::send_to(plugin, event);
}

impl ClaudeHub {
    fn new(cx: &mut Context<Self>) -> Self {
        let region = region_guard::global(cx).map(|guard| {
            cx.subscribe(&guard, |hub, _, _: &region_guard::RegionEvent, cx| {
                hub.post_region(cx)
            })
        });
        Self {
            stores: Vec::new(),
            sessions: HashMap::new(),
            posted: HashMap::new(),
            limits: None,
            probed: None,
            probe: None,
            probe_waiters: Vec::new(),
            pending_new: Vec::new(),
            region: None,
            _region: region,
        }
    }

    // --- Following the windows ---

    /// A window's store, made just now (nothing in it yet).
    fn follow(&mut self, store: Entity<ClaudeStore>, cx: &mut Context<Self>) {
        let id = store.entity_id();
        let subscriptions = [
            cx.subscribe(&store, Self::on_store_event),
            // The window closed: its sessions end with it and leave the plugins' list.
            cx.observe_release(&store, move |hub, _, cx| hub.store_gone(id, cx)),
        ];
        self.stores.push(FollowedStore {
            store: store.downgrade(),
            _subscriptions: subscriptions,
        });
    }

    fn store_gone(&mut self, id: EntityId, cx: &mut Context<Self>) {
        self.stores
            .retain(|followed| followed.store.entity_id() != id);
        self.sessions.retain(|_, watched| watched.store != id);
        let gone: Vec<String> = self
            .posted
            .iter()
            .filter(|(_, (store, _))| *store == id)
            .map(|(session, _)| session.clone())
            .collect();
        for session in gone {
            self.posted.remove(&session);
            post(RemoteEvent::SessionGone(session));
        }
        for (store, _, reply) in self.pending_new.extract_if(.., |(store, ..)| *store == id) {
            let _ = store;
            let _ = reply.send(RemoteReply::Failed("The window closed".into()));
        }
        cx.notify();
    }

    fn live_stores(&self) -> Vec<Entity<ClaudeStore>> {
        self.stores
            .iter()
            .filter_map(|followed| followed.store.upgrade())
            .collect()
    }

    fn on_store_event(
        &mut self,
        store: Entity<ClaudeStore>,
        event: &ClaudeStoreEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            ClaudeStoreEvent::SessionAdded(session)
            | ClaudeStoreEvent::SessionRestored { session, .. } => {
                self.watch(session, store.entity_id(), cx)
            }
            ClaudeStoreEvent::SessionRemoved(session) => {
                self.sessions.remove(&session.entity_id());
            }
            ClaudeStoreEvent::Changed => self.start_waiting(&store, cx),
            _ => {}
        }
        self.sync_store(&store, cx);
    }

    /// Follows a session of a window: what it had so far isn't news.
    fn watch(&mut self, session: &Entity<ClaudeSession>, store: EntityId, cx: &mut Context<Self>) {
        if self.sessions.contains_key(&session.entity_id()) {
            return;
        }
        let subscription = cx.subscribe(session, Self::on_session_event);
        self.sessions.insert(
            session.entity_id(),
            Watched {
                session: session.downgrade(),
                store,
                _subscription: subscription,
                text: None,
                steps: None,
                baseline: true,
                pace: None,
                streamed: None,
                request: None,
                numbers: HashMap::new(),
            },
        );
        self.flush(session.entity_id(), cx);
    }

    fn on_session_event(
        &mut self,
        session: Entity<ClaudeSession>,
        event: &SessionEvent,
        cx: &mut Context<Self>,
    ) {
        let key = session.entity_id();
        let Some(id) = session.read(cx).cli_id().map(str::to_string) else {
            return;
        };
        match event {
            // Streamed text, a step's progress: paced.
            SessionEvent::Changed => return self.stream(key, cx),
            SessionEvent::PendingAdded(request) => {
                // What Claude wrote before it asks goes first.
                self.flush(key, cx);
                if let Some(remote) = self.request_of(&session, request, cx) {
                    post(RemoteEvent::Request {
                        session: id,
                        request: remote,
                    });
                }
            }
            SessionEvent::PendingRemoved(request) => post(RemoteEvent::Resolved {
                session: id,
                request: request.clone(),
            }),
            SessionEvent::TurnFinished { is_error } => {
                self.flush(key, cx);
                let model = session.read(cx).model();
                if let Some(summary) = turn_summary(model, *is_error) {
                    post(RemoteEvent::TurnEnded {
                        session: id,
                        summary,
                    });
                }
            }
            SessionEvent::Limits => self.post_limits(cx),
            SessionEvent::Exited => {
                self.flush(key, cx);
                if let Some(error) = exit_error(session.read(cx).model()) {
                    post(RemoteEvent::Failed { session: id, error });
                }
            }
            _ => {}
        }
        if let Some(store) = self.store_of(key) {
            self.sync_store(&store, cx);
        }
    }

    fn store_of(&self, session: EntityId) -> Option<Entity<ClaudeStore>> {
        let store = self.sessions.get(&session)?.store;
        self.live_stores()
            .into_iter()
            .find(|known| known.entity_id() == store)
    }

    // --- Sessions ---

    /// Posts what changed of the store's sessions; those it no longer lists are gone.
    fn sync_store(&mut self, store: &Entity<ClaudeStore>, cx: &mut Context<Self>) {
        let store_id = store.entity_id();
        let list = self.store_sessions(store, cx);
        let listed: HashSet<String> = list.iter().map(|session| session.id.clone()).collect();
        for session in list {
            match self.posted.get_mut(&session.id) {
                Some((owner, known)) if *known == session => *owner = store_id,
                _ => {
                    self.posted
                        .insert(session.id.clone(), (store_id, session.clone()));
                    post(RemoteEvent::Session(session));
                }
            }
        }
        let gone: Vec<String> = self
            .posted
            .iter()
            .filter(|(id, (owner, _))| *owner == store_id && !listed.contains(*id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in gone {
            self.posted.remove(&id);
            post(RemoteEvent::SessionGone(id));
        }
    }

    /// The sessions of a window's pult: the live ones and the unloaded ones of «Сейчас».
    fn store_sessions(
        &mut self,
        store: &Entity<ClaudeStore>,
        cx: &mut Context<Self>,
    ) -> Vec<RemoteSession> {
        let items = store.read(cx).console(cx);
        let window = store
            .read(cx)
            .plugins()
            .and_then(|plugins| plugins.upgrade())
            .map(|plugins| plugins.read(cx).window_id());
        let root = store.read(cx).root().map(Path::to_path_buf);
        let mut list: Vec<RemoteSession> = Vec::new();
        for item in items {
            let session = match item {
                ConsoleItem::Live(session) => self.live_session(&session, store, window, cx),
                ConsoleItem::Unloaded(saved) => Some(unloaded_session(&saved, root.as_deref())),
            };
            if let Some(session) = session
                && !list.iter().any(|known| known.id == session.id)
            {
                list.push(session);
            }
        }
        list
    }

    fn live_session(
        &mut self,
        session: &Entity<ClaudeSession>,
        store: &Entity<ClaudeStore>,
        window: Option<u64>,
        cx: &mut Context<Self>,
    ) -> Option<RemoteSession> {
        let request = {
            let model = session.read(cx).model();
            model.pending.first().map(|pending| pending.id.clone())
        };
        let request = request.and_then(|request| self.request_of(session, &request, cx));
        let store = store.read(cx);
        let owner = session.read(cx);
        let model = owner.model();
        let updated = store
            .last_worked(session, cx)
            .or_else(|| last_message_at(model))
            .map_or(0, seconds);
        Some(RemoteSession {
            id: owner.cli_id()?.to_string(),
            project: model.info.cwd.to_string_lossy().into_owned(),
            title: model.info.title.clone().unwrap_or_default(),
            state: match model.status {
                Status::Starting => SessionState::Starting,
                Status::Working { .. } => SessionState::Working,
                Status::WaitingForUser => SessionState::Waiting,
                Status::Idle => SessionState::Idle,
                Status::Exited { .. } => SessionState::Unloaded,
            },
            activity: crate::claude_status::activity(model),
            mode: remote_mode(model.info.permission_mode),
            window,
            updated,
            request,
            unseen: store.is_unseen(session.entity_id()),
        })
    }

    /// Every session of every window, the unloaded ones of the pults too.
    fn all_sessions(&mut self, cx: &mut Context<Self>) -> Vec<RemoteSession> {
        let mut all: Vec<RemoteSession> = Vec::new();
        for store in self.live_stores() {
            for session in self.store_sessions(&store, cx) {
                if !all.iter().any(|known| known.id == session.id) {
                    all.push(session);
                }
            }
        }
        all
    }

    /// The session a call names: live in a window, or unloaded in a pult.
    fn find(&self, id: &str, cx: &App) -> Option<Found> {
        let stores = self.live_stores();
        for store in &stores {
            if let Some(session) = store.read(cx).live_session(id, cx) {
                return Some(Found::Live(session));
            }
        }
        for store in stores {
            let saved = store
                .read(cx)
                .console(cx)
                .into_iter()
                .find_map(|item| match item {
                    ConsoleItem::Unloaded(saved) if saved.id == id => Some(saved),
                    _ => None,
                });
            if let Some(saved) = saved {
                return Some(Found::Unloaded(saved, store));
            }
        }
        None
    }

    /// A live session: an unloaded one is resumed first, in its window, its chat out of sight.
    fn live_or_resume(
        &self,
        id: &str,
        cx: &mut Context<Self>,
    ) -> Result<Entity<ClaudeSession>, String> {
        match self.find(id, cx) {
            Some(Found::Live(session)) => Ok(session),
            Some(Found::Unloaded(saved, store)) => store
                .update(cx, |store, cx| store.remote_session(Some(&saved), None, cx))
                .ok_or_else(|| "Claude Code isn't ready".to_string()),
            None => Err(no_session()),
        }
    }

    fn live(&self, id: &str, cx: &App) -> Result<Entity<ClaudeSession>, String> {
        match self.find(id, cx) {
            Some(Found::Live(session)) => Ok(session),
            Some(Found::Unloaded(..)) => Err("The session is unloaded".into()),
            None => Err(no_session()),
        }
    }

    /// The session's request as a plugin sees it, made once per request.
    fn request_of(
        &mut self,
        session: &Entity<ClaudeSession>,
        request: &str,
        cx: &App,
    ) -> Option<RemoteRequest> {
        let watched = self.sessions.get_mut(&session.entity_id());
        if let Some(known) = watched
            .as_ref()
            .and_then(|watched| watched.request.as_ref())
            .filter(|known| known.id == request)
        {
            return Some(known.clone());
        }
        let model = session.read(cx).model();
        let made = remote_request(model.pending(request)?, &model.info.cwd);
        if let Some(watched) = watched {
            watched.request = Some(made.clone());
        }
        Some(made)
    }

    // --- Text and steps as they grow ---

    /// The conversation changed: the text and the steps go out now, or at the pace.
    fn stream(&mut self, key: EntityId, cx: &mut Context<Self>) {
        // Nobody hears the text as it is written: nothing to follow — a plugin that starts
        // watching gets the whole text with the next change.
        if !remote::watching() {
            return;
        }
        let Some(watched) = self.sessions.get_mut(&key) else {
            return;
        };
        if watched.pace.is_some() {
            return;
        }
        let wait = watched
            .streamed
            .map_or(Duration::ZERO, |at| TEXT_PACE.saturating_sub(at.elapsed()));
        if wait.is_zero() {
            return self.flush(key, cx);
        }
        watched.pace = Some(cx.spawn(async move |hub, cx| {
            cx.background_executor().timer(wait).await;
            hub.update(cx, |hub, cx| hub.flush(key, cx)).ok();
        }));
    }

    /// Posts Claude's message and the group of steps as they are now, when they changed.
    fn flush(&mut self, key: EntityId, cx: &mut Context<Self>) {
        let Some(watched) = self.sessions.get_mut(&key) else {
            return;
        };
        watched.pace = None;
        let Some(session) = watched.session.upgrade() else {
            return;
        };
        let session = session.read(cx);
        // A resumed session's transcript is still being read: its history isn't news.
        if session.is_loading() {
            return;
        }
        let Some(id) = session.cli_id().map(str::to_string) else {
            return;
        };
        watched.streamed = Some(Instant::now());
        let model = session.model();
        let text = last_text(model);
        let steps = group_steps(model, session, None);
        if std::mem::take(&mut watched.baseline) {
            watched.text = text.map(|(message, text, _)| PostedText {
                message,
                len: text.len(),
                done: true,
            });
            watched.steps = steps.map(|(start, steps)| {
                let group = watched.number(start);
                (
                    start,
                    RemoteSteps {
                        group,
                        done: true,
                        ..steps
                    },
                )
            });
            return;
        }
        let mut events = Vec::new();
        if let Some((message, text, streaming)) = text {
            let now = PostedText {
                message,
                len: text.len(),
                done: !streaming,
            };
            match watched.text.clone() {
                Some(posted) if posted.message == message => {
                    if posted != now {
                        let number = watched.number(message);
                        events.push(text_event(&id, number, text, !streaming));
                    }
                }
                posted => {
                    // The message before it ended: whole, done.
                    if let Some(before) = posted.filter(|posted| !posted.done)
                        && let Some(text) = text_of(model, before.message)
                    {
                        let number = watched.number(before.message);
                        events.push(text_event(&id, number, text, true));
                    }
                    let number = watched.number(message);
                    events.push(text_event(&id, number, text, !streaming));
                }
            }
            watched.text = Some(now);
        }
        let steps = steps.map(|(start, steps)| {
            let group = watched.number(start);
            (start, RemoteSteps { group, ..steps })
        });
        match (watched.steps.clone(), steps) {
            (Some((start, posted)), Some((now_start, now))) if start == now_start => {
                if posted.summary != now.summary || posted.lines != now.lines {
                    events.push(steps_event(&id, now.clone()));
                    watched.steps = Some((start, now));
                }
            }
            (posted, now) => {
                // The group before ended: as it is in the end (its thinking done), done.
                if let Some((start, posted)) = posted.filter(|(_, posted)| !posted.done) {
                    let last = group_steps(model, session, Some(start)).map(|(_, steps)| steps);
                    let done = RemoteSteps {
                        group: posted.group,
                        done: true,
                        ..last.unwrap_or(posted)
                    };
                    events.push(steps_event(&id, done.clone()));
                    watched.steps = Some((start, done));
                }
                if let Some((start, now)) = now {
                    events.push(steps_event(&id, now.clone()));
                    watched.steps = Some((start, now));
                }
            }
        }
        for event in events {
            post(event);
        }
    }

    // --- Limits and the country check ---

    fn post_limits(&mut self, cx: &App) {
        let Some(limits) = self.store_limits(cx) else {
            return;
        };
        if self.limits.as_ref() != Some(&limits) {
            self.limits = Some(limits.clone());
            post(RemoteEvent::Limits(limits));
        }
    }

    /// The newest limits a session of any window reported.
    fn store_limits(&self, cx: &App) -> Option<Limits> {
        self.live_stores()
            .iter()
            .find_map(|store| store.read(cx).limits().map(remote_limits))
    }

    fn post_region(&mut self, cx: &mut Context<Self>) {
        let state = region_state(cx);
        if self.region.as_ref() != Some(&state) {
            self.region = Some(state.clone());
            post(RemoteEvent::Region(state));
        }
    }

    /// The limits: a session's, else the probe's of the last minute, else the CLI is asked.
    fn limits(&mut self, reply: Sender<RemoteReply>, cx: &mut Context<Self>) {
        if let Some(limits) = self.store_limits(cx) {
            let _ = reply.send(RemoteReply::Limits(limits));
            return;
        }
        if let Some((at, limits)) = &self.probed
            && at.elapsed() < LIMITS_FRESH
        {
            let _ = reply.send(RemoteReply::Limits(limits.clone()));
            return;
        }
        self.probe_waiters.push(reply);
        if self.probe.is_some() {
            return;
        }
        let cli = self
            .live_stores()
            .iter()
            .find_map(|store| match store.read(cx).cli() {
                CliState::Ready { cli, .. } => Some(cli.clone()),
                _ => None,
            });
        let Some(cli) = cli else {
            return self.probed(Err("Claude Code isn't ready".into()), cx);
        };
        // Nothing goes to Anthropic unless the country check allows it.
        self.probe = Some(Task::ready(()));
        region_guard::when_allowed(cx, move |outcome, cx| {
            let Some(hub) = global(cx) else {
                return;
            };
            hub.update(cx, |hub, cx| match outcome {
                Ok(()) => {
                    hub.probe = Some(cx.spawn(async move |hub, cx| {
                        let limits = probe_limits(cli, cx).await;
                        hub.update(cx, |hub, cx| hub.probed(limits, cx)).ok();
                    }))
                }
                Err(refusal) => hub.probed(Err(refusal.message()), cx),
            });
        });
    }

    fn probed(&mut self, limits: Result<Limits, String>, cx: &mut Context<Self>) {
        self.probe = None;
        if let Ok(limits) = &limits {
            self.probed = Some((Instant::now(), limits.clone()));
        }
        for reply in self.probe_waiters.drain(..) {
            let _ = reply.send(match &limits {
                Ok(limits) => RemoteReply::Limits(limits.clone()),
                Err(error) => RemoteReply::Failed(error.clone()),
            });
        }
        cx.notify();
    }

    // --- Calls ---

    fn call(
        &mut self,
        plugin: &str,
        call: RemoteCall,
        reply: Sender<RemoteReply>,
        cx: &mut Context<Self>,
    ) {
        let answer = match call {
            RemoteCall::Watch(_) => Ok(RemoteReply::Done),
            RemoteCall::Sessions => Ok(RemoteReply::Sessions(self.all_sessions(cx))),
            RemoteCall::Transcript { session, count } => {
                return self.transcript(&session, count, reply, cx);
            }
            RemoteCall::Send {
                session,
                text,
                images,
            } => self.send(&session, text, &images, cx),
            RemoteCall::Answer {
                session,
                request,
                answer,
            } => self.answer(&session, &request, answer, cx),
            RemoteCall::Interrupt { session } => self.live(&session, cx).map(|session| {
                session.update(cx, |session, cx| session.interrupt(cx));
                RemoteReply::Done
            }),
            RemoteCall::SetMode { session, mode } => self.set_mode(&session, mode, cx),
            RemoteCall::NewSession(new) => return self.new_session(new, reply, cx),
            RemoteCall::TurnDiff { session, turn } => self.turn_diff(&session, turn, cx),
            RemoteCall::CheckRollback { session, turn } => self.check_rollback(&session, turn, cx),
            RemoteCall::Rollback { session, turn } => {
                return self.rollback(&session, turn, reply, cx);
            }
            RemoteCall::Limits => return self.limits(reply, cx),
            RemoteCall::Region { again } => {
                if again {
                    region_guard::check_again(cx);
                }
                Ok(RemoteReply::Region(region_state(cx)))
            }
            RemoteCall::Projects => Ok(RemoteReply::Projects(projects(cx))),
            RemoteCall::SetAwake(mode) => {
                crate::power::set_awake(plugin, mode, cx);
                Ok(RemoteReply::Done)
            }
            RemoteCall::System => Ok(RemoteReply::System(crate::power::state())),
        };
        let _ = reply.send(answer.unwrap_or_else(RemoteReply::Failed));
    }

    fn transcript(
        &mut self,
        id: &str,
        count: u32,
        reply: Sender<RemoteReply>,
        cx: &mut Context<Self>,
    ) {
        let count = count as usize;
        match self.find(id, cx) {
            Some(Found::Live(session)) => {
                let items = transcript_items(&session.read(cx).model().entries, count);
                let _ = reply.send(RemoteReply::Transcript(items));
            }
            // Read from its transcript file, in the background.
            Some(Found::Unloaded(saved, store)) => {
                let cwd = store
                    .read(cx)
                    .root()
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                let read = cx.background_spawn(async move {
                    flux_claude::transcript::load(&saved.path, cwd)
                        .map(|model| transcript_items(&model.entries, count))
                        .map_err(|err| err.to_string())
                });
                cx.spawn(async move |_, _| {
                    let _ = reply.send(match read.await {
                        Ok(items) => RemoteReply::Transcript(items),
                        Err(error) => RemoteReply::Failed(error),
                    });
                })
                .detach();
            }
            None => {
                let _ = reply.send(RemoteReply::Failed(no_session()));
            }
        }
    }

    fn send(
        &mut self,
        id: &str,
        text: String,
        images: &[String],
        cx: &mut Context<Self>,
    ) -> Result<RemoteReply, String> {
        let images = images
            .iter()
            .map(|path| read_image(Path::new(path)))
            .collect::<Result<Vec<_>, _>>()?;
        let session = self.live_or_resume(id, cx)?;
        session.update(cx, |session, cx| {
            session.send(
                UserInput {
                    text,
                    images,
                    priority: None,
                },
                cx,
            )
        });
        Ok(RemoteReply::Done)
    }

    fn answer(
        &mut self,
        id: &str,
        request: &str,
        answer: RemoteAnswer,
        cx: &mut Context<Self>,
    ) -> Result<RemoteReply, String> {
        let session = self.live(id, cx)?;
        let pending = session
            .read(cx)
            .model()
            .pending(request)
            .cloned()
            .ok_or_else(|| "The request is gone".to_string())?;
        let answer = flux_answer(&pending, answer)?;
        session.update(cx, |session, cx| session.answer(request, answer, cx));
        Ok(RemoteReply::Done)
    }

    fn set_mode(
        &mut self,
        id: &str,
        mode: RemoteMode,
        cx: &mut Context<Self>,
    ) -> Result<RemoteReply, String> {
        let mode =
            permission_mode(mode).ok_or_else(|| "This mode isn't set remotely".to_string())?;
        let session = self.live_or_resume(id, cx)?;
        session.update(cx, |session, cx| session.set_permission_mode(mode, cx));
        Ok(RemoteReply::Done)
    }

    /// A new session in the project's window — opened in the background when none has it.
    fn new_session(&mut self, new: NewSession, reply: Sender<RemoteReply>, cx: &mut Context<Self>) {
        // As the windows name their projects (`/tmp` is `/private/tmp` on macOS).
        let root = flux_platform::path_style::canonicalize(Path::new(&new.project))
            .unwrap_or_else(|_| PathBuf::from(&new.project));
        if !root.is_dir() {
            let _ = reply.send(RemoteReply::Failed("No such project folder".into()));
            return;
        }
        let window = crate::workspace::project_window(&root, cx)
            .or_else(|| crate::workspace::open_project_window_in_background(root, cx));
        let store = window.and_then(|window| {
            window
                .read(cx)
                .ok()
                .map(|workspace: &Workspace| workspace.claude.clone())
        });
        let Some(store) = store else {
            let _ = reply.send(RemoteReply::Failed("Couldn't open the project".into()));
            return;
        };
        // A window just opened looks for `claude` first.
        if matches!(store.read(cx).cli(), CliState::Checking) {
            self.pending_new.push((store.entity_id(), new, reply));
            return;
        }
        start_new(&store, new, reply, cx);
    }

    /// The window's `claude` check answered: the new sessions that waited for it start.
    fn start_waiting(&mut self, store: &Entity<ClaudeStore>, cx: &mut Context<Self>) {
        if matches!(store.read(cx).cli(), CliState::Checking) {
            return;
        }
        let id = store.entity_id();
        let waiting: Vec<_> = self
            .pending_new
            .extract_if(.., |(known, ..)| *known == id)
            .collect();
        for (_, new, reply) in waiting {
            start_new(store, new, reply, cx);
        }
    }

    fn turn_diff(
        &mut self,
        id: &str,
        turn: Option<u32>,
        cx: &mut Context<Self>,
    ) -> Result<RemoteReply, String> {
        let session = self.live(id, cx)?;
        let model = session.read(cx).model();
        let end = turn_end(model, turn)?;
        let cwd = &model.info.cwd;
        let mut diff = String::new();
        for file in model.turn_restores(end) {
            let after = file
                .after
                .clone()
                .or_else(|| std::fs::read_to_string(&file.path).ok())
                .unwrap_or_default();
            diff.push_str(&crate::commit_panel::unified_diff(
                &relative(&file.path, cwd),
                file.before.as_deref().unwrap_or(""),
                &after,
            ));
        }
        Ok(RemoteReply::Text(diff))
    }

    fn check_rollback(
        &mut self,
        id: &str,
        turn: Option<u32>,
        cx: &mut Context<Self>,
    ) -> Result<RemoteReply, String> {
        let session = self.live(id, cx)?;
        let model = session.read(cx).model();
        let end = turn_end(model, turn)?;
        let cwd = &model.info.cwd;
        let files = crate::claude_turns::to_restore(model.turn_restores(end));
        Ok(RemoteReply::Rollback(RollbackCheck {
            files: files.iter().map(|file| relative(&file.path, cwd)).collect(),
            changed_since: files
                .iter()
                .filter(|file| crate::claude_turns::changed_since(file))
                .map(|file| relative(&file.path, cwd))
                .collect(),
            created: files
                .iter()
                .filter(|file| file.before.is_none())
                .map(|file| relative(&file.path, cwd))
                .collect(),
        }))
    }

    /// Rolls the turn back without Flux's dialog (the plugin asked its user): the chat's rollback.
    fn rollback(
        &mut self,
        id: &str,
        turn: Option<u32>,
        reply: Sender<RemoteReply>,
        cx: &mut Context<Self>,
    ) {
        let files = self.live(id, cx).and_then(|session| {
            let model = session.read(cx).model();
            let end = turn_end(model, turn)?;
            Ok((
                session.clone(),
                crate::claude_turns::to_restore(model.turn_restores(end)),
            ))
        });
        let (session, files) = match files {
            Ok(found) => found,
            Err(error) => {
                let _ = reply.send(RemoteReply::Failed(error));
                return;
            }
        };
        if files.is_empty() {
            let _ = reply.send(RemoteReply::Done);
            return;
        }
        let jobs = crate::claude_turns::restore_jobs(&files);
        let paths: Vec<PathBuf> = files.into_iter().map(|file| file.path).collect();
        cx.spawn(async move |_, cx| {
            let errors = cx
                .background_spawn(async move { crate::commit_panel::restore_originals(&jobs) })
                .await;
            session
                .update(cx, |session, cx| session.files_restored(paths, cx))
                .ok();
            let _ = reply.send(if errors.is_empty() {
                RemoteReply::Done
            } else {
                RemoteReply::Failed(errors.join("\n"))
            });
        })
        .detach();
    }
}

/// Starts a plugin's new session in the store's window, its chat out of sight, and replies its id.
fn start_new(
    store: &Entity<ClaudeStore>,
    new: NewSession,
    reply: Sender<RemoteReply>,
    cx: &mut App,
) {
    let images = match new
        .images
        .iter()
        .map(|path| read_image(Path::new(path)))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(images) => images,
        Err(error) => {
            let _ = reply.send(RemoteReply::Failed(error));
            return;
        }
    };
    let mode = permission_mode(new.mode);
    let Some(session) = store.update(cx, |store, cx| store.remote_session(None, mode, cx)) else {
        let _ = reply.send(RemoteReply::Failed("Claude Code isn't ready".into()));
        return;
    };
    let attachments: Vec<Attachment> = new
        .attachments
        .into_iter()
        .map(|(label, markdown)| Attachment { label, markdown })
        .collect();
    session.update(cx, |session, cx| {
        if let Some(title) = new.title.filter(|title| !title.trim().is_empty()) {
            session.set_title_hint(title.clone());
            session.rename(title, cx);
        }
        match new.prompt.filter(|prompt| !prompt.trim().is_empty()) {
            Some(prompt) => {
                let text = crate::claude_session::with_attachments(&attachments, &prompt);
                session.send(
                    UserInput {
                        text,
                        images,
                        priority: None,
                    },
                    cx,
                );
            }
            // Nothing to send: the chips wait in the chat's field.
            None => session.set_draft(SessionDraft {
                prompt: None,
                attachments,
            }),
        }
    });
    let id = session.read(cx).cli_id().unwrap_or_default().to_string();
    let _ = reply.send(RemoteReply::Session(id));
}

// --- Conversions ---

fn no_session() -> String {
    "No such session".into()
}

fn seconds(at: SystemTime) -> i64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// A path for a plugin: relative to the project when inside it.
fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn remote_mode(mode: PermissionMode) -> RemoteMode {
    match mode {
        PermissionMode::Default => RemoteMode::Ask,
        PermissionMode::AcceptEdits => RemoteMode::AcceptEdits,
        PermissionMode::Plan => RemoteMode::Plan,
        PermissionMode::BypassPermissions => RemoteMode::Bypass,
        PermissionMode::DontAsk | PermissionMode::Auto => RemoteMode::Other,
    }
}

/// The CLI's mode of a plugin's; `Other` isn't set remotely.
fn permission_mode(mode: RemoteMode) -> Option<PermissionMode> {
    match mode {
        RemoteMode::Ask => Some(PermissionMode::Default),
        RemoteMode::AcceptEdits => Some(PermissionMode::AcceptEdits),
        RemoteMode::Plan => Some(PermissionMode::Plan),
        RemoteMode::Bypass => Some(PermissionMode::BypassPermissions),
        RemoteMode::Other => None,
    }
}

/// An unloaded session of a pult: what its transcript's listing knows.
fn unloaded_session(saved: &SavedSession, root: Option<&Path>) -> RemoteSession {
    RemoteSession {
        id: saved.id.clone(),
        project: root
            .map(|root| root.to_string_lossy().into_owned())
            .unwrap_or_default(),
        title: saved.title.clone(),
        state: SessionState::Unloaded,
        activity: String::new(),
        mode: RemoteMode::Ask,
        window: None,
        updated: seconds(saved.modified),
        request: None,
        unseen: false,
    }
}

/// When the user last wrote in the session.
fn last_message_at(model: &Session) -> Option<SystemTime> {
    model
        .entries
        .iter()
        .rev()
        .find_map(|entry| match &entry.kind {
            EntryKind::User(user) => user.at,
            _ => None,
        })
}

/// A question of the session as a plugin sees it: as its card shows it.
pub(crate) fn remote_request(pending: &Pending, cwd: &Path) -> RemoteRequest {
    let kind = match &pending.kind {
        PendingKind::Questions(questions) => RequestKind::Questions(
            questions
                .iter()
                .map(|question| RemoteQuestion {
                    question: question.question.clone(),
                    header: question.header.clone(),
                    options: question
                        .options
                        .iter()
                        .map(|option| (option.label.clone(), option.description.clone()))
                        .collect(),
                    multi_select: question.multi_select,
                })
                .collect(),
        ),
        PendingKind::Plan { plan, .. } => RequestKind::Plan(plan.clone()),
        PendingKind::Tool | PendingKind::Edit(_) => {
            let edit = matches!(pending.kind, PendingKind::Edit(_));
            let (subject, diff) = match &pending.kind {
                PendingKind::Edit(proposal) => (
                    Some(relative(&proposal.path, cwd)),
                    proposal.proposed.as_ref().ok().map(|proposed| {
                        short_diff(
                            &crate::commit_panel::unified_diff(
                                &relative(&proposal.path, cwd),
                                proposal.original.as_deref().unwrap_or(""),
                                proposed,
                            ),
                            REQUEST_DIFF_LINES,
                        )
                    }),
                ),
                _ => (crate::claude_cards::tool_details(pending, cwd).0, None),
            };
            let choices = if pending.suppress_always_allow {
                Vec::new()
            } else {
                pending
                    .suggestions
                    .iter()
                    .enumerate()
                    .filter_map(|(index, suggestion)| {
                        // The card's label marks code with backticks; a plugin's button is
                        // plain text.
                        crate::claude_cards::suggestion_label(suggestion, cwd, edit)
                            .map(|label| (index.to_string(), label.replace('`', "")))
                    })
                    .collect()
            };
            RequestKind::Permission(PermissionRequest {
                title: crate::claude_cards::request_title(pending),
                tool: pending.tool.clone(),
                subject,
                diff,
                choices,
            })
        }
    };
    RemoteRequest {
        id: pending.id.clone(),
        kind,
    }
}

/// A unified diff cut after `max` lines (with a line that says how many more).
fn short_diff(diff: &str, max: usize) -> String {
    let lines: Vec<&str> = diff.lines().collect();
    if lines.len() <= max {
        return diff.to_string();
    }
    let mut short = lines[..max].join("\n");
    short.push_str(&format!("\n… {} more lines\n", lines.len() - max));
    short
}

/// A plugin's answer as the card would give it.
pub(crate) fn flux_answer(pending: &Pending, answer: RemoteAnswer) -> Result<Answer, String> {
    let permission = matches!(pending.kind, PendingKind::Tool | PendingKind::Edit(_));
    match (&pending.kind, answer) {
        (_, RemoteAnswer::Allow { choice }) if permission => {
            let remember = match choice {
                None => Vec::new(),
                Some(choice) => {
                    let suggestion = choice
                        .parse::<usize>()
                        .ok()
                        .filter(|_| !pending.suppress_always_allow)
                        .and_then(|index| pending.suggestions.get(index))
                        .ok_or_else(|| "No such choice".to_string())?;
                    vec![suggestion.clone()]
                }
            };
            Ok(Answer::Allow { remember })
        }
        (
            PendingKind::Tool | PendingKind::Edit(_) | PendingKind::Questions(_),
            RemoteAnswer::Deny { message },
        ) => Ok(crate::claude_cards::deny(&message)),
        (PendingKind::Questions(questions), RemoteAnswer::Answers(answers)) => Ok(Answer::Answers(
            questions
                .iter()
                .enumerate()
                .map(|(index, question)| {
                    let parts: Vec<&str> = answers
                        .get(index)
                        .into_iter()
                        .flatten()
                        .map(|part| part.trim())
                        .filter(|part| !part.is_empty())
                        .collect();
                    (question.question.clone(), parts.join(", "))
                })
                .collect(),
        )),
        (PendingKind::Plan { .. }, RemoteAnswer::ApprovePlan { accept_edits }) => {
            Ok(Answer::ApprovePlan { accept_edits })
        }
        (PendingKind::Plan { .. }, RemoteAnswer::KeepPlanning(feedback)) => {
            Ok(crate::claude_cards::keep_planning(&feedback))
        }
        _ => Err("The answer doesn't fit the request".into()),
    }
}

/// The last turn's end (a `TurnEnd` entry), or the `turn`th one (from 1).
fn turn_end(model: &Session, turn: Option<u32>) -> Result<EntryId, String> {
    let ends: Vec<EntryId> = model
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, EntryKind::TurnEnd(_)))
        .map(|entry| entry.id)
        .collect();
    match turn {
        None => ends.last(),
        Some(turn) => (turn as usize)
            .checked_sub(1)
            .and_then(|index| ends.get(index)),
    }
    .copied()
    .ok_or_else(|| "No such turn".to_string())
}

/// What the turn that just ended did.
pub(crate) fn turn_summary(model: &Session, is_error: bool) -> Option<TurnSummary> {
    let entries = &model.entries;
    let end = entries
        .iter()
        .rposition(|entry| matches!(entry.kind, EntryKind::TurnEnd(_)))?;
    let EntryKind::TurnEnd(summary) = &entries[end].kind else {
        return None;
    };
    let start = entries[..end]
        .iter()
        .rposition(|entry| matches!(entry.kind, EntryKind::TurnEnd(_)))
        .map_or(0, |index| index + 1);
    let turn = entries
        .iter()
        .filter(|entry| matches!(entry.kind, EntryKind::TurnEnd(_)))
        .count() as u32;
    let cwd = &model.info.cwd;
    let text = entries[start..end]
        .iter()
        .rev()
        .find_map(|entry| match &entry.kind {
            EntryKind::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    Some(TurnSummary {
        turn,
        files: model
            .turn_changes(entries[end].id)
            .into_iter()
            .map(|file| TurnFile {
                path: relative(&file.path, cwd),
                added: file.added as u32,
                removed: file.removed as u32,
                created: file.created,
            })
            .collect(),
        failed: is_error || summary.is_error,
        text,
        seconds: summary.duration.as_secs() as u32,
    })
}

/// Why the process stopped, when it failed: its last lines, as the chat's error card shows them.
fn exit_error(model: &Session) -> Option<String> {
    let Status::Exited { code } = model.status else {
        return None;
    };
    if code == Some(0) {
        return None;
    }
    let stderr = model
        .entries
        .iter()
        .rev()
        .find_map(|entry| match &entry.kind {
            EntryKind::Notice(Notice::Exited { stderr, .. }) => Some(stderr.join("\n")),
            _ => None,
        })
        .filter(|stderr| !stderr.trim().is_empty());
    Some(stderr.unwrap_or_else(|| match code {
        Some(code) => format!("Claude stopped (exit code {code})"),
        None => "Claude stopped".to_string(),
    }))
}

/// Claude's newest message: its entry, text, and whether it is still being written.
fn last_text(model: &Session) -> Option<(EntryId, &str, bool)> {
    model
        .entries
        .iter()
        .rev()
        .find_map(|entry| match &entry.kind {
            EntryKind::Text { text, streaming } => Some((entry.id, text.as_str(), *streaming)),
            _ => None,
        })
}

fn text_of(model: &Session, message: EntryId) -> Option<&str> {
    model.entries.iter().find_map(|entry| match &entry.kind {
        EntryKind::Text { text, .. } if entry.id == message => Some(text.as_str()),
        _ => None,
    })
}

fn text_event(session: &str, message: u64, text: &str, done: bool) -> RemoteEvent {
    RemoteEvent::Text {
        session: session.to_string(),
        message,
        text: text.to_string(),
        done,
    }
}

fn steps_event(session: &str, steps: RemoteSteps) -> RemoteEvent {
    RemoteEvent::Steps {
        session: session.to_string(),
        steps,
    }
}

/// A group of steps as the chat groups them — the one the conversation ends with, or the one that
/// starts at the entry `at` — with the entry it starts at (a group needs a tool among its steps:
/// thinking alone is the session's activity).
fn group_steps(
    model: &Session,
    session: &ClaudeSession,
    at: Option<EntryId>,
) -> Option<(EntryId, RemoteSteps)> {
    let entries = &model.entries;
    let tools = session.plugin_tools();
    let owner = |entry: &flux_claude::Entry| match &entry.kind {
        EntryKind::Tool(tool) => tools.of_step(&tool.name).map(|tool| tool.plugin.clone()),
        _ => None,
    };
    let items = crate::claude_transcript::items(entries, &owner);
    let span = |item: &Item| match item {
        Item::Group { start, end } => Some((*start, *end)),
        Item::Entry(index) if crate::claude_transcript::is_minor(&entries[*index].kind) => {
            Some((*index, *index + 1))
        }
        Item::Entry(_) => None,
    };
    let (start, end) = match at {
        None => span(items.last()?)?,
        Some(at) => items
            .iter()
            .filter_map(span)
            .find(|(start, _)| entries[*start].id == at)?,
    };
    let group: Vec<&flux_claude::Entry> = entries[start..end].iter().collect();
    let lines: Vec<String> = group
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::Tool(tool) => Some(step_line(tool, &model.info.cwd, &model.tasks)),
            _ => None,
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some((
        entries[start].id,
        RemoteSteps {
            group: 0,
            summary: crate::claude_transcript::group_summary(&group),
            lines,
            done: false,
        },
    ))
}

/// A step as the chat's row says it: «Read src/config.rs».
fn step_line(tool: &ToolEntry, root: &Path, tasks: &[flux_claude::TaskItem]) -> String {
    let summary = crate::claude_transcript::tool_summary(tool, root, tasks);
    let mut line = summary.title.to_string();
    if !summary.detail.is_empty() {
        line.push(' ');
        line.push_str(&summary.detail);
    }
    if let Some(meta) = summary.meta {
        line.push_str(" — ");
        line.push_str(&meta);
    }
    line
}

/// The last `count` messages of the conversation, the user's and Claude's, as Markdown.
fn transcript_items(entries: &[flux_claude::Entry], count: usize) -> Vec<TranscriptItem> {
    let mut items: Vec<TranscriptItem> = entries
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::User(user) if !user.cancelled => Some(TranscriptItem {
                user: true,
                text: crate::claude_transcript::bubble_text(&user.text).to_string(),
            }),
            EntryKind::Text { text, .. } if !text.trim().is_empty() => Some(TranscriptItem {
                user: false,
                text: text.clone(),
            }),
            _ => None,
        })
        .collect();
    let skip = items.len().saturating_sub(count);
    items.drain(..skip);
    items
}

/// A picture a plugin sends: the formats the chat's field takes.
fn read_image(path: &Path) -> Result<ImageAttachment, String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let media_type = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return Err("Only PNG, JPEG, GIF and WebP pictures can be sent".into()),
    };
    let data = std::fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
    if data.len() > MAX_IMAGE_BYTES {
        return Err("The picture is larger than 10 MB".into());
    }
    Ok(ImageAttachment {
        media_type: media_type.to_string(),
        data,
    })
}

pub(crate) fn remote_limits(limits: &RateLimits) -> Limits {
    let window = |window: &Option<flux_claude::LimitWindow>| {
        window.map(|window| {
            (
                crate::claude::percent(window.utilization).min(100) as u8,
                seconds(window.resets_at),
            )
        })
    };
    Limits {
        five_hour: window(&limits.five_hour),
        seven_day: window(&limits.seven_day),
        rejected: limits.status == LimitStatus::Rejected,
    }
}

fn region_state(cx: &App) -> RegionState {
    let verdict = region_guard::verdict(cx);
    match verdict.refusal() {
        Some(refusal) => RegionState::Blocked {
            country: refusal.country.clone().unwrap_or_default(),
            message: refusal.message(),
        },
        None if matches!(verdict, Verdict::Checking) => RegionState::Checking,
        None => RegionState::Allowed,
    }
}

/// The projects open in windows, then the recent ones.
fn projects(cx: &App) -> Vec<RemoteProject> {
    let open: Vec<PathBuf> = cx
        .windows()
        .into_iter()
        .filter_map(|window| window.downcast::<Workspace>())
        .filter_map(|window| {
            window
                .read(cx)
                .ok()
                .and_then(|workspace| workspace.root().map(Path::to_path_buf))
        })
        .collect();
    let project = |path: &Path, open: bool| RemoteProject {
        path: path.to_string_lossy().into_owned(),
        name: path
            .file_name()
            .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy())
            .into_owned(),
        open,
    };
    let canonical =
        |path: &Path| flux_platform::path_style::canonicalize(path).unwrap_or(path.to_path_buf());
    let mut list: Vec<RemoteProject> = open.iter().map(|path| project(path, true)).collect();
    let mut known: Vec<PathBuf> = open.iter().map(|path| canonical(path)).collect();
    for path in crate::recent::load() {
        let path = canonical(&path);
        if !known
            .iter()
            .any(|known| flux_platform::same_path(known, &path))
        {
            list.push(project(&path, false));
            known.push(path);
        }
    }
    list
}

/// Asks the CLI for the limits without a session: a short process in host mode — `initialize`,
/// then `get_usage` (again while it says it has none yet) — and no model turn.
async fn probe_limits(cli: Cli, cx: &mut AsyncApp) -> Result<Limits, String> {
    let cwd = flux_platform::paths::home().unwrap_or_else(std::env::temp_dir);
    let options = LaunchOptions {
        cwd,
        ..LaunchOptions::default()
    };
    let (process, _events) = Process::spawn(&cli, &options).map_err(|err| err.to_string())?;
    let ask = async {
        process
            .request(&HostRequest::Initialize(serde_json::json!({})))
            .await
            .map_err(|_| "Claude Code stopped".to_string())??;
        for attempt in 0..PROBE_ATTEMPTS {
            if attempt > 0 {
                cx.background_executor().timer(PROBE_RETRY).await;
            }
            let response = process
                .request(&HostRequest::GetUsage)
                .await
                .map_err(|_| "Claude Code stopped".to_string())??;
            if let Some(limits) = RateLimits::from_usage(&response) {
                return Ok(remote_limits(&limits));
            }
        }
        Err("Claude Code doesn't know the limits yet".to_string())
    };
    let timeout = cx.background_executor().timer(PROBE_WAIT);
    let result = futures::select_biased! {
        result = futures::FutureExt::fuse(Box::pin(ask)) => result,
        _ = futures::FutureExt::fuse(timeout) => Err("Claude Code didn't answer".to_string()),
    };
    process.close();
    result
}

// --- Scenarios ---

/// `FLUX_HUB_LOG=<file>`: every event the hub posts, a line each after the milliseconds since the
/// app started (scenarios check the flows and the pace).
#[cfg(feature = "scenario")]
fn log_event(plugin: Option<&str>, event: &RemoteEvent) {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let Some(path) = std::env::var_os("FLUX_HUB_LOG") else {
        return;
    };
    let ms = START.get_or_init(Instant::now).elapsed().as_millis();
    let line = match plugin {
        Some(plugin) => format!("{ms}\tTO {plugin} {event:?}\n"),
        None => format!("{ms}\t{event:?}\n"),
    };
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

/// A scenario's `hub:` step (`scenario.rs`): the call as a plugin would make it; the reply comes
/// back on the channel.
#[cfg(feature = "scenario")]
pub(crate) fn scenario_call(
    call: RemoteCall,
    cx: &mut App,
) -> std::sync::mpsc::Receiver<RemoteReply> {
    let (reply, replies) = std::sync::mpsc::channel();
    handle("scenario", call, reply, cx);
    replies
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_claude::session::{Question, QuestionOption};
    use serde_json::json;

    fn pending(tool: &str, kind: PendingKind, suggestions: Vec<serde_json::Value>) -> Pending {
        Pending {
            id: "r1".into(),
            tool_use_id: "t1".into(),
            tool: tool.into(),
            display_name: tool.into(),
            description: None,
            input: json!({ "command": "git push", "description": "Push" }),
            kind,
            suggestions,
            blocked_path: None,
            reason: None,
            default_to_no: false,
            suppress_always_allow: false,
            from_agent: false,
        }
    }

    #[test]
    fn a_permission_reads_as_its_card() {
        let rule = json!({
            "type": "addRules",
            "behavior": "allow",
            "destination": "localSettings",
            "rules": [{ "toolName": "Bash", "ruleContent": "git push:*" }],
        });
        let request = remote_request(
            &pending("Bash", PendingKind::Tool, vec![rule.clone()]),
            Path::new("/p"),
        );
        let RequestKind::Permission(permission) = request.kind else {
            panic!("a permission");
        };
        assert_eq!(permission.title, "Run a command?");
        assert_eq!(permission.subject.as_deref(), Some("git push"));
        assert_eq!(permission.choices.len(), 1);
        assert_eq!(permission.choices[0].0, "0");
        assert!(permission.choices[0].1.contains("git push:*"));
        // The choice goes back as the suggestion to remember.
        let answer = flux_answer(
            &pending("Bash", PendingKind::Tool, vec![rule.clone()]),
            RemoteAnswer::Allow {
                choice: Some("0".into()),
            },
        );
        assert_eq!(
            answer,
            Ok(Answer::Allow {
                remember: vec![rule]
            })
        );
    }

    #[test]
    fn answers_go_the_cards_way() {
        let tool = pending("Bash", PendingKind::Tool, Vec::new());
        assert!(
            flux_answer(
                &tool,
                RemoteAnswer::Allow {
                    choice: Some("3".into())
                }
            )
            .is_err()
        );
        // No words: Flux's own, and the turn stops — as the card's Deny.
        assert_eq!(
            flux_answer(
                &tool,
                RemoteAnswer::Deny {
                    message: String::new()
                }
            ),
            Ok(crate::claude_cards::deny(""))
        );
        let questions = PendingKind::Questions(vec![Question {
            question: "Which?".into(),
            header: "Pick".into(),
            options: vec![
                QuestionOption {
                    label: "A".into(),
                    description: String::new(),
                    preview: None,
                },
                QuestionOption {
                    label: "B".into(),
                    description: String::new(),
                    preview: None,
                },
            ],
            multi_select: true,
        }]);
        let asked = pending("AskUserQuestion", questions, Vec::new());
        assert_eq!(
            flux_answer(
                &asked,
                RemoteAnswer::Answers(vec![vec!["A".into(), "B".into()]])
            ),
            Ok(Answer::Answers(vec![("Which?".into(), "A, B".into())]))
        );
        let plan = pending(
            "ExitPlanMode",
            PendingKind::Plan {
                plan: "# Plan".into(),
                path: None,
            },
            Vec::new(),
        );
        assert_eq!(
            flux_answer(&plan, RemoteAnswer::ApprovePlan { accept_edits: true }),
            Ok(Answer::ApprovePlan { accept_edits: true })
        );
        assert!(flux_answer(&plan, RemoteAnswer::Allow { choice: None }).is_err());
        assert_eq!(
            remote_request(&plan, Path::new("/p")).kind,
            RequestKind::Plan("# Plan".into())
        );
    }

    #[test]
    fn modes_both_ways() {
        for mode in [
            RemoteMode::Ask,
            RemoteMode::AcceptEdits,
            RemoteMode::Plan,
            RemoteMode::Bypass,
        ] {
            assert_eq!(remote_mode(permission_mode(mode).unwrap()), mode);
        }
        assert_eq!(permission_mode(RemoteMode::Other), None);
        assert_eq!(remote_mode(PermissionMode::DontAsk), RemoteMode::Other);
    }

    #[test]
    fn long_diffs_are_cut() {
        let diff: String = (0..50).map(|line| format!("+{line}\n")).collect();
        let short = short_diff(&diff, 10);
        assert_eq!(short.lines().count(), 11);
        assert!(short.ends_with("… 40 more lines\n"));
        assert_eq!(short_diff("+a\n", 10), "+a\n");
    }

    #[test]
    fn limits_in_percent() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_900_000_000);
        let limits = RateLimits {
            status: LimitStatus::Rejected,
            five_hour: Some(flux_claude::LimitWindow {
                utilization: 0.494,
                resets_at: at,
            }),
            seven_day: None,
        };
        assert_eq!(
            remote_limits(&limits),
            Limits {
                five_hour: Some((49, 1_900_000_000)),
                seven_day: None,
                rejected: true,
            }
        );
    }
}
