//! `tasks` (API 0.6): the user's tasks between plugins. A plugin with `task-source = true` (a
//! tracker's) gives them: each of its instances joins the sources when its store is made and
//! leaves when it is gone ([`join`]). Another plugin with the `tasks` permission asks with `find`:
//! the query goes to every source plugin that runs — its service when it has one, else one of
//! its windows' instances — as `task-query`, straight into its queue; their `respond`s come back
//! to the asking thread, which waits up to [`FIND_TIMEOUT`] and merges the tasks in the sources'
//! order (by plugin id). An instance never gets its own query: it couldn't answer while it waits.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::api::events::Event;
use crate::api::tasks::{self as wit, Query, Task, TaskQuery};
use crate::runtime::{EventSender, InstanceRole, State};

use super::window::missing;

/// How long `find` waits for the sources.
const FIND_TIMEOUT: Duration = Duration::from_secs(10);

/// The running instances of the source plugins, in the order they joined.
fn sources() -> &'static Mutex<Vec<SourceInstance>> {
    static SOURCES: OnceLock<Mutex<Vec<SourceInstance>>> = OnceLock::new();
    SOURCES.get_or_init(Default::default)
}

struct SourceInstance {
    token: u64,
    plugin: Arc<str>,
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

static TOKENS: AtomicU64 = AtomicU64::new(1);

/// An instance's place among the sources: it leaves when dropped.
pub(crate) struct Source {
    token: u64,
}

/// An instance of a plugin with `task-source = true` joins the sources.
pub(crate) fn join(plugin: &str, role: InstanceRole, events: &EventSender) -> Source {
    let token = TOKENS.fetch_add(1, Ordering::Relaxed);
    sources().lock().unwrap().push(SourceInstance {
        token,
        plugin: plugin.into(),
        role,
        events: events.clone(),
    });
    Source { token }
}

impl Drop for Source {
    fn drop(&mut self) {
        sources()
            .lock()
            .unwrap()
            .retain(|source| source.token != self.token);
    }
}

/// The instance that answers for each source plugin, by plugin id: the service, else the window
/// with the smallest id — never the asking instance (its token).
fn answering(asker: Option<u64>) -> BTreeMap<Arc<str>, EventSender> {
    let mut chosen: BTreeMap<Arc<str>, (u8, u64, EventSender)> = BTreeMap::new();
    for source in sources().lock().unwrap().iter() {
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

impl wit::Host for State {
    fn find(&mut self, query: Query) -> Result<Vec<Task>, String> {
        let host = &mut self.host;
        if !host.entry.manifest.permissions.tasks {
            return Err(missing("ask for the user's tasks", "tasks = true"));
        }
        let asker = host.task_source.as_ref().map(|source| source.token);
        let sources = answering(asker);
        if sources.is_empty() {
            return Err("No plugin gives tasks".into());
        }
        static IDS: AtomicU64 = AtomicU64::new(1);
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
        // In the sources' order; the failures only when no source gave anything.
        let mut tasks = Vec::new();
        let mut errors = Vec::new();
        let mut answered = false;
        for plugin in &asked {
            match got.remove(plugin) {
                Some(Ok(found)) => {
                    answered = true;
                    tasks.extend(found);
                }
                Some(Err(error)) => errors.push(format!("{plugin}: {error}")),
                None => {}
            }
        }
        if answered {
            return Ok(tasks);
        }
        if errors.is_empty() {
            Err("No plugin with tasks answered in 10 seconds".into())
        } else {
            Err(errors.join("; "))
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
            None => host.warn(&format!("tasks.respond: the query {id} is gone")),
        }
    }
}
