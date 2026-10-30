//! Flux's own log (part 10.6), as JetBrains' `idea.log`: `~/Library/Logs/Flux/flux.log` — the
//! records of the `log` crate (gpui's warnings and errors among them, Flux's own from Info) and,
//! when nobody reads Flux's stderr (started from Finder, the Dock or `open`: stderr is
//! `/dev/null`), whatever Flux prints there — `eprintln!`, the panic messages. Started from a
//! terminal, or with stderr sent to a file or a pipe (`2> err.log`, the UI scenarios), stderr stays
//! where it was and the records go to both. Help → Show Log in Finder shows this run's log.
//!
//! The logs of the runs before are kept beside it: `flux.1.log` is the previous run's, up to
//! `flux.4.log`. A run that writes more than [`MAX_LOG_BYTES`] moves on to a new file the same way
//! (checked every few seconds), so the logs never take more than a few tens of MB.
//!
//! Two Flux processes at once (a build being tried next to the everyday one): the first one owns
//! `flux.log` — it holds a lock on `.flux.lock` while it runs (the system drops it when the process
//! ends, a crash too); another one writes `flux-<pid>.log`, leaves the first one's files alone, and
//! of the earlier second processes' logs keeps the last few.
//!
//! A panic leaves a crash report next to the log (`crash-<time>.txt`: the version, the Mac, the
//! thread, the message, where, the stack and the log's last lines) and a line in the log. A crash
//! of an earlier run is offered when the first window of the next launch is up — a notification
//! "Flux quit unexpectedly" with Report a Bug… (`feedback::report_bug`, the report filled in) and
//! Show in Finder — as IDE Internal Errors of JetBrains IDEs; a panic Flux survives (a worker
//! thread's) is offered at once as "Flux ran into an error". An offered report gets a hidden marker
//! (`.crash-<time>.txt.offered`, created atomically: two Flux processes don't both offer it); the
//! last [`KEEP_CRASHES`] reports are kept.
//!
//! `FLUX_LOG` sets the levels, as `RUST_LOG` does, simpler: `debug` (everything), `gpui=info`,
//! `flux_lsp=trace,warn`. `FLUX_LOG_DIR` puts the logs elsewhere; a UI scenario without it logs
//! into a fresh temporary folder (as its plugins live in one), and `FLUX_SCENARIO_PANIC` makes it
//! crash: `1` — on the main thread a moment after the start (Flux quits), `thread` — on a worker
//! thread (Flux survives).

use std::backtrace::Backtrace;
use std::cell::RefCell;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{Action, App, Context, Window, actions};
use log::{Level, LevelFilter, Log, Metadata, Record};

use crate::feedback::{self, SystemInfo};
use crate::i18n::tr;
use crate::notifications::Notification;
use crate::workspace::Workspace;

actions!(
    help,
    [
        /// Help → Show Log in Finder: the logs' folder with `flux.log` selected.
        ShowLog,
    ]
);

/// Report a Bug… of a crash's notification: the bug form with the crash report's facts. Not in
/// the palette (it carries the report).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = help, no_json)]
pub struct ReportCrash {
    pub file: PathBuf,
}

/// Show in Finder of a crash's notification: the report selected in Finder.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = help, no_json)]
pub struct RevealCrash {
    pub file: PathBuf,
}

/// A crash of an earlier run: the report a panic left in the logs' folder.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Crash {
    /// The panic's message.
    pub message: String,
    /// Where it happened: `file:line:column`.
    pub location: String,
    /// The thread that panicked: "main" and the like.
    pub thread: String,
    /// The stack, as Rust prints it.
    pub backtrace: String,
    /// The version of the Flux that crashed (`feedback::build_version`): it may differ from this
    /// one.
    pub version: String,
    /// When, in local time: "2026-10-15 14:03:12".
    pub time: String,
    /// The report's file.
    pub file: PathBuf,
}

/// The log of the run that owns the folder, and the stem of its older files.
const MAIN_STEM: &str = "flux";
/// How many logs are kept: this run's and the four before it.
const KEEP_LOGS: usize = 5;
/// A second Flux keeps its log and the one it moved on from.
const KEEP_SECOND: usize = 2;
/// The logs of earlier second processes kept (the newest).
const KEEP_SECOND_RUNS: usize = 3;
/// The lock the Flux that owns `flux.log` holds while it runs.
const LOCK: &str = ".flux.lock";
/// A log moves on to a new file past this size (as `idea.log` does at 10 MB).
const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
/// How often the log's size is checked.
const SIZE_CHECK: Duration = Duration::from_secs(5);
/// Crash reports kept.
const KEEP_CRASHES: usize = 10;
/// At most this many crash reports per run: a panic in a loop doesn't fill the disk.
const MAX_CRASHES_PER_RUN: usize = 5;
/// The bounds of a report's parts, in bytes.
const MAX_MESSAGE: usize = 16 * 1024;
const MAX_BACKTRACE: usize = 256 * 1024;
/// How much of the log's end goes into a crash report.
const LOG_TAIL: u64 = 8 * 1024;
/// The machinery of a panic and of this hook is cut from the top of a report's stack: looked for
/// in this many first frames.
const MACHINERY_FRAMES: usize = 40;
/// The notice of an earlier crash waits a moment after the first window opens: the window is
/// drawn, the card comes in.
const STARTUP_OFFER_DELAY: Duration = Duration::from_millis(800);
/// A panic Flux survives is offered after this long: a crash would have ended Flux by then (the
/// next launch offers it instead).
const LIVE_OFFER_DELAY: Duration = Duration::from_secs(1);
/// The first line of every crash report.
const REPORT_TITLE: &str = "Flux crash report";
const MESSAGE: &str = "Message:";
const BACKTRACE: &str = "Backtrace:";
const LOG_LINES: &str = "Log (its last lines before the crash):";

/// Where this run logs.
struct Sink {
    dir: PathBuf,
    /// `flux.log`, or `flux-<pid>.log` for a second Flux.
    path: PathBuf,
    /// "flux" / "flux-<pid>": the files it moves on from are `<stem>.1.log`…
    stem: String,
    keep: usize,
    file: Mutex<File>,
    /// fd 2 points at the log: stderr went nowhere.
    stderr: bool,
    /// The records also go to stderr: someone reads it (a terminal, a file, a pipe).
    echo: bool,
    /// The lock on [`LOCK`] of the Flux that owns `flux.log`, held while it runs; `None` for a
    /// second Flux.
    lock: Option<File>,
}

static DIR: OnceLock<PathBuf> = OnceLock::new();
static SINK: OnceLock<Sink> = OnceLock::new();
/// The Mac's facts, read once at the start: the log's header and the crash reports (a panic hook
/// shouldn't read them anew).
static MACHINE: OnceLock<SystemInfo> = OnceLock::new();
static LOGGER: Logger = Logger {
    filter: OnceLock::new(),
};
/// A panic's report, to the running Flux: offered if Flux survives it.
static LIVE: OnceLock<mpsc::UnboundedSender<PathBuf>> = OnceLock::new();
/// Crash reports written by this run.
static CRASHES: AtomicUsize = AtomicUsize::new(0);
/// The home folder, read at the start: a report says `~` instead (it goes to a public issue).
static HOME: OnceLock<String> = OnceLock::new();

thread_local! {
    /// This thread's last crash report: a panic that only follows it (unwinding into a function
    /// that can't unwind — Flux quits) completes it instead of a report of its own.
    static LAST_REPORT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// The logs' folder: `~/Library/Logs/Flux` (the plugins' logs are in `plugins/` inside), or
/// `FLUX_LOG_DIR`.
pub fn dir() -> PathBuf {
    DIR.get().cloned().unwrap_or_else(default_dir)
}

/// This run's log: `flux.log` in [`dir`] (`flux-<pid>.log` when another Flux owns that one).
pub fn file() -> PathBuf {
    match SINK.get() {
        Some(sink) => sink.path.clone(),
        None => dir().join(format!("{MAIN_STEM}.log")),
    }
}

/// First thing in `main`: keeps the previous runs' logs, opens this run's, sends the `log` records
/// and stderr there, and sets the panic hook that writes crash reports.
pub fn start() {
    let dir = start_dir();
    let _ = DIR.set(dir.clone());
    if let Some(home) = flux_platform::paths::home().map(std::ffi::OsString::from) {
        let _ = HOME.set(home.to_string_lossy().trim_end_matches('/').to_string());
    }
    let machine = MACHINE.get_or_init(feedback::machine);
    let spec = std::env::var("FLUX_LOG").unwrap_or_default();
    let filter = Filter::parse(&spec);
    let max = filter.max();
    let _ = LOGGER.filter.set(filter);
    match Sink::open(&dir) {
        Ok(sink) => {
            let owner = sink.lock.is_some();
            let second = (!owner).then(|| sink.path.clone());
            if SINK.set(sink).is_ok() {
                if owner {
                    prune_crashes(&dir, KEEP_CRASHES);
                }
                watch_size();
            }
            write_header(machine, &spec, second.as_deref());
        }
        Err(error) => eprintln!("flux: no log in {}: {error}", dir.display()),
    }
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(max);
    }
    set_panic_hook();
}

/// A crash report that hasn't been offered to the user yet, the newest; it is marked as offered
/// (and so are older ones still waiting: one notice for them all).
pub fn take_unreported_crash() -> Option<Crash> {
    take_unreported_in(&dir())
}

pub fn init(cx: &mut App) {
    cx.on_action(|_: &ShowLog, cx| show_log(cx));
    cx.on_action(|action: &ReportCrash, cx| {
        let crash = read_report(&action.file);
        feedback::report_bug(crash.as_ref(), cx);
    });
    cx.on_action(|action: &RevealCrash, cx| reveal(&action.file, cx));

    // A crash of an earlier run: once the first window is up.
    if let Some(crash) = take_unreported_crash() {
        let pending = Rc::new(RefCell::new(Some(crash)));
        cx.observe_new(
            move |_: &mut Workspace, _: Option<&mut Window>, cx: &mut Context<Workspace>| {
                let Some(crash) = pending.borrow_mut().take() else {
                    return;
                };
                cx.spawn(async move |workspace, cx| {
                    cx.background_executor().timer(STARTUP_OFFER_DELAY).await;
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.notify(notice(&crash, true), cx)
                        })
                        .ok();
                })
                .detach();
            },
        )
        .detach();
    }

    // A panic of this run that Flux survives.
    let (sender, mut reports) = mpsc::unbounded();
    if LIVE.set(sender).is_ok() {
        cx.spawn(async move |cx| {
            while let Some(path) = reports.next().await {
                cx.background_executor().timer(LIVE_OFFER_DELAY).await;
                if !mark_offered(&path) {
                    continue;
                }
                if let Some(crash) = read_report(&path) {
                    cx.update(|cx| offer(notice(&crash, false), cx)).ok();
                }
            }
        })
        .detach();
    }

    #[cfg(feature = "scenario")]
    scenario_panic(cx);
}

// --- The log ---

/// The folder `start` logs in: `FLUX_LOG_DIR`; a UI scenario's own temporary one; the user's.
fn start_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FLUX_LOG_DIR").filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir);
    }
    #[cfg(feature = "scenario")]
    if std::env::var_os("FLUX_SCENARIO").is_some() {
        return std::env::temp_dir().join(format!("flux-scenario-logs-{}", std::process::id()));
    }
    default_dir()
}

fn default_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FLUX_LOG_DIR").filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir);
    }
    flux_platform::paths::logs_dir()
        .unwrap_or_else(|| flux_platform::paths::logs_dir_in(&std::env::temp_dir()))
}

impl Sink {
    /// Takes the folder's `flux.log` if no other Flux has it (the older logs move one step), or
    /// `flux-<pid>.log`; stderr that goes nowhere goes into it.
    fn open(dir: &Path) -> io::Result<Sink> {
        let mut sink = Sink::open_in(dir)?;
        if flux_platform::process::stderr_unread()
            && flux_platform::process::redirect_stderr(&lock(&sink.file))
        {
            sink.stderr = true;
            sink.echo = false;
        }
        Ok(sink)
    }

    /// [`Sink::open`] without touching stderr: the records go to the log and to stderr.
    fn open_in(dir: &Path) -> io::Result<Sink> {
        fs::create_dir_all(dir)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(LOCK))?;
        // An exclusive lock that doesn't wait (`flock`, `LockFileEx`).
        let owner = lock.try_lock().is_ok();
        let (stem, keep) = if owner {
            rotate(dir, MAIN_STEM, KEEP_LOGS);
            (MAIN_STEM.to_string(), KEEP_LOGS)
        } else {
            prune_second_logs(dir, KEEP_SECOND_RUNS);
            (format!("{MAIN_STEM}-{}", std::process::id()), KEEP_SECOND)
        };
        let path = dir.join(format!("{stem}.log"));
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Sink {
            dir: dir.to_path_buf(),
            path,
            stem,
            keep,
            file: Mutex::new(file),
            stderr: false,
            echo: true,
            lock: owner.then_some(lock),
        })
    }

    /// Past `max` bytes the log moves on to a new file (the older ones one step down), with the
    /// header again. Whether it did.
    fn move_on_past(&self, max: u64) -> bool {
        let mut file = lock(&self.file);
        if file.metadata().map_or(0, |metadata| metadata.len()) <= max {
            return false;
        }
        rotate(&self.dir, &self.stem, self.keep);
        let Ok(next) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        else {
            return false;
        };
        if self.stderr {
            flux_platform::process::redirect_stderr(&next);
        }
        *file = next;
        let line = header_line(
            SystemTime::now(),
            &format!(
                "the log went past {} MB: it goes on here, the earlier part is {}.1.log",
                max / (1024 * 1024),
                self.stem
            ),
        );
        let _ = file.write_all(line.as_bytes());
        true
    }
}

/// `<stem>.log` → `<stem>.1.log` → … → `<stem>.<keep - 1>.log`; the oldest goes. Missing files are
/// skipped; errors are ignored (the log is a convenience, not a reason not to start).
fn rotate(dir: &Path, stem: &str, keep: usize) {
    let name = |n: usize| match n {
        0 => dir.join(format!("{stem}.log")),
        n => dir.join(format!("{stem}.{n}.log")),
    };
    if keep < 2 {
        let _ = fs::remove_file(name(0));
        return;
    }
    let _ = fs::remove_file(name(keep - 1));
    for n in (0..keep - 1).rev() {
        let _ = fs::rename(name(n), name(n + 1));
    }
}

/// The logs of second Flux processes that have ended: the newest `keep` stay.
fn prune_second_logs(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    // pid → the newest change of its files.
    let mut runs: Vec<(u32, SystemTime)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = second_log_pid(&name) else {
            continue;
        };
        let changed = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        match runs.iter_mut().find(|(known, _)| *known == pid) {
            Some((_, newest)) => *newest = (*newest).max(changed),
            None => runs.push((pid, changed)),
        }
    }
    runs.retain(|(pid, _)| !process_alive(*pid));
    runs.sort_by_key(|run| std::cmp::Reverse(run.1));
    for (pid, _) in runs.into_iter().skip(keep) {
        let stem = format!("{MAIN_STEM}-{pid}");
        rotate(dir, &stem, 1);
        for n in 1..KEEP_SECOND {
            let _ = fs::remove_file(dir.join(format!("{stem}.{n}.log")));
        }
    }
}

/// `flux-123.log`, `flux-123.1.log` → 123.
fn second_log_pid(name: &str) -> Option<u32> {
    let rest = name.strip_prefix(MAIN_STEM)?.strip_prefix('-')?;
    let digits = rest.split('.').next()?;
    let tail = &rest[digits.len()..];
    let numbered = tail
        .strip_prefix('.')
        .and_then(|tail| tail.strip_suffix(".log"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    (tail == ".log" || numbered).then(|| digits.parse().ok())?
}

fn process_alive(pid: u32) -> bool {
    flux_platform::process::alive(pid)
}

/// Every few seconds: a log past [`MAX_LOG_BYTES`] moves on to a new file.
fn watch_size() {
    let _ = std::thread::Builder::new()
        .name("flux-log".into())
        .spawn(|| {
            loop {
                std::thread::sleep(SIZE_CHECK);
                if let Some(sink) = SINK.get() {
                    sink.move_on_past(MAX_LOG_BYTES);
                }
            }
        });
}

/// The first lines of a run's log: the version and the Mac, the levels if set, and whose
/// `flux.log` it isn't.
fn write_header(machine: &SystemInfo, spec: &str, second: Option<&Path>) {
    let now = SystemTime::now();
    let mut text = header_line(
        now,
        &format!(
            "Flux {} started, pid {}",
            machine.version,
            std::process::id()
        ),
    );
    let mac = [machine.macos.as_str(), machine.hardware.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    if !mac.is_empty() {
        text.push_str(&header_line(now, &mac));
    }
    if !spec.is_empty() {
        text.push_str(&header_line(now, &format!("FLUX_LOG={spec}")));
    }
    if let Some(path) = second {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        text.push_str(&header_line(
            now,
            &format!("another Flux writes {MAIN_STEM}.log: this one writes {name}"),
        ));
    }
    write_line(&text);
}

fn header_line(at: SystemTime, message: &str) -> String {
    line(at, Level::Info, "flux", format_args!("{message}"))
}

/// A record's line: `2026-10-15 14:03:12.345 WARN  gpui::window: …`.
fn line(at: SystemTime, level: Level, target: &str, message: fmt::Arguments) -> String {
    format!("{} {level:<5} {target}: {message}\n", timestamp(at))
}

/// Into the log (and to stderr when someone reads it); to stderr only when there's no log.
fn write_line(text: &str) {
    let Some(sink) = SINK.get() else {
        let _ = io::stderr().write_all(text.as_bytes());
        return;
    };
    let _ = lock(&sink.file).write_all(text.as_bytes());
    if sink.echo {
        let _ = io::stderr().write_all(text.as_bytes());
    }
}

/// [`write_line`] from a panic hook: the log's lock may be held — by the very thread that panicked
/// while writing — so a busy lock means a descriptor of its own.
fn write_line_in_panic(text: &str) {
    let Some(sink) = SINK.get() else {
        let _ = io::stderr().write_all(text.as_bytes());
        return;
    };
    match sink.file.try_lock() {
        Ok(mut file) => {
            let _ = file.write_all(text.as_bytes());
        }
        Err(_) => {
            if let Ok(mut file) = OpenOptions::new().append(true).open(&sink.path) {
                let _ = file.write_all(text.as_bytes());
            }
        }
    }
    if sink.echo {
        let _ = io::stderr().write_all(text.as_bytes());
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The `log` crate's records into the log.
struct Logger {
    filter: OnceLock<Filter>,
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.filter
            .get()
            .is_some_and(|filter| metadata.level() <= filter.level(metadata.target()))
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            write_line(&line(
                SystemTime::now(),
                record.level(),
                record.target(),
                *record.args(),
            ));
        }
    }

    fn flush(&self) {}
}

/// The levels: Info for Flux's own records (`flux`, `flux_lsp`…), Warn for the rest (gpui,
/// wasmtime…); `FLUX_LOG` changes them.
#[derive(Debug, Clone, PartialEq)]
struct Filter {
    flux: LevelFilter,
    other: LevelFilter,
    /// `target=level`: a target and the ones under it (`gpui` — `gpui::window` too).
    rules: Vec<(String, LevelFilter)>,
}

impl Filter {
    /// `FLUX_LOG`: comma-separated `level` (for everything) and `target=level`; what doesn't parse
    /// is skipped.
    fn parse(spec: &str) -> Filter {
        let mut filter = Filter {
            flux: LevelFilter::Info,
            other: LevelFilter::Warn,
            rules: Vec::new(),
        };
        for part in spec
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            match part.split_once('=') {
                Some((target, level)) => {
                    if let Ok(level) = level.trim().parse::<LevelFilter>() {
                        filter.rules.push((target.trim().to_string(), level));
                    }
                }
                None => {
                    if let Ok(level) = part.parse::<LevelFilter>() {
                        filter.flux = level;
                        filter.other = level;
                    }
                }
            }
        }
        filter
    }

    fn level(&self, target: &str) -> LevelFilter {
        let rule = self
            .rules
            .iter()
            .filter(|(prefix, _)| {
                target == prefix
                    || target
                        .strip_prefix(prefix.as_str())
                        .is_some_and(|rest| rest.starts_with("::"))
            })
            .max_by_key(|(prefix, _)| prefix.len());
        match rule {
            Some((_, level)) => *level,
            None if is_flux(target) => self.flux,
            None => self.other,
        }
    }

    /// The most verbose level any target gets: `log` skips the records past it cheaply.
    fn max(&self) -> LevelFilter {
        self.rules
            .iter()
            .map(|(_, level)| *level)
            .chain([self.flux, self.other])
            .max()
            .unwrap_or(LevelFilter::Warn)
    }
}

/// Flux's own crates: the app (`flux`) and `flux_*`.
fn is_flux(target: &str) -> bool {
    let krate = target.split("::").next().unwrap_or_default();
    krate == "flux" || krate.starts_with("flux_")
}

// --- Time ---

/// Local time of `at`: (year, month, day, hour, minute, second, millisecond).
fn local(at: SystemTime) -> (i64, u32, u32, u32, u32, u32, u32) {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = since.as_secs() as i64;
    let local = seconds + i64::from(flux_platform::time::local_utc_offset(seconds));
    let (year, month, day, hour, minute, second) = flux_platform::time::civil(local);
    (
        year,
        month,
        day,
        hour,
        minute,
        second,
        since.subsec_millis(),
    )
}

/// "2026-10-15 14:03:12.345".
fn timestamp(at: SystemTime) -> String {
    let (year, month, day, hour, minute, second, milli) = local(at);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{milli:03}")
}

/// "2026-10-15 14:03:12": a report's time.
fn report_time(at: SystemTime) -> String {
    let (year, month, day, hour, minute, second, _) = local(at);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

/// "2026-10-15-14-03-12": a report's file name.
fn file_stamp(at: SystemTime) -> String {
    let (year, month, day, hour, minute, second, _) = local(at);
    format!("{year:04}-{month:02}-{day:02}-{hour:02}-{minute:02}-{second:02}")
}

// --- Crash reports ---

fn set_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // The usual message first (into the log when stderr goes there), then the report.
        previous(info);
        on_panic(info);
    }));
}

/// Writes the crash report of a panic. Nothing here may panic: a panic in the hook aborts without
/// a word; the allocations are bounded (the message, the stack, the log's tail).
fn on_panic(info: &PanicHookInfo) {
    let now = SystemTime::now();
    let message = clip(info.payload_as_str().unwrap_or("Box<dyn Any>"), MAX_MESSAGE);
    let location = info
        .location()
        .map(|location| {
            format!(
                "{}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            )
        })
        .unwrap_or_default();
    // Unwinding reached a function that can't unwind (gpui's callbacks from macOS are such): the
    // runtime panics again and ends Flux. That belongs to this thread's report.
    if follows_a_panic(message) {
        let last = LAST_REPORT
            .try_with(|last| last.try_borrow().ok().and_then(|last| last.clone()))
            .ok()
            .flatten();
        if let Some(report) = last {
            if let Ok(mut file) = OpenOptions::new().append(true).open(&report) {
                let _ = write!(file, "\nThen: {message} ({location}): Flux quit.\n");
            }
            return;
        }
    }
    if CRASHES.fetch_add(1, Ordering::Relaxed) >= MAX_CRASHES_PER_RUN {
        return;
    }
    let Some(dir) = DIR.get() else {
        return;
    };
    let machine = MACHINE.get().cloned().unwrap_or_default();
    let home = HOME.get().map_or("", String::as_str);
    let thread = std::thread::current();
    let crash = Crash {
        message: tilde(message, home),
        location: tilde(&location, home),
        thread: thread
            .name()
            .map_or_else(|| format!("{:?}", thread.id()), str::to_string),
        backtrace: tilde(
            &trim_backtrace(&Backtrace::force_capture().to_string()),
            home,
        ),
        version: machine.version.clone(),
        time: report_time(now),
        file: PathBuf::new(),
    };
    let tail = SINK
        .get()
        .map(|sink| tilde(&log_tail(&sink.path), home))
        .unwrap_or_default();
    let Some(path) = write_report(dir, now, &report_text(&crash, &machine, &tail)) else {
        return;
    };
    let _ = LAST_REPORT.try_with(|last| {
        if let Ok(mut last) = last.try_borrow_mut() {
            *last = Some(path.clone());
        }
    });
    write_line_in_panic(&line(
        now,
        Level::Error,
        "flux",
        format_args!(
            "panicked on {} at {}: {} — crash report: {}",
            crash.thread,
            crash.location,
            first_line(&crash.message),
            path.display()
        ),
    ));
    if let Some(live) = LIVE.get() {
        let _ = live.unbounded_send(path);
    }
}

/// The runtime's panic after a panic: a function that can't unwind was reached, or a destructor
/// panicked while unwinding.
fn follows_a_panic(message: &str) -> bool {
    message.starts_with("panic in a function that cannot unwind")
        || message.starts_with("panic in a destructor during cleanup")
}

/// `/Users/name/…` → `~/…`: the user's name stays out of a report meant for a public issue.
fn tilde(text: &str, home: &str) -> String {
    if home.len() < 2 {
        return text.to_string();
    }
    text.replace(&format!("{home}/"), "~/")
}

/// At most `max` bytes of `text`, cut at a character's boundary.
fn clip(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

/// The stack without its top: the frames of the backtrace's capture, this hook and the panic's
/// machinery, down to where the panic happened. Frames keep their numbers; past [`MAX_BACKTRACE`]
/// the rest is cut.
fn trim_backtrace(text: &str) -> String {
    // A frame: "  12: name" and its "at file:line" lines.
    let mut frames: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let frame_name = trimmed
            .split_once(": ")
            .filter(|(number, _)| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
            .map(|(_, name)| name.to_string());
        match (frame_name, frames.last_mut()) {
            (Some(name), _) => frames.push((name, format!("{line}\n"))),
            (None, Some((_, block))) => {
                block.push_str(line);
                block.push('\n');
            }
            (None, None) => frames.push((String::new(), format!("{line}\n"))),
        }
    }
    // The machinery is the top of the stack, up to the frame that panicked; the frames under
    // that one (a thread's start, `catch_unwind`) stay, whatever they are.
    let mut skip = frames
        .iter()
        .take(MACHINERY_FRAMES)
        .take_while(|(name, _)| is_machinery(name))
        .count();
    if skip == frames.len() {
        skip = 0;
    }
    let mut trimmed = String::new();
    for (_, block) in &frames[skip..] {
        if trimmed.len() + block.len() > MAX_BACKTRACE {
            trimmed.push_str("   …\n");
            break;
        }
        trimmed.push_str(block);
    }
    let trimmed = trimmed.trim_end().to_string();
    if trimmed.is_empty() {
        clip(text, MAX_BACKTRACE).trim_end().to_string()
    } else {
        trimmed
    }
}

/// A frame of the backtrace's capture, this hook or the panic's machinery: `std::panicking::…`,
/// `<std::backtrace::Backtrace>::create` (an inherent method is named in angle brackets).
fn is_machinery(frame: &str) -> bool {
    const PREFIXES: [&str; 11] = [
        "std::backtrace",
        "std::panicking",
        "std::sys::backtrace::__rust_end_short_backtrace",
        "core::panicking",
        "core::result::unwrap_failed",
        "core::option::unwrap_failed",
        "core::option::expect_failed",
        "rust_begin_unwind",
        "__rustc::rust_begin_unwind",
        "flux::app_log::on_panic",
        "flux::app_log::set_panic_hook",
    ];
    // The hook itself is a boxed closure: `<alloc::boxed::Box<dyn Fn(&PanicHookInfo)…>>::call`.
    if frame.starts_with("<alloc::boxed::Box<") && frame.contains("PanicHookInfo") {
        return true;
    }
    let name = frame.strip_prefix('<').unwrap_or(frame);
    PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

/// The last lines of the log: up to [`LOG_TAIL`] bytes, from a line's start.
fn log_tail(path: &Path) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let length = file.metadata().map_or(0, |metadata| metadata.len());
    let start = length.saturating_sub(LOG_TAIL);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::with_capacity((length - start) as usize);
    if file.take(LOG_TAIL).read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let text = match (start > 0, text.find('\n')) {
        (true, Some(newline)) => &text[newline + 1..],
        _ => &text[..],
    };
    text.trim_end().to_string()
}

/// The report's text: the facts as `Key: value`, then the sections — the message and the log's
/// lines indented, the stack as Rust prints it.
fn report_text(crash: &Crash, machine: &SystemInfo, log_tail: &str) -> String {
    let mut text = format!("{REPORT_TITLE}\n\n");
    for (key, value) in [
        ("Time", &crash.time),
        ("Version", &crash.version),
        (crate::feedback::SYSTEM_NAME, &machine.macos),
        ("Hardware", &machine.hardware),
        ("Thread", &crash.thread),
        ("Location", &crash.location),
    ] {
        text.push_str(&format!("{key}: {value}\n"));
    }
    text.push_str(&format!("\n{MESSAGE}\n{}\n", indent(&crash.message)));
    text.push_str(&format!("\n{BACKTRACE}\n{}\n", crash.backtrace));
    if !log_tail.is_empty() {
        text.push_str(&format!("\n{LOG_LINES}\n{}\n", indent(log_tail)));
    }
    text
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A report written by [`report_text`] (the log's lines and what followed aren't kept).
fn parse_report(text: &str, file: PathBuf) -> Option<Crash> {
    let mut lines = text.lines();
    if lines.next()? != REPORT_TITLE {
        return None;
    }
    let mut crash = Crash {
        file,
        ..Crash::default()
    };
    let mut section: Option<&str> = None;
    let mut message: Vec<&str> = Vec::new();
    let mut backtrace: Vec<&str> = Vec::new();
    for line in lines {
        if [MESSAGE, BACKTRACE, LOG_LINES].contains(&line) {
            section = Some(line);
            continue;
        }
        if line.starts_with("Then: ") {
            section = None;
            continue;
        }
        match section {
            None => {
                let Some((key, value)) = line.split_once(": ") else {
                    continue;
                };
                let value = value.to_string();
                match key {
                    "Time" => crash.time = value,
                    "Version" => crash.version = value,
                    "Thread" => crash.thread = value,
                    "Location" => crash.location = value,
                    _ => {}
                }
            }
            Some(MESSAGE) => message.push(line.strip_prefix("    ").unwrap_or(line)),
            Some(BACKTRACE) => backtrace.push(line),
            _ => {}
        }
    }
    crash.message = message.join("\n").trim_end().to_string();
    crash.backtrace = backtrace.join("\n").trim_end().to_string();
    Some(crash)
}

fn read_report(path: &Path) -> Option<Crash> {
    parse_report(&fs::read_to_string(path).ok()?, path.to_path_buf())
}

/// `crash-<time>.txt` (with `-1`, `-2`… when that second has one already), never overwritten.
fn write_report(dir: &Path, at: SystemTime, text: &str) -> Option<PathBuf> {
    let stamp = file_stamp(at);
    for n in 0..100 {
        let name = match n {
            0 => format!("crash-{stamp}.txt"),
            n => format!("crash-{stamp}-{n}.txt"),
        };
        let path = dir.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => return file.write_all(text.as_bytes()).ok().map(|()| path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

/// The folder's crash reports, newest first.
fn crash_reports(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut reports: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("crash-") && name.ends_with(".txt")
        })
        .map(|entry| {
            let changed = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(UNIX_EPOCH);
            (changed, entry.path())
        })
        .collect();
    reports.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    reports.into_iter().map(|(_, path)| path).collect()
}

/// The hidden marker of an offered report: `.crash-<time>.txt.offered`.
fn offered_marker(report: &Path) -> PathBuf {
    let name = report.file_name().unwrap_or_default().to_string_lossy();
    report.with_file_name(format!(".{name}.offered"))
}

/// Marks a report as offered; `false` if it already was (by this Flux or another one).
fn mark_offered(report: &Path) -> bool {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(offered_marker(report))
        .is_ok()
}

fn take_unreported_in(dir: &Path) -> Option<Crash> {
    let mut newest = None;
    for report in crash_reports(dir) {
        if mark_offered(&report) && newest.is_none() {
            newest = Some(report);
        }
    }
    read_report(&newest?)
}

/// Keeps the newest `keep` reports; the markers of the gone ones go too.
fn prune_crashes(dir: &Path, keep: usize) {
    let reports = crash_reports(dir);
    for report in reports.iter().skip(keep) {
        let _ = fs::remove_file(report);
        let _ = fs::remove_file(offered_marker(report));
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let report = name
            .strip_prefix('.')
            .and_then(|name| name.strip_suffix(".offered"));
        if let Some(report) = report
            && !dir.join(report).exists()
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

// --- In the app ---

/// The notice of a crash: an error card that stays, with Report a Bug… and Show in Finder.
/// `quit`: Flux ended with it (an earlier run's crash), else Flux survived it.
fn notice(crash: &Crash, quit: bool) -> Notification {
    let title = if quit {
        tr("Flux quit unexpectedly")
    } else {
        tr("Flux ran into an error")
    };
    let time = crash.time.get(..16).unwrap_or(&crash.time);
    let message = first_line(&crash.message);
    let mut body = clip(message, 240).to_string();
    if body.len() < message.len() {
        body.push('…');
    }
    Notification::error(title)
        .body(format!("{time} · {body}"))
        .action(
            tr("Report a Bug…"),
            ReportCrash {
                file: crash.file.clone(),
            },
        )
        .action(
            tr("Show in Finder"),
            RevealCrash {
                file: crash.file.clone(),
            },
        )
}

/// In the active Flux window, or the first one.
fn offer(notification: Notification, cx: &mut App) {
    let window = cx
        .active_window()
        .into_iter()
        .chain(cx.windows())
        .find_map(|window| window.downcast::<Workspace>());
    if let Some(window) = window {
        window
            .update(cx, |workspace, _, cx| workspace.notify(notification, cx))
            .ok();
    }
}

/// Help → Show Log in Finder: this run's log selected (the folder and an empty log made if
/// they're missing).
fn show_log(cx: &mut App) {
    let path = file();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if !path.exists() {
        let _ = File::create(&path);
    }
    reveal(&path, cx);
}

/// Finder with `path` selected — in a scenario with `FLUX_SCENARIO_OPEN_URL`, a line
/// `reveal <path>` in that file instead (no Finder window over the author's).
fn reveal(path: &Path, cx: &mut App) {
    #[cfg(feature = "scenario")]
    if let Some(file) = std::env::var_os("FLUX_SCENARIO_OPEN_URL") {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(file) {
            let _ = writeln!(file, "reveal {}", path.display());
        }
        return;
    }
    cx.reveal_path(path);
}

/// `FLUX_SCENARIO_PANIC`: `1` — a panic on the main thread a moment after the start (from a gpui
/// task, as a real one: Flux quits); `thread` — on a worker thread (Flux survives it).
#[cfg(feature = "scenario")]
fn scenario_panic(cx: &mut App) {
    let Some(how) = std::env::var("FLUX_SCENARIO_PANIC")
        .ok()
        .filter(|how| !how.is_empty())
    else {
        return;
    };
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(1500))
            .await;
        if how == "thread" {
            let _ = std::thread::Builder::new()
                .name("flux-scenario-worker".into())
                .spawn(|| panic!("FLUX_SCENARIO_PANIC: a worker thread's panic"));
        } else {
            let _: Result<(), _> =
                cx.update(|_| panic!("FLUX_SCENARIO_PANIC: a crash on the main thread"));
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder, removed when the test is over.
    struct Folder(PathBuf);

    impl std::ops::Deref for Folder {
        type Target = Path;

        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl AsRef<Path> for Folder {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn folder(name: &str) -> Folder {
        let dir = std::env::temp_dir().join(format!(
            "flux-app-log-test-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        Folder(dir)
    }

    fn read(path: PathBuf) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn previous_runs_move_one_step_and_the_oldest_goes() {
        let dir = folder("rotate");
        for (name, text) in [
            ("flux.log", "run 5"),
            ("flux.1.log", "run 4"),
            ("flux.2.log", "run 3"),
            ("flux.3.log", "run 2"),
            ("flux.4.log", "run 1"),
        ] {
            fs::write(dir.join(name), text).unwrap();
        }
        rotate(&dir, "flux", 5);
        assert!(!dir.join("flux.log").exists());
        assert_eq!(read(dir.join("flux.1.log")), "run 5");
        assert_eq!(read(dir.join("flux.4.log")), "run 2");
        assert!(!dir.join("flux.5.log").exists());
        // Gaps are fine: the first run has only flux.log.
        let dir = folder("rotate-first");
        fs::write(dir.join("flux.log"), "first").unwrap();
        rotate(&dir, "flux", 5);
        assert_eq!(read(dir.join("flux.1.log")), "first");
        rotate(&dir, "flux", 5);
        assert_eq!(read(dir.join("flux.2.log")), "first");
    }

    #[test]
    fn the_first_flux_owns_the_log_and_a_second_one_writes_its_own() {
        let dir = folder("owner");
        fs::write(dir.join("flux.log"), "the run before").unwrap();
        let first = Sink::open_in(&dir).unwrap();
        assert!(first.lock.is_some());
        assert_eq!(first.path, dir.join("flux.log"));
        assert_eq!(read(dir.join("flux.1.log")), "the run before");
        lock(&first.file).write_all(b"first\n").unwrap();
        // flock is per open file: a second open in this process stands for a second Flux.
        let second = Sink::open_in(&dir).unwrap();
        assert!(second.lock.is_none());
        assert_eq!(
            second.path,
            dir.join(format!("flux-{}.log", std::process::id()))
        );
        // The second one rotated nothing of the first one's.
        assert_eq!(read(dir.join("flux.log")), "first\n");
        assert_eq!(read(dir.join("flux.1.log")), "the run before");
        drop(first);
        let third = Sink::open_in(&dir).unwrap();
        assert!(third.lock.is_some(), "the lock goes with its Flux");
    }

    #[test]
    fn a_log_past_its_size_moves_on_to_a_new_file() {
        let dir = folder("cap");
        let sink = Sink::open_in(&dir).unwrap();
        lock(&sink.file).write_all(&[b'x'; 300]).unwrap();
        assert!(!sink.move_on_past(1000));
        lock(&sink.file).write_all(&[b'y'; 800]).unwrap();
        assert!(sink.move_on_past(1000));
        let earlier = read(dir.join("flux.1.log"));
        assert_eq!(earlier.len(), 1100);
        let now = read(dir.join("flux.log"));
        assert!(now.contains("it goes on here"), "{now}");
        lock(&sink.file).write_all(b"more\n").unwrap();
        assert!(read(dir.join("flux.log")).ends_with("more\n"));
    }

    #[test]
    fn ended_second_processes_keep_their_last_logs() {
        let dir = folder("second");
        let alive = std::process::id();
        // Pids past macOS's pid_max: no such processes.
        let dead = [9_000_001_u32, 9_000_002, 9_000_003, 9_000_004, 9_000_005];
        for (n, pid) in dead.iter().enumerate() {
            let path = dir.join(format!("flux-{pid}.log"));
            fs::write(&path, "x").unwrap();
            let at = UNIX_EPOCH + Duration::from_secs(1_000 + n as u64);
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(at)
                .unwrap();
        }
        let older = dir.join(format!("flux-{}.1.log", dead[0]));
        fs::write(&older, "x").unwrap();
        let at = UNIX_EPOCH + Duration::from_secs(1_000);
        File::options()
            .write(true)
            .open(&older)
            .unwrap()
            .set_modified(at)
            .unwrap();
        fs::write(dir.join(format!("flux-{alive}.log")), "x").unwrap();
        fs::write(dir.join("flux.log"), "x").unwrap();
        prune_second_logs(&dir, 3);
        let left = |pid: u32| dir.join(format!("flux-{pid}.log")).exists();
        assert!(!left(dead[0]) && !left(dead[1]));
        assert!(!dir.join(format!("flux-{}.1.log", dead[0])).exists());
        assert!(left(dead[2]) && left(dead[3]) && left(dead[4]));
        assert!(left(alive), "a running Flux's log stays");
        assert!(dir.join("flux.log").exists());
        assert_eq!(second_log_pid("flux-123.log"), Some(123));
        assert_eq!(second_log_pid("flux-123.1.log"), Some(123));
        assert_eq!(second_log_pid("flux.1.log"), None);
        assert_eq!(second_log_pid("flux-x.log"), None);
        assert_eq!(second_log_pid("flux-123.log.bak"), None);
    }

    #[test]
    fn a_record_is_a_line_with_the_time_level_and_target() {
        let at = UNIX_EPOCH + Duration::from_millis(1_760_526_192_345);
        let text = line(
            at,
            Level::Warn,
            "gpui::window",
            format_args!("no {}", "frame"),
        );
        let (time, rest) = text.split_at(23);
        let bytes = time.as_bytes();
        assert!(
            bytes[4] == b'-'
                && bytes[7] == b'-'
                && bytes[10] == b' '
                && bytes[13] == b':'
                && bytes[19] == b'.',
            "{text}"
        );
        assert!(time.ends_with(".345"), "{text}");
        assert_eq!(rest, " WARN  gpui::window: no frame\n");
        let text = line(at, Level::Error, "flux", format_args!("x"));
        assert!(text.ends_with(" ERROR flux: x\n"), "{text}");
    }

    #[test]
    fn levels_are_info_for_flux_and_warn_for_the_rest_unless_flux_log_says() {
        let filter = Filter::parse("");
        assert_eq!(filter.level("flux::app_log"), LevelFilter::Info);
        assert_eq!(filter.level("flux_lsp::client"), LevelFilter::Info);
        assert_eq!(filter.level("gpui::window"), LevelFilter::Warn);
        assert_eq!(filter.level("fluxion"), LevelFilter::Warn);
        assert_eq!(filter.max(), LevelFilter::Info);
        let filter = Filter::parse("debug");
        assert_eq!(filter.level("gpui"), LevelFilter::Debug);
        let filter = Filter::parse(" gpui=info, flux_lsp=trace ,error,bogus=nope, gpui::text=off");
        assert_eq!(filter.level("gpui::window"), LevelFilter::Info);
        assert_eq!(filter.level("gpui::text"), LevelFilter::Off);
        assert_eq!(filter.level("gpui::text::shaper"), LevelFilter::Off);
        assert_eq!(filter.level("gpuix"), LevelFilter::Error);
        assert_eq!(filter.level("flux_lsp"), LevelFilter::Trace);
        assert_eq!(filter.level("flux::editor"), LevelFilter::Error);
        assert_eq!(filter.max(), LevelFilter::Trace);
    }

    fn sample_crash() -> Crash {
        Crash {
            message: "index out of bounds: the len is 3 but the index is 5\nsecond line".into(),
            location: "crates/flux-app/src/editor.rs:120:9".into(),
            thread: "main".into(),
            backtrace: [
                "   7: flux::editor::Editor::paint",
                "             at ./src/editor.rs:120:9",
                "   8: gpui::window::Window::draw",
            ]
            .join("\n"),
            version: "0.1.0 (ca086c2)".into(),
            time: "2026-10-15 14:03:12".into(),
            file: PathBuf::from("/tmp/crash.txt"),
        }
    }

    #[test]
    fn a_crash_report_reads_back_as_written() {
        let crash = sample_crash();
        let machine = SystemInfo {
            version: crash.version.clone(),
            macos: "macOS 26.1 (25B78)".into(),
            hardware: "Apple M3 Pro, arm64".into(),
            ..SystemInfo::default()
        };
        let text = report_text(&crash, &machine, "line one\nTime: not a fact");
        assert!(
            text.starts_with("Flux crash report\n\nTime: 2026-10-15 14:03:12\n"),
            "{text}"
        );
        // The line is labeled with the system's name: "macOS:", on Windows "Windows:".
        let system = format!(
            "\n{}: macOS 26.1 (25B78)\nHardware: Apple M3 Pro, arm64\n",
            crate::feedback::SYSTEM_NAME
        );
        assert!(text.contains(&system), "{text}");
        assert!(
            text.contains("\nMessage:\n    index out of bounds"),
            "{text}"
        );
        assert!(
            text.contains("\n    line one\n    Time: not a fact\n"),
            "{text}"
        );
        let read = parse_report(&text, crash.file.clone()).unwrap();
        assert_eq!(read, crash);
        // What a following panic adds doesn't change it.
        let more = format!(
            "{text}\nThen: panic in a function that cannot unwind (x.rs:1:1): Flux quit.\n"
        );
        assert_eq!(parse_report(&more, crash.file.clone()).unwrap(), crash);
        assert_eq!(parse_report("not a report", PathBuf::new()), None);
    }

    #[test]
    fn the_stack_starts_where_the_panic_happened() {
        // As Rust 1.99 prints a worker thread's stack (the hook's frames named as in Flux).
        let backtrace = [
            "   0: std::backtrace_rs::backtrace::libunwind::trace",
            "             at /rustc/x/library/std/src/../../backtrace/src/libunwind.rs:117:9",
            concat!(
                "   1: std::backtrace_rs::backtrace::trace_unsynchronized::",
                "<<std::backtrace::Backtrace>::create::{closure#0}>"
            ),
            "   2: <std::backtrace::Backtrace>::create",
            "   3: flux::app_log::on_panic",
            "   4: flux::app_log::set_panic_hook::{closure#0}",
            concat!(
                "   5: <alloc::boxed::Box<dyn for<'a, 'b> core::ops::function::Fn<(&'a ",
                "std::panic::PanicHookInfo<'b>,), Output = ()> + core::marker::Send + ",
                "core::marker::Sync> as core::ops::function::Fn<(&std::panic::PanicHookInfo,)>>",
                "::call"
            ),
            "   6: std::panicking::panic_with_hook",
            "   7: std::panicking::panic_handler::{closure#0}",
            "   8: std::sys::backtrace::__rust_end_short_backtrace::<_, !>",
            "   9: __rustc::rust_begin_unwind",
            "  10: core::panicking::panic_fmt",
            "  11: core::panicking::panic",
            "  12: core::option::unwrap_failed",
            "  13: <core::option::Option<u8>>::unwrap",
            "  14: flux::editor::Editor::paint",
            "             at ./src/editor.rs:120:9",
            "  15: std::sys::backtrace::__rust_begin_short_backtrace::<_, ()>",
            "  16: std::panicking::catch_unwind::do_call::<(), ()>",
            "  17: <std::sys::thread::unix::Thread>::new::thread_start",
        ]
        .join("\n");
        assert_eq!(
            trim_backtrace(&backtrace),
            backtrace.lines().skip(14).collect::<Vec<_>>().join("\n"),
            "the stack from the unwrap down, the thread's start included"
        );
        let only_machinery = "   0: std::backtrace_rs::backtrace::libunwind::trace";
        assert_eq!(trim_backtrace(only_machinery), only_machinery);
        assert_eq!(trim_backtrace("disabled backtrace"), "disabled backtrace");
        let long = (0..20_000)
            .map(|n| format!("  {n}: frame::number_{n}\n"))
            .collect::<String>();
        let trimmed = trim_backtrace(&long);
        assert!(trimmed.len() <= MAX_BACKTRACE + 8 && trimmed.ends_with('…'));
    }

    #[test]
    fn the_newest_report_is_offered_once() {
        let dir = folder("offer");
        let at = |seconds: u64| UNIX_EPOCH + Duration::from_secs(1_760_000_000 + seconds);
        let mut paths = Vec::new();
        for (n, message) in ["old", "older still", "newest"].iter().enumerate() {
            let crash = Crash {
                message: message.to_string(),
                ..sample_crash()
            };
            let text = report_text(&crash, &SystemInfo::default(), "");
            let path = write_report(&dir, at(n as u64), &text).unwrap();
            let modified = at([10, 5, 20][n]);
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(modified)
                .unwrap();
            paths.push(path);
        }
        // Two reports of the same second don't overwrite each other.
        let again = write_report(&dir, at(2), "x").unwrap();
        assert_ne!(again, paths[2]);
        fs::remove_file(again).unwrap();
        let crash = take_unreported_in(&dir).unwrap();
        assert_eq!(crash.message, "newest");
        assert_eq!(crash.file, paths[2]);
        assert!(paths.iter().all(|path| offered_marker(path).exists()));
        assert_eq!(take_unreported_in(&dir), None);
        assert!(!mark_offered(&paths[0]), "a marker is made once");
    }

    #[test]
    fn only_the_last_reports_are_kept() {
        let dir = folder("prune");
        for n in 0..13_u64 {
            let at = UNIX_EPOCH + Duration::from_secs(1_760_000_000 + n);
            let path = write_report(&dir, at, "Flux crash report\n").unwrap();
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(at)
                .unwrap();
            if n < 4 {
                assert!(mark_offered(&path));
            }
        }
        fs::write(dir.join(".crash-gone.txt.offered"), "").unwrap();
        prune_crashes(&dir, 10);
        assert_eq!(crash_reports(&dir).len(), 10);
        let markers: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".offered"))
            .collect();
        // Of the four offered, the three oldest went with their reports; the strays too.
        assert_eq!(markers.len(), 1, "{markers:?}");
    }

    #[test]
    fn the_log_tail_starts_at_a_line() {
        let dir = folder("tail");
        let path = dir.join("flux.log");
        let mut text = String::new();
        for n in 0..2000 {
            text.push_str(&format!("line number {n}\n"));
        }
        fs::write(&path, &text).unwrap();
        let tail = log_tail(&path);
        assert!(tail.len() <= LOG_TAIL as usize);
        assert!(tail.starts_with("line number "), "{}", &tail[..40]);
        assert!(tail.ends_with("line number 1999"));
        fs::write(&path, "short\n").unwrap();
        assert_eq!(log_tail(&path), "short");
        assert_eq!(log_tail(&dir.join("missing.log")), "");
    }

    #[test]
    fn a_report_names_the_home_folder_as_tilde() {
        let home = "/Users/someone";
        assert_eq!(
            tilde(
                "at /Users/someone/.cargo/registry/x.rs:1:1 and /Users/someone2/y",
                home
            ),
            "at ~/.cargo/registry/x.rs:1:1 and /Users/someone2/y"
        );
        assert_eq!(tilde("/Users/someone/a", ""), "/Users/someone/a");
    }

    #[test]
    fn follow_up_panics_and_clipping() {
        assert!(follows_a_panic("panic in a function that cannot unwind"));
        assert!(follows_a_panic("panic in a destructor during cleanup"));
        assert!(!follows_a_panic("index out of bounds"));
        assert_eq!(clip("привет", 3), "п");
        assert_eq!(clip("abc", 10), "abc");
    }

    #[test]
    fn the_notice_says_when_and_what() {
        let crash = sample_crash();
        let notification = notice(&crash, true);
        assert_eq!(notification.title.as_ref(), "Flux quit unexpectedly");
        assert_eq!(
            notification.body.as_ref().map(|body| body.as_ref()),
            Some("2026-10-15 14:03 · index out of bounds: the len is 3 but the index is 5")
        );
        assert!(notification.offers(&["help::ReportCrash", "help::RevealCrash"]));
        assert!(notification.sticky);
        assert_eq!(
            notice(&crash, false).title.as_ref(),
            "Flux ran into an error"
        );
    }
}
