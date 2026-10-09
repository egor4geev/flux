//! A running plugin. Each instance has its own thread with its wasmtime store: the window posts
//! calls to it ([`Instance::run_command`], [`Instance::send_event`]) and never waits for them; the
//! plugin's calls that need the window come back as [`PluginMessage`]s to the window's channel.
//! A call that needs an answer from the window (the text of a document) blocks the plugin's
//! thread until the window replies — never the other way round, so a plugin can't freeze Flux.
//!
//! A trap (a panic in the plugin), a call longer than [`CALL_LIMIT`] or more memory than
//! [`MEMORY_LIMIT`] stops the instance: [`MessageKind::Stopped`] says why, and the window offers
//! to restart it. Components are compiled by Cranelift once and cached in
//! `paths::cache_dir()`.
//!
//! The sandbox is WASI 0.2: the project root (as the manifest's `project` permission says) and the
//! plugin's data folder, each at its real absolute path; stdout and stderr go to the plugin's log.
//! A plugin with access to the project is restarted (`deactivate`, `activate`) when the window's
//! project root changes: its sandbox is opened on the root.

use std::collections::VecDeque;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedSender;
use serde_json::{Map, Value};
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder, Trap};
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::api::bindings::Plugin;
use crate::api::dialogs::{Question, TextQuestion};
use crate::api::editors::TextEdit;
use crate::api::events::Event;
use crate::api::notifications::{Notification, Progress};
use crate::api::status_bar::StatusItem;
use crate::api::types::{EditorInfo, Range};
use crate::api::ui::View;
use crate::host::{HostState, LogSignal, LogStream};
use crate::log::{Level, PluginLog};
use crate::manifest::ProjectAccess;
use crate::registry::PluginEntry;

/// The longest one call into a plugin may take; then the plugin is stopped.
pub const CALL_LIMIT: Duration = Duration::from_secs(10);
/// The most memory a plugin may take.
pub const MEMORY_LIMIT: usize = 512 << 20;

/// How often the engine's epoch advances: the precision of the time limit.
const TICK: Duration = Duration::from_millis(10);

/// What an instance starts with.
pub struct InstanceConfig {
    pub entry: Arc<PluginEntry>,
    /// The window's project root: the plugin searches it, and reads or writes its files as its
    /// permissions say.
    pub project_root: Option<PathBuf>,
    /// The interface language: "en", "ru".
    pub language: String,
    /// The plugin's settings: the user's values over the manifest's defaults.
    pub settings: Map<String, Value>,
    /// Where the instance's messages go.
    pub messages: UnboundedSender<PluginMessage>,
    pub log: PluginLog,
}

/// A running plugin. Dropping it stops the plugin like [`Instance::stop`].
pub struct Instance {
    id: Arc<str>,
    calls: Sender<Call>,
    /// Set when the instance is stopped: a search in progress gives up.
    cancel: Arc<AtomicBool>,
}

/// A call posted to the plugin's thread.
enum Call {
    Command(String),
    Event(Event),
    Settings(Map<String, Value>),
    Root(Option<PathBuf>),
    Stop,
}

impl Instance {
    /// Starts the plugin's thread: compiles the component (or takes it from the cache),
    /// instantiates it and calls `activate`. Then [`MessageKind::Started`] comes, or
    /// [`MessageKind::Stopped`] if it couldn't.
    pub fn start(config: InstanceConfig) -> Instance {
        let id: Arc<str> = config.entry.id().into();
        let (calls, inbox) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let messages = config.messages.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("plugin {id}"))
            .spawn({
                let cancel = cancel.clone();
                move || run(config, inbox, cancel)
            });
        if let Err(err) = spawned {
            let _ = messages.unbounded_send(PluginMessage {
                plugin: id.clone(),
                kind: MessageKind::Stopped {
                    error: format!("Couldn't start a thread: {err}"),
                    details: String::new(),
                },
            });
        }
        Instance { id, calls, cancel }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Runs one of the plugin's commands.
    pub fn run_command(&self, command: &str) {
        let _ = self.calls.send(Call::Command(command.to_string()));
    }

    /// Delivers an event. Bursts of `editor-changed` and `selection-changed` of one document
    /// waiting in the queue are coalesced into one.
    pub fn send_event(&self, event: Event) {
        let _ = self.calls.send(Call::Event(event));
    }

    /// New values of the plugin's settings; the window sends `settings-changed` after them.
    pub fn set_settings(&self, settings: Map<String, Value>) {
        let _ = self.calls.send(Call::Settings(settings));
    }

    /// The window's project root changed; the window sends `project-changed` after it.
    pub fn set_project_root(&self, root: Option<PathBuf>) {
        let _ = self.calls.send(Call::Root(root));
    }

    /// Calls `deactivate` after the calls already queued, and ends the thread. From now on the
    /// window hears nothing from the instance: the calls it still makes (in `deactivate`, say)
    /// are dropped, and a stale `Started` or `Stopped` never comes — a new instance of the same
    /// plugin can start right away.
    pub fn stop(self) {}
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.calls.send(Call::Stop);
    }
}

/// A message of an instance to its window.
#[derive(Debug)]
pub struct PluginMessage {
    /// The plugin's id.
    pub plugin: Arc<str>,
    pub kind: MessageKind,
}

#[derive(Debug)]
pub enum MessageKind {
    /// `activate` returned: the plugin runs.
    Started,
    /// A call of the plugin that needs the window.
    Call(HostCall),
    /// The plugin stopped by itself: a trap, the time or memory limit, a failed start. The
    /// instance is gone. `error` is a line for a notification; `details` the whole story (the
    /// backtrace, the build output).
    Stopped { error: String, details: String },
    /// New lines in the plugin's log.
    Logged,
}

/// The plugin's calls the window carries out. Ids of notifications and questions are the
/// plugin's own (the instance numbers them); the window maps them to its notifications.
#[derive(Debug)]
pub enum HostCall {
    Notify {
        id: u64,
        notification: Notification,
    },
    UpdateNotification {
        id: u64,
        notification: Notification,
    },
    SetProgress {
        id: u64,
        progress: Option<Progress>,
    },
    ExpireNotification {
        id: u64,
    },
    RemoveNotification {
        id: u64,
    },
    /// A question; the answer goes back as `Event::DialogAnswered(id, …)`.
    Ask {
        id: u64,
        question: Question,
    },
    /// A question with a text field; the answer goes back as `Event::TextAnswered(id, …)`.
    AskText {
        id: u64,
        question: TextQuestion,
    },
    SetCommandEnabled {
        command: String,
        enabled: bool,
    },
    SetStatusItem {
        id: String,
        item: Option<StatusItem>,
    },
    SetView {
        window: String,
        view: View,
    },
    ShowToolWindow {
        window: String,
    },
    HideToolWindow {
        window: String,
    },
    /// A call about documents: the plugin's thread waits for the reply.
    Editor {
        call: EditorCall,
        reply: Sender<EditorReply>,
    },
}

/// The `editors` interface. Ids are the window's (`EditorInfo::id`).
#[derive(Debug, Clone, PartialEq)]
pub enum EditorCall {
    Active,
    List,
    Text(u64),
    Selections(u64),
    SetSelections(u64, Vec<Range>),
    Edit(u64, Vec<TextEdit>),
    Open(String, Option<Range>),
    Save(u64),
}

/// The window's reply to an [`EditorCall`], in the same order of kinds.
#[derive(Debug, Clone, PartialEq)]
pub enum EditorReply {
    /// `Active`.
    Editor(Option<EditorInfo>),
    /// `List`.
    Editors(Vec<EditorInfo>),
    /// `Text`.
    Text(Option<String>),
    /// `Selections`.
    Selections(Vec<Range>),
    /// `SetSelections`, `Edit`, `Save`.
    Done(Result<(), String>),
    /// `Open`: the document's id.
    Opened(Result<u64, String>),
}

// --- The engine ---

/// The engine and the linker every instance shares.
struct Shared {
    engine: Engine,
    linker: Linker<State>,
}

static SHARED: OnceLock<Result<Shared, String>> = OnceLock::new();

fn shared() -> Result<&'static Shared, String> {
    SHARED
        .get_or_init(|| {
            let mut config = Config::new();
            config.epoch_interruption(true);
            let engine = Engine::new(&config).map_err(|err| format!("{err:#}"))?;
            let mut linker = Linker::new(&engine);
            wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(|err| format!("{err:#}"))?;
            Plugin::add_to_linker::<State, HasSelf<State>>(&mut linker, |state| state)
                .map_err(|err| format!("{err:#}"))?;
            // The clock of the time limit: deadlines are counted in ticks of the epoch. The engine
            // lives as long as the process, and so does its clock.
            let ticker = engine.clone();
            std::thread::Builder::new()
                .name("plugin epoch".into())
                .spawn(move || {
                    loop {
                        ticker.increment_epoch();
                        std::thread::sleep(TICK);
                    }
                })
                .map_err(|err| err.to_string())?;
            Ok(Shared { engine, linker })
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// The time limit of one call: [`CALL_LIMIT`], or `FLUX_PLUGIN_CALL_LIMIT_MS` (UI scenarios).
pub(crate) fn call_limit(plugin: &str) -> Duration {
    static FROM_ENV: OnceLock<Option<Duration>> = OnceLock::new();
    #[cfg(test)]
    if let Some(limit) = test_limits().lock().unwrap().get(plugin) {
        return *limit;
    }
    let _ = plugin;
    FROM_ENV
        .get_or_init(|| {
            std::env::var("FLUX_PLUGIN_CALL_LIMIT_MS")
                .ok()?
                .parse()
                .ok()
                .map(Duration::from_millis)
        })
        .unwrap_or(CALL_LIMIT)
}

/// Shorter limits of some plugins, by id, for tests.
#[cfg(test)]
pub(crate) fn test_limits() -> &'static std::sync::Mutex<std::collections::HashMap<String, Duration>>
{
    static LIMITS: OnceLock<std::sync::Mutex<std::collections::HashMap<String, Duration>>> =
        OnceLock::new();
    LIMITS.get_or_init(Default::default)
}

/// The compiled component: from the cache, or compiled by Cranelift and put there. Says which
/// in the log, with the time.
fn component(engine: &Engine, bytes: &[u8], log: &PluginLog) -> wasmtime::Result<Component> {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    engine.precompile_compatibility_hash().hash(&mut hasher);
    let dir = crate::paths::cache_dir();
    let file = dir.join(format!("{:016x}-{}.cwasm", hasher.finish(), bytes.len()));
    let started = Instant::now();
    if let Ok(compiled) = std::fs::read(&file) {
        // SAFETY: the file is Flux's own cache, written by `serialize` below for this engine
        // (its compatibility hash is in the name); `deserialize` checks the header and the
        // version, and a file it refuses is compiled again.
        if let Ok(component) = unsafe { Component::deserialize(engine, &compiled) } {
            log.write(
                Level::Debug,
                &format!("Loaded from the cache in {} ms", millis(started.elapsed())),
            );
            // Recently used: the cache keeps it over older ones.
            if let Ok(cached) = std::fs::File::options().append(true).open(&file) {
                let _ = cached.set_modified(std::time::SystemTime::now());
            }
            return Ok(component);
        }
    }
    let component = Component::new(engine, bytes)?;
    log.write(
        Level::Debug,
        &format!("Compiled in {} ms", millis(started.elapsed())),
    );
    if let Ok(compiled) = component.serialize() {
        let _ = std::fs::create_dir_all(&dir);
        let temp = file.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&temp, compiled).is_ok() && std::fs::rename(&temp, &file).is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        trim_cache(&dir);
    }
    Ok(component)
}

/// The compiled components the cache keeps: every rebuild of a plugin under development adds
/// one, so the least recently used go.
const CACHE_FILES: usize = 64;

fn trim_cache(dir: &std::path::Path) {
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "cwasm"))
        .filter_map(|path| Some((std::fs::metadata(&path).ok()?.modified().ok()?, path)))
        .collect();
    if files.len() <= CACHE_FILES {
        return;
    }
    files.sort();
    for (_, path) in &files[..files.len() - CACHE_FILES] {
        let _ = std::fs::remove_file(path);
    }
}

fn millis(duration: Duration) -> String {
    format!("{:.1}", duration.as_secs_f64() * 1000.)
}

// --- The plugin's thread ---

/// The data of a plugin's store: the WASI context and what the API's functions need.
pub(crate) struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
    pub(crate) host: HostState,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// Why the plugin stopped: a line and the whole story.
struct Stop {
    error: String,
    details: String,
}

impl Stop {
    fn new(error: impl Into<String>, details: impl Into<String>) -> Self {
        Stop {
            error: error.into(),
            details: details.into(),
        }
    }
}

/// The body of the plugin's thread: start, then the calls one by one until `Stop`.
fn run(config: InstanceConfig, inbox: Receiver<Call>, cancel: Arc<AtomicBool>) {
    let id: Arc<str> = config.entry.id().into();
    let messages = config.messages.clone();
    let log = config.log.clone();
    let stopping = cancel.clone();
    // Once the window has let the instance go, it hears nothing more from it.
    let send = |kind| {
        if !stopping.load(Ordering::Relaxed) {
            let _ = messages.unbounded_send(PluginMessage {
                plugin: id.clone(),
                kind,
            });
        }
    };
    let mut runner = match Runner::start(config, cancel) {
        Ok(runner) => runner,
        Err(stop) => {
            log.write(Level::Error, &format!("Couldn't start: {}", stop.error));
            send(MessageKind::Stopped {
                error: stop.error,
                details: stop.details,
            });
            return;
        }
    };
    send(MessageKind::Started);
    let mut queue = VecDeque::new();
    loop {
        if queue.is_empty() {
            match inbox.recv() {
                Ok(call) => enqueue(&mut queue, call),
                // The instance is gone without `Stop` (it can't be, `Drop` sends it).
                Err(_) => break,
            }
        }
        while let Ok(call) = inbox.try_recv() {
            enqueue(&mut queue, call);
        }
        let Some(call) = queue.pop_front() else {
            continue;
        };
        let result = match call {
            Call::Command(command) => {
                runner.call(|plugin, store| plugin.call_run_command(store, &command))
            }
            Call::Event(event) => runner.call(|plugin, store| plugin.call_on_event(store, &event)),
            Call::Settings(settings) => {
                runner.store.data_mut().host.settings = settings;
                Ok(())
            }
            Call::Root(root) => runner.set_root(root).map(|restarted| {
                if restarted {
                    send(MessageKind::Started);
                }
            }),
            Call::Stop => break,
        };
        if let Err(stop) = result {
            log.write(Level::Error, &format!("Stopped: {}", stop.error));
            send(MessageKind::Stopped {
                error: stop.error,
                details: stop.details,
            });
            return;
        }
    }
    runner.deactivate();
}

/// Puts a call into the queue; a document's `editor-changed` or `selection-changed` already
/// waiting there makes a new one unnecessary.
fn enqueue(queue: &mut VecDeque<Call>, call: Call) {
    let waiting = |event: &Event| {
        queue.iter().any(|queued| match queued {
            Call::Event(queued) => queued == event,
            _ => false,
        })
    };
    if let Call::Event(event @ (Event::EditorChanged(_) | Event::SelectionChanged(_))) = &call
        && waiting(event)
    {
        return;
    }
    queue.push_back(call);
}

/// The plugin's store and instance; without code — neither, and calls do nothing.
struct Runner {
    store: Store<State>,
    plugin: Option<Plugin>,
    component: Option<Component>,
    config: Arc<RunnerConfig>,
    stdout: LogStream,
    stderr: LogStream,
    signal: Arc<LogSignal>,
}

/// What a store is made from: kept to make a new one when the project root changes.
struct RunnerConfig {
    entry: Arc<PluginEntry>,
    language: String,
    messages: UnboundedSender<PluginMessage>,
    log: PluginLog,
    cancel: Arc<AtomicBool>,
}

impl Runner {
    fn start(config: InstanceConfig, cancel: Arc<AtomicBool>) -> Result<Runner, Stop> {
        let id: Arc<str> = config.entry.id().into();
        let signal = Arc::new(LogSignal::new(id.clone(), config.messages.clone()));
        let stdout = LogStream::new(Level::Info, config.log.clone(), signal.clone());
        let stderr = LogStream::new(Level::Warn, config.log.clone(), signal.clone());
        let runner_config = Arc::new(RunnerConfig {
            entry: config.entry.clone(),
            language: config.language,
            messages: config.messages,
            log: config.log,
            cancel,
        });
        let component = match &runner_config.entry.manifest.wasm {
            None => None,
            Some(wasm) => {
                let bytes = runner_config.entry.wasm().ok_or_else(|| {
                    Stop::new(format!("The component {wasm} is missing"), String::new())
                })?;
                let shared = shared().map_err(|err| Stop::new("Plugins can't run", err))?;
                let component = component(&shared.engine, &bytes, &runner_config.log)
                    .map_err(|err| Stop::new(first_line(&err), format!("{err:?}")))?;
                Some(component)
            }
        };
        let store = new_store(
            &runner_config,
            config.project_root,
            config.settings,
            &stdout,
            &stderr,
            &signal,
        )?;
        let mut runner = Runner {
            store,
            plugin: None,
            component,
            config: runner_config,
            stdout,
            stderr,
            signal,
        };
        runner.instantiate()?;
        Ok(runner)
    }

    /// Instantiates the component in the store and calls `activate`.
    fn instantiate(&mut self) -> Result<(), Stop> {
        let Some(component) = &self.component else {
            return Ok(());
        };
        let shared = shared().map_err(|err| Stop::new("Plugins can't run", err))?;
        self.store
            .set_epoch_deadline(deadline_ticks(self.config.entry.id()));
        let plugin = Plugin::instantiate(&mut self.store, component, &shared.linker)
            .map_err(|err| self.stop(&err, "Couldn't load"))?;
        self.plugin = Some(plugin);
        self.call(|plugin, store| plugin.call_activate(store))
    }

    /// Calls an export within the time limit; a trap ends the plugin.
    fn call(
        &mut self,
        f: impl FnOnce(&Plugin, &mut Store<State>) -> wasmtime::Result<()>,
    ) -> Result<(), Stop> {
        let Some(plugin) = &self.plugin else {
            return Ok(());
        };
        self.stderr.clear_recent();
        self.store
            .set_epoch_deadline(deadline_ticks(self.config.entry.id()));
        let result = f(plugin, &mut self.store);
        self.stdout.flush_partial();
        self.stderr.flush_partial();
        self.signal.flush();
        result.map_err(|err| self.stop(&err, ""))
    }

    /// Why a call failed, in words: the time limit, the memory limit, a panic, a trap.
    fn stop(&self, err: &wasmtime::Error, context: &str) -> Stop {
        let output = self.stderr.recent();
        let panic = panic_message(&output);
        let out_of_memory = output
            .iter()
            .any(|line| line.contains("memory allocation of"));
        let error = match err.downcast_ref::<Trap>() {
            Some(Trap::Interrupt) => format!(
                "took longer than {}",
                seconds(call_limit(self.config.entry.id()))
            ),
            _ if out_of_memory => format!("ran out of memory ({} MB)", MEMORY_LIMIT >> 20),
            _ if panic.is_some() => format!("panicked: {}", panic.unwrap_or_default()),
            Some(trap) => trap.to_string(),
            None => first_line(err),
        };
        let error = if context.is_empty() {
            error
        } else {
            format!("{context}: {error}")
        };
        let mut details = format!("{err:?}");
        if !output.is_empty() {
            details.push_str("\n\nOutput:\n");
            details.push_str(&output.join("\n"));
        }
        Stop { error, details }
    }

    /// A new project root. A plugin with access to the project gets a new sandbox: it is
    /// deactivated and activated again in a new store (true).
    fn set_root(&mut self, root: Option<PathBuf>) -> Result<bool, Stop> {
        let host = &mut self.store.data_mut().host;
        if host.root == root {
            return Ok(false);
        }
        host.root = root.clone();
        if self.config.entry.manifest.permissions.project == ProjectAccess::None
            || self.component.is_none()
        {
            return Ok(false);
        }
        let settings = host.settings.clone();
        self.deactivate();
        self.store = new_store(
            &self.config,
            root,
            settings,
            &self.stdout,
            &self.stderr,
            &self.signal,
        )?;
        self.plugin = None;
        self.instantiate().map(|()| true)
    }

    /// Calls `deactivate`; a failure there is only logged: the plugin is going anyway.
    fn deactivate(&mut self) {
        if self.plugin.is_none() {
            return;
        }
        if let Err(stop) = self.call(|plugin, store| plugin.call_deactivate(store)) {
            self.config
                .log
                .write(Level::Warn, &format!("deactivate failed: {}", stop.error));
        }
        self.plugin = None;
    }
}

/// A store with the sandbox: the project root as the permissions say, the plugin's data folder,
/// stdout and stderr into the log; no environment, arguments or network.
fn new_store(
    config: &RunnerConfig,
    root: Option<PathBuf>,
    settings: Map<String, Value>,
    stdout: &LogStream,
    stderr: &LogStream,
    signal: &Arc<LogSignal>,
) -> Result<Store<State>, Stop> {
    let shared = shared().map_err(|err| Stop::new("Plugins can't run", err))?;
    let id = config.entry.id();
    let data_dir = crate::paths::data_dir(id);
    std::fs::create_dir_all(&data_dir).map_err(|err| {
        Stop::new(
            format!("Couldn't create the plugin's folder: {err}"),
            data_dir.display().to_string(),
        )
    })?;
    let mut wasi = WasiCtxBuilder::new();
    // Files are read on the plugin's own thread: no thread pool is needed.
    wasi.allow_blocking_current_thread(true);
    wasi.stdout(stdout.clone()).stderr(stderr.clone());
    let access = match config.entry.manifest.permissions.project {
        ProjectAccess::None => None,
        ProjectAccess::Read => Some(FsPerms::ReadOnly),
        ProjectAccess::Write => Some(FsPerms::ReadWrite),
    };
    // A project folder that can't be opened (removed meanwhile) leaves the plugin without it,
    // not stopped.
    if let (Some(perms), Some(root)) = (access, &root)
        && let Err(err) = wasi.preopened_dir(root, root.to_string_lossy(), perms)
    {
        config.log.write(
            Level::Warn,
            &format!("Couldn't open the project {}: {err}", root.display()),
        );
    }
    wasi.preopened_dir(&data_dir, data_dir.to_string_lossy(), FsPerms::ReadWrite)
        .map_err(|err| Stop::new(format!("Couldn't open the plugin's folder: {err}"), ""))?;
    let host = HostState {
        id: id.into(),
        entry: config.entry.clone(),
        language: config.language.clone(),
        root,
        settings,
        messages: config.messages.clone(),
        log: config.log.clone(),
        signal: signal.clone(),
        next_id: 0,
        storage: None,
        data_dir,
        cancel: config.cancel.clone(),
    };
    let state = State {
        wasi: wasi.build(),
        table: ResourceTable::new(),
        limits: StoreLimitsBuilder::new().memory_size(MEMORY_LIMIT).build(),
        host,
    };
    let mut store = Store::new(&shared.engine, state);
    store.limiter(|state| &mut state.limits);
    store.epoch_deadline_trap();
    Ok(store)
}

/// The plugin's time limit in ticks of the epoch.
fn deadline_ticks(plugin: &str) -> u64 {
    (call_limit(plugin).as_millis() / TICK.as_millis()).max(1) as u64
}

/// "10 s", "300 ms".
fn seconds(duration: Duration) -> String {
    if duration < Duration::from_secs(1) {
        format!("{} ms", duration.as_millis())
    } else if duration.subsec_millis() == 0 {
        format!("{} s", duration.as_secs())
    } else {
        format!("{:.1} s", duration.as_secs_f64())
    }
}

fn first_line(err: &wasmtime::Error) -> String {
    err.to_string()
        .lines()
        .next()
        .unwrap_or("unknown error")
        .to_string()
}

/// The message of a Rust panic from its output: the lines between "… panicked at …:" and the
/// `RUST_BACKTRACE` note.
fn panic_message(output: &[String]) -> Option<String> {
    let start = output
        .iter()
        .rposition(|line| line.contains(" panicked at "))?;
    let message: Vec<&str> = output[start + 1..]
        .iter()
        .map(|line| line.trim())
        .take_while(|line| !line.starts_with("note: "))
        .filter(|line| !line.is_empty())
        .collect();
    (!message.is_empty()).then(|| message.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_messages() {
        let output: Vec<String> = [
            "thread '<unnamed>' panicked at src/lib.rs:28:17:",
            "index out of bounds: the len is 0 but the index is 3",
            "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            panic_message(&output).as_deref(),
            Some("index out of bounds: the len is 0 but the index is 3")
        );
        assert_eq!(panic_message(&["just output".to_string()]), None);
    }

    #[test]
    fn coalesces_bursts_of_one_document() {
        let mut queue = VecDeque::new();
        enqueue(&mut queue, Call::Event(Event::EditorChanged(1)));
        enqueue(&mut queue, Call::Event(Event::SelectionChanged(1)));
        enqueue(&mut queue, Call::Event(Event::EditorChanged(1)));
        enqueue(&mut queue, Call::Event(Event::EditorChanged(2)));
        enqueue(&mut queue, Call::Event(Event::SettingsChanged));
        enqueue(&mut queue, Call::Event(Event::SettingsChanged));
        assert_eq!(queue.len(), 5);
    }

    #[test]
    fn limits_in_words() {
        assert_eq!(seconds(Duration::from_secs(10)), "10 s");
        assert_eq!(seconds(Duration::from_millis(300)), "300 ms");
        assert_eq!(seconds(Duration::from_millis(1500)), "1.5 s");
    }
}
