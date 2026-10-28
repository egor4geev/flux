//! The country check of Claude (part «Флаг страны», wiki ADR-041). Flux finds out the country of
//! the public IP address ([`flux_claude::region`]) and keeps Claude's requests from leaving while
//! it is on the user's list (Settings → Claude Code → Country Check; Russia by default) or can't be
//! told. The flag shows in the status bar and the console ([`crate::region_status`]). When the
//! country becomes a blocked one, everything of Claude stops at once: the sessions kill their
//! processes ([`RegionEvent::Blocked`] — a running turn goes with them; even an idle `claude`
//! talks to Anthropic), a commit message being written stops ([`stop_flag`]), and every `claude`
//! this Flux started through its terminals is ended. Nothing starts or is sent until the country is
//! allowed again.
//!
//! When it checks: as Flux starts; every 10 s while a session works, every minute otherwise; at
//! once when the network changes (its interfaces and addresses, looked at every 2 s); before
//! something new goes to Anthropic (a process starts, an idle one gets a message) when the last
//! answer is older than 10 s ([`gate`]); on "Check Again". A failed check is tried once more after
//! 2 s before Claude is blocked.
//!
//! One guard for the app, all windows: [`global`]. What waits for an answer ([`when_allowed`], a
//! session's message) goes on the next one. Scenarios and manual checks: `FLUX_REGION_FILE` —
//! the country is read from that file ("FI", "RU 203.0.113.7"; anything else — the check fails);
//! a scenario without it is in Finland; its `region:<code>` step changes the country.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use gpui::{App, AppContext as _, Context, Entity, EntityId, EventEmitter, Global, Task, actions};

use flux_claude::region::{self, Location};

use crate::i18n::{tr, trf};
use crate::notification_center::NotificationGroup;
use crate::notifications::Notification;
use crate::settings::{self, Settings};
use crate::workspace::Workspace;

actions!(
    region,
    [
        /// Asks the country again (the flag's popover, the banners, a notification).
        CheckCountry
    ]
);

/// An allowing answer older than this is asked again before something new goes to Anthropic.
pub const FRESH: Duration = Duration::from_secs(10);
/// The periodic checks: while a session works, and otherwise.
const WORKING_PACE: Duration = Duration::from_secs(10);
const IDLE_PACE: Duration = Duration::from_secs(60);
/// A failed check is tried once more after this.
const RETRY: Duration = Duration::from_secs(2);
/// How often the network's interfaces and addresses are looked at.
const NETWORK_POLL: Duration = Duration::from_secs(2);
/// A `claude` of a terminal gets SIGTERM (it puts the terminal back), then SIGKILL after this.
#[cfg(target_os = "macos")]
const TERM_GRACE: Duration = Duration::from_secs(2);

/// What the guard says about Claude now.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// The check is off (or Claude Code is): Claude goes.
    Off,
    /// No answer yet: Flux has just started, the network changed, the user asked again after a
    /// block. Nothing new starts meanwhile.
    Checking,
    /// The country isn't on the list.
    Allowed(Check),
    /// The country is on the list.
    Blocked(Check),
    /// The country couldn't be told: blocked as well (the author's rule).
    Unknown(Failure),
}

/// An answer of the check.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub location: Location,
    pub at: Instant,
}

/// A check that failed twice.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub error: String,
    pub at: Instant,
}

impl Verdict {
    /// Claude may go.
    pub fn allows(&self) -> bool {
        matches!(self, Verdict::Off | Verdict::Allowed(_))
    }

    /// Claude is blocked (the country is on the list or unknown).
    pub fn blocks(&self) -> bool {
        matches!(self, Verdict::Blocked(_) | Verdict::Unknown(_))
    }

    /// Where the network saw this computer at the last answer.
    pub fn location(&self) -> Option<&Location> {
        match self {
            Verdict::Allowed(check) | Verdict::Blocked(check) => Some(&check.location),
            _ => None,
        }
    }

    /// When the last answer came.
    pub fn checked_at(&self) -> Option<Instant> {
        match self {
            Verdict::Allowed(check) | Verdict::Blocked(check) => Some(check.at),
            Verdict::Unknown(failure) => Some(failure.at),
            Verdict::Off | Verdict::Checking => None,
        }
    }

    /// Why Claude doesn't go, when it is blocked.
    pub fn refusal(&self) -> Option<Refusal> {
        match self {
            Verdict::Blocked(check) => Some(Refusal {
                country: Some(check.location.country.clone()),
            }),
            Verdict::Unknown(_) => Some(Refusal { country: None }),
            _ => None,
        }
    }

    /// What shows of the verdict: its kind and the country (an answer that only refreshes the time
    /// redraws nothing).
    fn sight(&self) -> (std::mem::Discriminant<Verdict>, Option<Location>) {
        (std::mem::discriminant(self), self.location().cloned())
    }
}

/// Why Claude doesn't go: the country on the list, or `None` — it couldn't be told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub country: Option<String>,
}

impl Refusal {
    /// Why, under a "Claude is blocked" title.
    pub fn reason(&self) -> String {
        match &self.country {
            Some(code) => trf("The IP address is in {0}.", &[&country_label(code)]),
            None => tr("Flux couldn't find out the country of the IP address.").to_string(),
        }
    }

    /// One sentence for a banner, a note in the chat.
    pub fn message(&self) -> String {
        match &self.country {
            Some(code) => trf(
                "Claude is blocked: the IP address is in {0}.",
                &[&country_label(code)],
            ),
            None => tr("Claude is blocked: Flux couldn't find out the country of the IP address.")
                .to_string(),
        }
    }
}

/// What the guard tells.
#[derive(Debug, Clone, PartialEq)]
pub enum RegionEvent {
    /// The verdict changed in a way that shows: the flag, the banners.
    Changed,
    /// Claude became blocked: everything of it stops (the sessions kill their processes).
    Blocked(Refusal),
    /// Claude is allowed again after a block.
    Unblocked,
}

/// Whether Claude may go now ([`gate`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Gate {
    Go,
    /// An answer is on its way: wait for it ([`wait`]).
    Wait,
    Refuse(Refusal),
}

type Waiter = Box<dyn FnOnce(Result<(), Refusal>, &mut App)>;

pub struct RegionGuard {
    verdict: Verdict,
    /// The check that runs.
    check: Option<Task<()>>,
    /// Counts the checks: the answer of one replaced meanwhile (the network changed) is dropped.
    generation: u64,
    _timer: Option<Task<()>>,
    _network: Option<Task<()>>,
    /// Sessions that work now, in any window: the 10-second pace.
    working: HashSet<EntityId>,
    waiters: Vec<Waiter>,
    /// The stop flags of `claude -p` runs (a commit message): set when Claude becomes blocked.
    stops: Vec<Weak<AtomicBool>>,
    /// The settings the verdict was made with.
    rules: Rules,
    /// Why Claude was blocked at the last answer (`None` — it went): a block and its end are told
    /// between answers; a check in between (`Checking`) is neither.
    refusal: Option<Refusal>,
}

#[derive(Debug, Clone, PartialEq)]
struct Rules {
    active: bool,
    countries: Vec<String>,
}

impl Rules {
    fn of(cx: &App) -> Rules {
        let claude = settings::claude(cx);
        Rules {
            active: claude.enabled && claude.country_check,
            countries: claude.blocked_countries,
        }
    }
}

impl EventEmitter<RegionEvent> for RegionGuard {}

struct GlobalGuard(Entity<RegionGuard>);

impl Global for GlobalGuard {}

/// The guard of the app, once [`init`] has run.
pub fn global(cx: &App) -> Option<Entity<RegionGuard>> {
    cx.try_global::<GlobalGuard>()
        .map(|global| global.0.clone())
}

pub fn init(cx: &mut App) {
    let guard = cx.new(RegionGuard::new);
    cx.set_global(GlobalGuard(guard.clone()));
    cx.on_action(|_: &CheckCountry, cx| check_again(cx));
    // Every window: the flag in its status bar and console follows; a block or its end posts a
    // notification there.
    cx.observe_new(move |_: &mut Workspace, _, cx: &mut Context<Workspace>| {
        cx.observe(&guard, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&guard, |workspace, _, event, cx| {
            crate::region_status::on_event(workspace, event, cx)
        })
        .detach();
    })
    .detach();
}

/// The verdict now (`Off` without the guard: tests).
pub fn verdict(cx: &App) -> Verdict {
    global(cx).map_or(Verdict::Off, |guard| guard.read(cx).verdict.clone())
}

/// Whether Claude may go now. `fresh` — something new goes to Anthropic (a process starts, an idle
/// one gets a message): an allowing answer older than [`FRESH`] is asked again first, and the
/// caller waits ([`Gate::Wait`], then [`wait`]).
pub fn gate(fresh: bool, cx: &mut App) -> Gate {
    match global(cx) {
        Some(guard) => guard.update(cx, |guard, cx| guard.gate(fresh, cx)),
        None => Gate::Go,
    }
}

/// `then` runs on the next answer of the check (after a [`Gate::Wait`]): `Ok` — Claude may go.
pub fn wait(cx: &mut App, then: impl FnOnce(Result<(), Refusal>, &mut App) + 'static) {
    match global(cx) {
        Some(guard) => guard.update(cx, |guard, _| guard.waiters.push(Box::new(then))),
        None => then(Ok(()), cx),
    }
}

/// Runs `then` once Claude may go — with a fresh answer right after the caller's update, else
/// after the check it waits for; `Err` — blocked (`then` tells the user, [`notification`]).
/// Always deferred: `then` may update the caller's entity.
pub fn when_allowed(cx: &mut App, then: impl FnOnce(Result<(), Refusal>, &mut App) + 'static) {
    match gate(true, cx) {
        Gate::Go => cx.defer(move |cx| then(Ok(()), cx)),
        Gate::Refuse(refusal) => cx.defer(move |cx| then(Err(refusal), cx)),
        Gate::Wait => wait(cx, then),
    }
}

/// "Claude is blocked" with the reason and "Check Again": something the user asked for (sign-in,
/// the terminal CLI) didn't run.
pub fn notification(refusal: &Refusal) -> Notification {
    Notification::error(tr("Claude is blocked"))
        .body(refusal.reason())
        .action(tr("Check Again"), CheckCountry)
        .group(NotificationGroup::Claude)
}

/// A flag for a `claude -p` run ([`flux_claude::Cli::ask`]): set — and the run killed — when
/// Claude becomes blocked.
pub fn stop_flag(cx: &mut App) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    if let Some(guard) = global(cx) {
        guard.update(cx, |guard, _| {
            guard.stops.retain(|stop| stop.strong_count() > 0);
            guard.stops.push(Arc::downgrade(&flag));
        });
    }
    flag
}

/// A session started or stopped working: the pace of the checks follows.
pub fn note_working(id: EntityId, working: bool, cx: &mut App) {
    let Some(guard) = global(cx) else {
        return;
    };
    guard.update(cx, |guard, cx| {
        let was_idle = guard.working.is_empty();
        if working {
            guard.working.insert(id);
        } else {
            guard.working.remove(&id);
        }
        if was_idle != guard.working.is_empty() {
            guard.schedule(cx);
        }
    });
}

/// "Check Again": after a block the answer is awaited in sight (the flag shows the check);
/// otherwise the country is refreshed quietly.
pub fn check_again(cx: &mut App) {
    if let Some(guard) = global(cx) {
        guard.update(cx, |guard, cx| {
            let reset = !guard.verdict.allows();
            guard.check_now(reset, cx)
        });
    }
}

/// The `region:<code>` step of a scenario: the country from now on (`fail` — the check fails),
/// asked at once.
#[cfg(feature = "scenario")]
pub fn simulate(code: &str, cx: &mut App) {
    *OVERRIDE.lock().unwrap_or_else(|err| err.into_inner()) = Some(code.to_string());
    if let Some(guard) = global(cx) {
        guard.update(cx, |guard, cx| guard.check_now(true, cx));
    }
}

impl RegionGuard {
    fn new(cx: &mut Context<Self>) -> Self {
        cx.observe_global::<Settings>(|this, cx| this.rules_changed(cx))
            .detach();
        let rules = Rules::of(cx);
        let mut guard = Self {
            verdict: Verdict::Off,
            check: None,
            generation: 0,
            _timer: None,
            _network: None,
            working: HashSet::new(),
            waiters: Vec::new(),
            stops: Vec::new(),
            rules,
            refusal: None,
        };
        if guard.rules.active {
            guard.verdict = Verdict::Checking;
            guard.start(cx);
        }
        guard
    }

    pub fn verdict(&self) -> &Verdict {
        &self.verdict
    }

    /// The countries Claude is blocked in (the settings').
    pub fn countries(&self) -> &[String] {
        &self.rules.countries
    }

    /// A check runs now.
    pub fn is_checking(&self) -> bool {
        self.check.is_some()
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.check_now(true, cx);
        self.watch_network(cx);
    }

    fn rules_changed(&mut self, cx: &mut Context<Self>) {
        let rules = Rules::of(cx);
        if rules == self.rules {
            return;
        }
        let was_active = self.rules.active;
        self.rules = rules;
        match (was_active, self.rules.active) {
            (_, false) => {
                self.check = None;
                self._timer = None;
                self._network = None;
                self.set_verdict(Verdict::Off, cx);
                self.answer_waiters(cx);
            }
            (false, true) => {
                self.set_verdict(Verdict::Checking, cx);
                self.start(cx);
            }
            // The list changed: the last country is judged again at once.
            (true, true) => {
                let verdict = match &self.verdict {
                    Verdict::Allowed(check) | Verdict::Blocked(check) => self.judge(check.clone()),
                    other => other.clone(),
                };
                self.set_verdict(verdict, cx);
                self.answer_waiters(cx);
            }
        }
    }

    fn judge(&self, check: Check) -> Verdict {
        if self.rules.countries.contains(&check.location.country) {
            Verdict::Blocked(check)
        } else {
            Verdict::Allowed(check)
        }
    }

    fn gate(&mut self, fresh: bool, cx: &mut Context<Self>) -> Gate {
        match &self.verdict {
            Verdict::Off => Gate::Go,
            Verdict::Allowed(check) if !fresh || check.at.elapsed() <= FRESH => Gate::Go,
            Verdict::Allowed(_) | Verdict::Checking => {
                self.check_now(false, cx);
                Gate::Wait
            }
            verdict => Gate::Refuse(verdict.refusal().expect("a blocking verdict")),
        }
    }

    /// Asks the country now. `reset` — the last answer no longer counts (the network changed,
    /// Flux started, the user asked again after a block): nothing new goes until the answer, and a
    /// check already running is replaced.
    fn check_now(&mut self, reset: bool, cx: &mut Context<Self>) {
        if !self.rules.active {
            return;
        }
        if reset && self.verdict != Verdict::Checking {
            self.set_verdict(Verdict::Checking, cx);
        }
        // The one running answers for this one too.
        if self.check.is_some() && !reset {
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        self.check = Some(cx.spawn(async move |this, cx| {
            let mut result = cx.background_spawn(async { locate() }).await;
            if result.is_err() {
                cx.background_executor().timer(RETRY).await;
                result = cx.background_spawn(async { locate() }).await;
            }
            this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.check = None;
                    this.answered(result, cx);
                }
            })
            .ok();
        }));
    }

    fn answered(&mut self, result: Result<Location, String>, cx: &mut Context<Self>) {
        let at = Instant::now();
        let verdict = match result {
            Ok(location) => self.judge(Check { location, at }),
            Err(error) => {
                log::warn!("country check: {error}");
                Verdict::Unknown(Failure { error, at })
            }
        };
        self.set_verdict(verdict, cx);
        self.schedule(cx);
        self.answer_waiters(cx);
    }

    /// What waited for an answer goes, or hears why not — after this update: they ask the guard
    /// again.
    fn answer_waiters(&mut self, cx: &mut Context<Self>) {
        if self.waiters.is_empty() || matches!(self.verdict, Verdict::Checking) {
            return;
        }
        let outcome = match self.verdict.refusal() {
            Some(refusal) => Err(refusal),
            None => Ok(()),
        };
        let waiters = std::mem::take(&mut self.waiters);
        cx.defer(move |cx| {
            for waiter in waiters {
                waiter(outcome.clone(), cx);
            }
        });
    }

    fn set_verdict(&mut self, verdict: Verdict, cx: &mut Context<Self>) {
        let before = std::mem::replace(&mut self.verdict, verdict);
        if before.sight() != self.verdict.sight() {
            cx.emit(RegionEvent::Changed);
            cx.notify();
        }
        // What may have started since the last answer stops (nothing does while checking: it waits).
        if self.verdict.blocks() && !before.blocks() {
            self.stop_everything();
        }
        if self.verdict == Verdict::Checking {
            return;
        }
        let refusal = self.verdict.refusal();
        if refusal != self.refusal {
            self.refusal = refusal.clone();
            cx.emit(match refusal {
                Some(refusal) => {
                    log::warn!("country check: Claude is blocked ({:?})", refusal.country);
                    RegionEvent::Blocked(refusal)
                }
                None => RegionEvent::Unblocked,
            });
        }
    }

    /// Claude became blocked: what runs outside the sessions stops — `claude -p` runs, every
    /// `claude` this Flux started through its terminals. The sessions kill their own processes
    /// ([`RegionEvent::Blocked`]).
    fn stop_everything(&mut self) {
        for stop in self.stops.drain(..).filter_map(|stop| stop.upgrade()) {
            stop.store(true, Ordering::Relaxed);
        }
        #[cfg(target_os = "macos")]
        std::thread::Builder::new()
            .name("region-stop".into())
            .spawn(stop_claude_processes)
            .ok();
    }

    /// The next periodic check: [`WORKING_PACE`] after the last answer while a session works,
    /// [`IDLE_PACE`] otherwise.
    fn schedule(&mut self, cx: &mut Context<Self>) {
        if !self.rules.active {
            self._timer = None;
            return;
        }
        let pace = if self.working.is_empty() {
            IDLE_PACE
        } else {
            WORKING_PACE
        };
        let since = self
            .verdict
            .checked_at()
            .map_or(Duration::ZERO, |at| at.elapsed());
        let wait = pace.saturating_sub(since);
        self._timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            this.update(cx, |this, cx| this.check_now(false, cx)).ok();
        }));
    }

    /// Looks at the network's interfaces and addresses every [`NETWORK_POLL`]: a change (a VPN
    /// went up or down, another Wi-Fi, waking up) — the last answer no longer counts.
    fn watch_network(&mut self, cx: &mut Context<Self>) {
        self._network = Some(cx.spawn(async move |this, cx| {
            let mut last = cx.background_spawn(async { network_fingerprint() }).await;
            loop {
                cx.background_executor().timer(NETWORK_POLL).await;
                let now = cx.background_spawn(async { network_fingerprint() }).await;
                if now == last {
                    continue;
                }
                last = now;
                if this
                    .update(cx, |this, cx| this.check_now(true, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }
}

/// The country set by a scenario's `region:` step.
static OVERRIDE: Mutex<Option<String>> = Mutex::new(None);

/// Where the network sees this computer: what a scenario or `FLUX_REGION_FILE` says, else the
/// services of [`flux_claude::region`] through the proxy `claude` uses. Blocks.
fn locate() -> Result<Location, String> {
    let set = OVERRIDE
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone();
    if let Some(code) = set {
        return pretend(&code);
    }
    if let Some(path) = std::env::var_os("FLUX_REGION_FILE").filter(|path| !path.is_empty()) {
        return pretend(&std::fs::read_to_string(&path).unwrap_or_default());
    }
    #[cfg(feature = "scenario")]
    if std::env::var_os("FLUX_SCENARIO").is_some() {
        return pretend("FI 203.0.113.7");
    }
    region::detect(region::claude_proxy().as_deref())
}

/// A country as scenarios give it: "FI", "RU 203.0.113.7"; anything else — the check failed.
fn pretend(text: &str) -> Result<Location, String> {
    let mut words = text.split_whitespace();
    let country = words
        .next()
        .and_then(region::normalize)
        .ok_or_else(|| format!("no country in {text:?}"))?;
    Ok(Location {
        country,
        ip: words.next().map(str::to_string),
    })
}

/// "Finland (FI)" in the language of the interface; the code alone when the system has no name.
pub fn country_label(code: &str) -> String {
    match country_name(code) {
        Some(name) => format!("{name} ({code})"),
        None => code.to_string(),
    }
}

/// The flag of a country: 🇫🇮.
pub fn flag(code: &str) -> String {
    region::flag(code)
}

/// The country's name in the language of the interface ("Финляндия"), from the system's locale
/// data.
#[cfg(target_os = "macos")]
pub fn country_name(code: &str) -> Option<String> {
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};
    use core_foundation_sys::base::{CFRelease, kCFAllocatorDefault};
    use core_foundation_sys::locale::{
        CFLocaleCopyDisplayNameForPropertyValue, CFLocaleCreate, kCFLocaleCountryCode,
    };

    let code = region::normalize(code)?;
    let language = CFString::new(crate::i18n::lang_code());
    let value = CFString::new(&code);
    // SAFETY: the locale is created and released here; the name comes with the create rule.
    unsafe {
        let locale = CFLocaleCreate(kCFAllocatorDefault, language.as_concrete_TypeRef());
        if locale.is_null() {
            return None;
        }
        let name: CFStringRef = CFLocaleCopyDisplayNameForPropertyValue(
            locale,
            kCFLocaleCountryCode,
            value.as_concrete_TypeRef(),
        );
        CFRelease(locale.cast());
        if name.is_null() {
            return None;
        }
        Some(CFString::wrap_under_create_rule(name).to_string())
    }
}

#[cfg(not(target_os = "macos"))]
pub fn country_name(_code: &str) -> Option<String> {
    None
}

/// The network as a number that changes when it does: the up interfaces (not loopback) and their
/// addresses.
#[cfg(target_os = "macos")]
fn network_fingerprint() -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::ffi::CStr;
    use std::hash::{Hash, Hasher};

    let mut found: Vec<(String, Vec<u8>)> = Vec::new();
    let mut addresses: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: `getifaddrs` fills a list freed below; each entry is read while the list lives.
    unsafe {
        if libc::getifaddrs(&mut addresses) != 0 {
            return 0;
        }
        let mut entry = addresses;
        while let Some(interface) = entry.as_ref() {
            entry = interface.ifa_next;
            let flags = interface.ifa_flags;
            let up = flags & libc::IFF_UP as u32 != 0 && flags & libc::IFF_LOOPBACK as u32 == 0;
            let Some(address) = interface.ifa_addr.as_ref().filter(|_| up) else {
                continue;
            };
            let size = match i32::from(address.sa_family) {
                libc::AF_INET => std::mem::size_of::<libc::sockaddr_in>(),
                libc::AF_INET6 => std::mem::size_of::<libc::sockaddr_in6>(),
                _ => continue,
            };
            let bytes = std::slice::from_raw_parts(interface.ifa_addr.cast::<u8>(), size);
            let name = CStr::from_ptr(interface.ifa_name)
                .to_string_lossy()
                .into_owned();
            found.push((name, bytes.to_vec()));
        }
        libc::freeifaddrs(addresses);
    }
    found.sort();
    let mut hasher = DefaultHasher::new();
    found.hash(&mut hasher);
    hasher.finish()
}

/// Other systems (the ports, 10.9–10.10): no watching yet — the periodic checks only.
#[cfg(not(target_os = "macos"))]
fn network_fingerprint() -> u64 {
    0
}

/// Ends every `claude` this Flux started, directly or through its terminals and their shells:
/// SIGTERM, then SIGKILL for one still there after [`TERM_GRACE`].
#[cfg(target_os = "macos")]
fn stop_claude_processes() {
    let me = std::process::id() as libc::pid_t;
    let found: Vec<libc::pid_t> = processes::all()
        .into_iter()
        .filter(|&pid| pid != me && processes::is_claude(pid) && processes::descends(pid, me))
        .collect();
    if found.is_empty() {
        return;
    }
    log::warn!("country check: stopping {} claude processes", found.len());
    for &pid in &found {
        // SAFETY: a signal to a process found above.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    std::thread::sleep(TERM_GRACE);
    for &pid in &found {
        if processes::is_claude(pid) && processes::descends(pid, me) {
            // SAFETY: as above; it is still that `claude`.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

/// The processes of the system, through libproc.
#[cfg(target_os = "macos")]
mod processes {
    use std::mem::{MaybeUninit, size_of};

    use libc::{c_int, pid_t};

    pub fn all() -> Vec<pid_t> {
        // SAFETY: a null buffer asks for the count; the second call fills at most the buffer.
        unsafe {
            let count = libc::proc_listallpids(std::ptr::null_mut(), 0);
            if count <= 0 {
                return Vec::new();
            }
            let mut pids: Vec<pid_t> = vec![0; count as usize + 64];
            let bytes = (pids.len() * size_of::<pid_t>()) as c_int;
            let filled = libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes);
            pids.truncate(filled.max(0) as usize);
            pids
        }
    }

    fn parent(pid: pid_t) -> Option<pid_t> {
        let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        let size = size_of::<libc::proc_bsdinfo>() as c_int;
        // SAFETY: the kernel writes at most `size` bytes into `info`.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if written != size {
            return None;
        }
        // SAFETY: the whole struct was written.
        Some(unsafe { info.assume_init() }.pbi_ppid as pid_t)
    }

    /// Whether `pid` is a child, a grandchild… of `ancestor`.
    pub fn descends(pid: pid_t, ancestor: pid_t) -> bool {
        let mut current = pid;
        for _ in 0..64 {
            match parent(current) {
                Some(parent) if parent == ancestor => return true,
                Some(parent) if parent > 1 => current = parent,
                _ => return false,
            }
        }
        false
    }

    /// The executable is `claude` (the native installer, Homebrew), or Node runs Claude Code's
    /// package (npm).
    pub fn is_claude(pid: pid_t) -> bool {
        let path = executable(pid).unwrap_or_default();
        let name = path.rsplit('/').next().unwrap_or_default();
        name == "claude" || name == "node" && arguments(pid).contains("@anthropic-ai/claude-code")
    }

    fn executable(pid: pid_t) -> Option<String> {
        let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: the buffer outlives the call and its size is passed along.
        let len = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
        if len <= 0 {
            return None;
        }
        path.truncate(len as usize);
        String::from_utf8(path).ok()
    }

    /// The command line, as one string (`KERN_PROCARGS2`).
    fn arguments(pid: pid_t) -> String {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut size: libc::size_t = 0;
        // SAFETY: the first call asks for the size, the second fills at most `size` bytes.
        unsafe {
            if libc::sysctl(
                mib.as_mut_ptr(),
                3,
                std::ptr::null_mut(),
                &mut size,
                std::ptr::null_mut(),
                0,
            ) != 0
            {
                return String::new();
            }
            let mut buffer = vec![0u8; size];
            if libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buffer.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            ) != 0
            {
                return String::new();
            }
            buffer.truncate(size);
            String::from_utf8_lossy(&buffer).replace('\0', " ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenarios_pretend_a_country() {
        assert_eq!(
            pretend("ru 203.0.113.7\n"),
            Ok(Location {
                country: "RU".into(),
                ip: Some("203.0.113.7".into()),
            })
        );
        assert_eq!(
            pretend("FI"),
            Ok(Location {
                country: "FI".into(),
                ip: None,
            })
        );
        assert!(pretend("fail").is_err());
        assert!(pretend("").is_err());
    }

    #[test]
    fn verdicts_block_and_allow() {
        let check = Check {
            location: Location {
                country: "RU".into(),
                ip: None,
            },
            at: Instant::now(),
        };
        assert!(Verdict::Off.allows());
        assert!(!Verdict::Checking.allows() && !Verdict::Checking.blocks());
        assert!(Verdict::Allowed(check.clone()).allows());
        assert_eq!(
            Verdict::Blocked(check).refusal(),
            Some(Refusal {
                country: Some("RU".into())
            })
        );
        let failure = Verdict::Unknown(Failure {
            error: "offline".into(),
            at: Instant::now(),
        });
        assert!(failure.blocks());
        assert_eq!(failure.refusal(), Some(Refusal { country: None }));
    }

    #[test]
    fn the_network_has_a_fingerprint() {
        // The same network twice in a row.
        assert_eq!(network_fingerprint(), network_fingerprint());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn countries_have_names() {
        // The interface speaks English in tests.
        assert_eq!(country_name("FI").as_deref(), Some("Finland"));
        assert_eq!(country_name("XX"), None);
        assert_eq!(country_label("FI"), "Finland (FI)");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn this_process_descends_from_its_parent() {
        let me = std::process::id() as libc::pid_t;
        // SAFETY: reads this process's parent id.
        let parent = unsafe { libc::getppid() };
        assert!(processes::descends(me, parent));
        assert!(!processes::descends(parent, me));
        assert!(!processes::is_claude(me));
    }
}
