//! WebAssembly grammars (stage 8.3): tree-sitter grammars compiled to WebAssembly (`tree-sitter build
//! --wasm`) that plugins ship, run by tree-sitter's `wasm` feature on wasmtime.
//!
//! - One engine for the process ([`engine`]): it compiles a grammar's functions on several threads
//!   and, once the application names a folder ([`set_wasm_cache_dir`]), keeps compiled grammars on
//!   disk — a cached grammar loads in ~1–6 ms instead of 4–50 ms and takes a quarter of the memory.
//! - A [`Language`] loads its module once ([`load`]): a loader store compiles it, and the loaded
//!   grammar stays usable after the store is gone.
//! - A parser of a WebAssembly grammar has a store of its own ([`new_store`]): it instantiates the
//!   grammar on first use (microseconds) and moves between threads with the parser. A grammar is
//!   never loaded into a store twice: a reloaded plugin makes a new [`Language`], and with it new
//!   parsers (a store keeps every module loaded into it until it is dropped).
//! - A grammar's external scanner is a plugin's code. A crash in it is caught: the parse fails, the
//!   parser keeps working. A scanner that never returns can't be stopped (tree-sitter gives wasmtime
//!   no deadline), so these grammars are never parsed on the UI thread, every parse of one runs on a
//!   thread of its own ([`crate::ParseJob::spawn`]), and a watchdog ([`Watch`]) marks a grammar
//!   whose parse runs longer than [`PARSE_LIMIT`] as failed: Flux stops parsing it, and the stuck
//!   thread is left behind.

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use tree_sitter::WasmStore;
use tree_sitter::wasmtime as wt;

use crate::language::Language;

/// How long a parse of a WebAssembly grammar may run before the grammar counts as stuck. Far above
/// any honest parse: a 10 MB file (the largest Flux highlights) parses in about a second.
pub const PARSE_LIMIT: Duration = Duration::from_secs(5);

/// The folder of compiled grammars, if the application named one.
static CACHE_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The engine, made when the first WebAssembly grammar loads.
static ENGINE: OnceLock<Result<wt::Engine, String>> = OnceLock::new();

/// Keeps compiled WebAssembly grammars in `dir` between launches. Call it before the first grammar
/// loads: the engine is made once.
pub fn set_wasm_cache_dir(dir: PathBuf) {
    *CACHE_DIR.lock().unwrap() = Some(dir);
}

/// The process's engine for WebAssembly grammars.
fn engine() -> Result<&'static wt::Engine, String> {
    ENGINE
        .get_or_init(|| {
            let mut config = wt::Config::new();
            config.parallel_compilation(true);
            let dir = CACHE_DIR.lock().unwrap().clone();
            if let Some(dir) = dir {
                // Without the cache grammars still load, only slower.
                match cache(dir) {
                    Ok(cache) => {
                        config.cache(Some(cache));
                    }
                    Err(err) => eprintln!("flux: no cache of compiled grammars: {err}"),
                }
            }
            wt::Engine::new(&config).map_err(|err| format!("WebAssembly engine: {err}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn cache(dir: PathBuf) -> Result<wt::Cache, String> {
    std::fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let mut config = wt::CacheConfig::new();
    config.with_directory(dir);
    wt::Cache::new(config).map_err(|err| err.to_string())
}

/// Loads a grammar: `name` is its own name (the module exports `tree_sitter_<name>`). Compiles the
/// module (or takes it from the cache): not on the UI thread.
pub(crate) fn load(name: &str, bytes: &[u8]) -> Result<tree_sitter::Language, String> {
    let mut store = WasmStore::new(engine()?).map_err(|err| err.to_string())?;
    store
        .load_language(name, bytes)
        .map_err(|err| err.to_string())
}

/// A store for a parser of WebAssembly grammars.
pub(crate) fn new_store() -> Result<WasmStore, String> {
    WasmStore::new(engine()?).map_err(|err| err.to_string())
}

/// A parse in progress, watched.
struct Running {
    id: u64,
    deadline: Instant,
    language: Weak<Language>,
}

#[derive(Default)]
struct Watchdog {
    running: Vec<Running>,
    next_id: u64,
    /// The watchdog's thread is there.
    started: bool,
}

static WATCHDOG: Mutex<Watchdog> = Mutex::new(Watchdog {
    running: Vec::new(),
    next_id: 0,
    started: false,
});

/// Wakes the watchdog's thread when a parse starts.
static WAKE: Condvar = Condvar::new();

/// A parse of a WebAssembly grammar under the watchdog: when it outlives its deadline, the language
/// is marked failed. Dropping the guard (the parse returned) takes it off the watch.
pub(crate) struct Watch {
    id: u64,
}

impl Watch {
    pub(crate) fn start(language: &Arc<Language>) -> Watch {
        Self::with_limit(language, PARSE_LIMIT)
    }

    pub(crate) fn with_limit(language: &Arc<Language>, limit: Duration) -> Watch {
        let mut watchdog = WATCHDOG.lock().unwrap();
        let id = watchdog.next_id;
        watchdog.next_id += 1;
        watchdog.running.push(Running {
            id,
            deadline: Instant::now() + limit,
            language: Arc::downgrade(language),
        });
        if !watchdog.started {
            watchdog.started = true;
            std::thread::Builder::new()
                .name("flux-grammar-watchdog".into())
                .spawn(watch)
                .ok();
        }
        WAKE.notify_one();
        Watch { id }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        WATCHDOG
            .lock()
            .unwrap()
            .running
            .retain(|running| running.id != self.id);
    }
}

/// The watchdog's thread: sleeps while nothing is parsed, otherwise wakes at the nearest deadline.
fn watch() {
    let mut watchdog = WATCHDOG.lock().unwrap();
    loop {
        let now = Instant::now();
        let mut overdue = Vec::new();
        watchdog.running.retain(|running| {
            if running.deadline <= now {
                overdue.push(running.language.clone());
                false
            } else {
                true
            }
        });
        for language in overdue.iter().filter_map(Weak::upgrade) {
            language.mark_hung();
            eprintln!(
                "flux: the {} grammar (plugin {}) stopped responding; it is no longer parsed",
                language.name(),
                language.owner()
            );
        }
        let next = watchdog.running.iter().map(|running| running.deadline).min();
        watchdog = match next {
            None => WAKE.wait(watchdog).unwrap(),
            Some(deadline) => {
                let wait = deadline.saturating_duration_since(Instant::now());
                WAKE.wait_timeout(watchdog, wait).unwrap().0
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::{GrammarSource, LanguageConfig, Precedence};

    fn language(name: &str) -> Arc<Language> {
        Arc::new(Language::new(
            "test.watchdog",
            LanguageConfig {
                name: name.into(),
                display_name: name.into(),
                extensions: Vec::new(),
                file_names: Vec::new(),
                aliases: Vec::new(),
                grammar: GrammarSource::Builtin("javascript".into()),
                highlights: String::new(),
                precedence: Precedence::LastPattern,
            },
        ))
    }

    #[test]
    fn a_parse_past_its_limit_marks_the_grammar_failed() {
        let stuck = language("zz-stuck");
        let watch = Watch::with_limit(&stuck, Duration::from_millis(30));
        let fine = language("zz-fine");
        drop(Watch::with_limit(&fine, Duration::from_millis(30)));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !stuck.grammar_failed() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(stuck.grammar_error(), Some("the grammar stopped responding"));
        assert!(!fine.grammar_failed(), "a parse that returned in time is left alone");
        // The stuck parse returns at last: the grammar stays off.
        drop(watch);
        assert!(stuck.grammar_failed());
    }
}
