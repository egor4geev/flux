//! `tasks` (API 0.6): the user's tasks between plugins. A plugin with `task-source = true` (a
//! tracker's) gives them: each of its instances joins the sources when its store is made and
//! leaves when it is gone ([`join`]). Another plugin with the `tasks` permission asks with `find`:
//! the query goes to every source plugin that runs — its service when it has one, else one of
//! its windows' instances — as `task-query`, straight into its queue; their `respond`s come back
//! to the asking thread, which waits up to [`FIND_TIMEOUT`] and merges the tasks in the sources'
//! order (by plugin id), each marked with the plugin that gave it (`source`). An instance never
//! gets its own query: it couldn't answer while it waits.
//!
//! `run` does one of a source's Claude actions on a task, in a project. The work is the project's
//! window's — Git, the tabs, Claude's chat — so the query goes to the source's instance in that
//! window, never to its service: the app gives the window, opened in the background when none has
//! the project and ready for the action ([`HostCall::ProjectWindow`]); the asking thread waits for
//! the source's instance there to join (it starts with the window), up to [`WINDOW_TIMEOUT`] for
//! both, then for the source's `started` up to [`RUN_TIMEOUT`]. A `started` of another plugin
//! than the one asked is dropped with a warning. The waits don't count toward the asker's time
//! limit.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::api::events::Event;
use crate::api::tasks::{self as wit, Query, Task, TaskQuery, TaskRun, TaskStarted};
use crate::host::HostState;
use crate::runtime::{EventSender, HostCall, InstanceRole, State};

use super::window::missing;

/// How long `find` waits for the sources.
const FIND_TIMEOUT: Duration = Duration::from_secs(10);
/// How long `run` waits for the project's window to be ready and the source's instance in it.
const WINDOW_TIMEOUT: Duration = Duration::from_secs(20);
/// How long `run` waits for the source's `started`; `FLUX_TASK_RUN_TIMEOUT_MS` makes it shorter
/// for a check (a source that doesn't answer).
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// The running instances of the source plugins, in the order they joined; the condition is
/// signalled when one joins (a `run` waits for the source's instance in a window just opened).
fn sources() -> &'static (Mutex<Vec<SourceInstance>>, Condvar) {
    static SOURCES: OnceLock<(Mutex<Vec<SourceInstance>>, Condvar)> = OnceLock::new();
    SOURCES.get_or_init(Default::default)
}

struct SourceInstance {
    token: u64,
    plugin: Arc<str>,
    /// The plugin's name, for the asker's errors: "Yandex Tracker didn't answer in a minute".
    name: Arc<str>,
    role: InstanceRole,
    events: EventSender,
}

/// The queries waiting for answers, by id.
fn queries() -> &'static Mutex<HashMap<u64, Pending>> {
    static QUERIES: OnceLock<Mutex<HashMap<u64, Pending>>> = OnceLock::new();
    QUERIES.get_or_init(Default::default)
}

/// A query in progress: the plugins that haven't answered yet, and where the answers go.
struct Pending {
    waiting: Vec<Arc<str>>,
    answers: Sender<Answer>,
}

/// A source's answer: its plugin and the tasks it gave, or why it couldn't.
type Answer = (Arc<str>, Result<Vec<Task>, String>);

/// The runs waiting for the source's `started`, by id.
fn runs() -> &'static Mutex<HashMap<u64, PendingRun>> {
    static RUNS: OnceLock<Mutex<HashMap<u64, PendingRun>>> = OnceLock::new();
    RUNS.get_or_init(Default::default)
}

/// A run in progress: the plugin that must answer, and where the answer goes.
struct PendingRun {
    plugin: Arc<str>,
    answer: Sender<Result<TaskStarted, String>>,
}

static TOKENS: AtomicU64 = AtomicU64::new(1);
/// The ids of the queries and the runs: one counter, so `respond` and `started` never mix them.
static IDS: AtomicU64 = AtomicU64::new(1);

/// An instance's place among the sources: it leaves when dropped.
pub(crate) struct Source {
    token: u64,
}

/// An instance of a plugin with `task-source = true` (`name` — the manifest's) joins the sources.
pub(crate) fn join(plugin: &str, name: &str, role: InstanceRole, events: &EventSender) -> Source {
    let token = TOKENS.fetch_add(1, Ordering::Relaxed);
    let (list, joined) = sources();
    list.lock().unwrap().push(SourceInstance {
        token,
        plugin: plugin.into(),
        name: name.into(),
        role,
        events: events.clone(),
    });
    joined.notify_all();
    Source { token }
}

impl Drop for Source {
    fn drop(&mut self) {
        sources()
            .0
            .lock()
            .unwrap()
            .retain(|source| source.token != self.token);
    }
}

/// The instance that answers for each source plugin, by plugin id: the service, else the window
/// with the smallest id — never the asking instance (its token).
fn answering(asker: Option<u64>) -> BTreeMap<Arc<str>, EventSender> {
    let mut chosen: BTreeMap<Arc<str>, (u8, u64, EventSender)> = BTreeMap::new();
    for source in sources().0.lock().unwrap().iter() {
        if Some(source.token) == asker {
            continue;
        }
        let rank = match source.role {
            InstanceRole::Service => (0, 0),
            InstanceRole::Window(window) => (1, window),
        };
        let better = chosen
            .get(&source.plugin)
            .is_none_or(|(kind, window, _)| rank < (*kind, *window));
        if better {
            chosen.insert(
                source.plugin.clone(),
                (rank.0, rank.1, source.events.clone()),
            );
        }
    }
    chosen
        .into_iter()
        .map(|(plugin, (_, _, events))| (plugin, events))
        .collect()
}

/// The name of a running source plugin; none when it doesn't run (or gives no tasks).
fn source_name(plugin: &str) -> Option<Arc<str>> {
    sources()
        .0
        .lock()
        .unwrap()
        .iter()
        .find(|source| *source.plugin == *plugin)
        .map(|source| source.name.clone())
}

/// The source's instance in a window, waiting for it to join until `deadline`: none when it
/// doesn't come. The asking instance (its token) can't answer while it waits: an error.
fn instance_in(
    plugin: &str,
    window: u64,
    asker: Option<u64>,
    deadline: Instant,
) -> Option<Result<EventSender, String>> {
    let (list, joined) = sources();
    let mut list = list.lock().unwrap();
    loop {
        let found = list.iter().find(|source| {
            *source.plugin == *plugin && source.role == InstanceRole::Window(window)
        });
        if let Some(source) = found {
            if Some(source.token) == asker {
                return Some(Err(
                    "A plugin can't run its own action from the window that runs it: ask from \
                     its service"
                        .into(),
                ));
            }
            return Some(Ok(source.events.clone()));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        list = joined.wait_timeout(list, left).unwrap().0;
    }
}

/// How long a run of `plugin`'s waits for its `started`.
fn run_timeout(plugin: &str) -> Duration {
    #[cfg(test)]
    if let Some(timeout) = test_run_timeouts().lock().unwrap().get(plugin) {
        return *timeout;
    }
    let _ = plugin;
    std::env::var("FLUX_TASK_RUN_TIMEOUT_MS")
        .ok()
        .and_then(|ms| ms.parse().ok())
        .map_or(RUN_TIMEOUT, Duration::from_millis)
}

/// Tests: a shorter wait for a source's `started`, by the source's plugin id.
#[cfg(test)]
pub(crate) fn test_run_timeouts() -> &'static Mutex<HashMap<String, Duration>> {
    static TIMEOUTS: OnceLock<Mutex<HashMap<String, Duration>>> = OnceLock::new();
    TIMEOUTS.get_or_init(Default::default)
}

/// "a minute", "5 seconds", "500 ms": how long a wait was, for an error.
fn spoken(wait: Duration) -> String {
    match wait.as_secs() {
        0 => format!("{} ms", wait.as_millis()),
        1 => "a second".into(),
        60 => "a minute".into(),
        seconds => format!("{seconds} seconds"),
    }
}

impl HostState {
    /// `tasks.run` after the checks: the project's window, the source's instance there, its
    /// `started`.
    fn run_action(&self, run: TaskRun, name: &str) -> Result<TaskStarted, String> {
        let deadline = Instant::now() + WINDOW_TIMEOUT;
        let (reply, answer) = mpsc::channel();
        self.call(HostCall::ProjectWindow {
            project: run.project.clone(),
            reply,
        });
        let window = match answer.recv_timeout(WINDOW_TIMEOUT) {
            Ok(window) => window?,
            Err(_) => return Err(format!("No window could open {}", run.project)),
        };
        let asker = self.task_source.as_ref().map(|source| source.token);
        let events = instance_in(&run.source, window, asker, deadline).unwrap_or_else(|| {
            Err(format!(
                "{name} didn't start in the window of {}",
                run.project
            ))
        })?;
        let id = IDS.fetch_add(1, Ordering::Relaxed);
        let (answer, received) = mpsc::channel();
        runs().lock().unwrap().insert(
            id,
            PendingRun {
                plugin: run.source.as_str().into(),
                answer,
            },
        );
        let timeout = run_timeout(&run.source);
        let sent = events.send(Event::TaskQuery(TaskQuery {
            id,
            query: Query::Run(run),
        }));
        let result = match sent {
            true => received.recv_timeout(timeout).ok(),
            false => None,
        };
        runs().lock().unwrap().remove(&id);
        result.unwrap_or_else(|| Err(format!("{name} didn't answer in {}", spoken(timeout))))
    }
}

impl wit::Host for State {
    fn find(&mut self, query: Query) -> Result<Vec<Task>, String> {
        let host = &mut self.host;
        if !host.entry.manifest.permissions.tasks {
            return Err(missing("ask for the user's tasks", "tasks = true"));
        }
        if matches!(query, Query::Run(_)) {
            return Err("tasks.find: an action runs with tasks.run".into());
        }
        let asker = host.task_source.as_ref().map(|source| source.token);
        let sources = answering(asker);
        if sources.is_empty() {
            return Err("No plugin gives tasks".into());
        }
        let id = IDS.fetch_add(1, Ordering::Relaxed);
        let (answers, received) = mpsc::channel();
        let mut asked = Vec::new();
        {
            let mut queries = queries().lock().unwrap();
            for (plugin, events) in &sources {
                let event = Event::TaskQuery(TaskQuery {
                    id,
                    query: query.clone(),
                });
                if events.send(event) {
                    asked.push(plugin.clone());
                }
            }
            queries.insert(
                id,
                Pending {
                    waiting: asked.clone(),
                    answers,
                },
            );
        }
        let started = Instant::now();
        let deadline = started + FIND_TIMEOUT;
        let mut got: HashMap<Arc<str>, Result<Vec<Task>, String>> = HashMap::new();
        while got.len() < asked.len() {
            let left = deadline.saturating_duration_since(Instant::now());
            match received.recv_timeout(left) {
                Ok((plugin, tasks)) => {
                    got.insert(plugin, tasks);
                }
                Err(_) => break,
            }
        }
        queries().lock().unwrap().remove(&id);
        host.waited += started.elapsed();
        // In the sources' order, each marked with its source; the failures only when no source
        // gave anything.
        let mut tasks = Vec::new();
        let mut errors = Vec::new();
        let mut answered = false;
        for plugin in &asked {
            match got.remove(plugin) {
                Some(Ok(found)) => {
                    answered = true;
                    tasks.extend(found.into_iter().map(|mut task| {
                        task.source = plugin.to_string();
                        task
                    }));
                }
                Some(Err(error)) => errors.push((plugin.clone(), error)),
                None => {}
            }
        }
        if answered {
            return Ok(tasks);
        }
        // One source's sentence goes to the user as it is; several are told apart by their plugin.
        match errors.as_slice() {
            [] => Err("No plugin with tasks answered in 10 seconds".into()),
            [(_, error)] => Err(error.clone()),
            errors => Err(errors
                .iter()
                .map(|(plugin, error)| format!("{plugin}: {error}"))
                .collect::<Vec<_>>()
                .join("; ")),
        }
    }

    fn respond(&mut self, id: u64, tasks: Result<Vec<Task>, String>) {
        let host = &self.host;
        let mut queries = queries().lock().unwrap();
        let waiting = queries.get_mut(&id).and_then(|query| {
            let at = query
                .waiting
                .iter()
                .position(|plugin| **plugin == *host.id)?;
            query.waiting.remove(at);
            Some(query.answers.clone())
        });
        drop(queries);
        match waiting {
            Some(answers) => {
                let _ = answers.send((host.id.clone(), tasks));
            }
            None if runs().lock().unwrap().contains_key(&id) => host.warn(&format!(
                "tasks.respond: the query {id} is a run — answer it with tasks.started"
            )),
            None => host.warn(&format!("tasks.respond: the query {id} is gone")),
        }
    }

    fn run(&mut self, run: TaskRun) -> Result<TaskStarted, String> {
        let host = &mut self.host;
        if !host.entry.manifest.permissions.tasks {
            return Err(missing("run actions on the user's tasks", "tasks = true"));
        }
        if run.source.is_empty() {
            return Err("The task has no source: take it from tasks.find".into());
        }
        let Some(name) = source_name(&run.source) else {
            return Err(format!("No plugin {} gives tasks", run.source));
        };
        let started = Instant::now();
        let result = host.run_action(run, &name);
        host.waited += started.elapsed();
        result
    }

    fn started(&mut self, id: u64, result: Result<TaskStarted, String>) {
        let host = &self.host;
        let mut runs = runs().lock().unwrap();
        let mine = runs
            .get(&id)
            .map(|run| *run.plugin == *host.id)
            .unwrap_or(false);
        let pending = if mine { runs.remove(&id) } else { None };
        let known = runs.contains_key(&id);
        drop(runs);
        match pending {
            Some(run) => {
                let _ = run.answer.send(result);
            }
            None if known => host.warn(&format!(
                "tasks.started: the run {id} was asked of another plugin"
            )),
            None => host.warn(&format!("tasks.started: the run {id} is gone")),
        }
    }
}
