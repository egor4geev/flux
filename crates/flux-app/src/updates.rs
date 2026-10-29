//! Updates of Flux over the air (part 10.8), as in JetBrains IDEs: a few seconds after Flux starts
//! and then once a day (Settings → Updates: on or off, the channel) the newest release of the
//! channel is read from GitHub Releases ([`flux_update`]). A newer version is offered in a card —
//! What's New (its release notes, [`crate::update_notes`]), Update and Restart, Skip This Version.
//! Update and Restart downloads the archive in the background (its progress in the status bar, a
//! click there cancels it), checks it, and quits Flux the usual way — the questions about unsaved
//! documents and running commands included; once Flux has quit, a helper puts the new `Flux.app` in
//! place and starts it. Keeping Flux open in one of those questions keeps the update ready: a card
//! and the status bar offer Restart Now.
//!
//! Check for Updates… (the Flux menu, the command palette) asks at once and always answers: the
//! release notes of a new version, «Flux is up to date», or why the check failed. A version skipped
//! is offered again only by a check asked for.
//!
//! One [`Updates`] for the app, whatever the number of windows: the cards go to the active project
//! window. The version skipped and the time of the last check are kept in `updates.json` next to
//! `settings.json` (`FLUX_UPDATES_FILE` — another file; a scenario without it keeps nothing).
//! `FLUX_UPDATE_FEED` reads another `latest.json` ([`flux_update::check`]); in a scenario Flux
//! checks by itself only with it.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flux_update::install::{self, CannotInstall, Prepared};
use flux_update::{Channel, CheckError, Manifest, Release, Version};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, Global, Task, Window, actions,
    div, prelude::*, px, relative,
};
use serde_json::{Value, json};

use crate::dialog::Dialog;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::motion::TransitionExt;
use crate::notification_center::NotificationGroup;
use crate::notifications::Notification;
use crate::theme::{self, Theme};
use crate::ui::{self, RADIUS_SM};
use crate::workspace::Workspace;
use crate::{settings, update_notes};

actions!(
    updates,
    [
        /// Asks GitHub for a newer Flux now and answers whatever it finds (the Flux menu).
        CheckForUpdates,
        /// The release notes of the version found (a card's What's New).
        ShowReleaseNotes,
        /// Downloads the version found and restarts into it.
        InstallUpdate,
        /// No more automatic offers of the version found.
        SkipUpdate,
        /// Stops the download in progress.
        CancelUpdate,
        /// Quits Flux to finish an update already downloaded.
        RestartToUpdate,
        /// The page of the version found on GitHub: its notes and its installer.
        OpenReleasePage,
    ]
);

/// The first automatic check: after the windows have opened and settled.
const STARTUP_DELAY: Duration = Duration::from_secs(5);
/// Automatic checks come this often.
const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// A launch checks unless the last check is younger than this: relaunching Flux a few times in a
/// row shouldn't spend GitHub's unauthenticated request limit (60 an hour).
const LAUNCH_FLOOR: Duration = Duration::from_secs(60 * 60);
/// The checking loop looks at the clock and the settings at least this often (a switch turned on,
/// a Mac waking up after a day asleep).
const WAKE: Duration = Duration::from_secs(60 * 60);

/// What the last check found.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Nothing newer on the channel than this version.
    Latest,
    /// A newer version.
    Found(Manifest),
    /// The check failed: why, in the interface language.
    Failed(String),
}

/// Where an update is.
#[derive(Debug, Clone)]
pub enum Phase {
    Idle,
    Downloading {
        version: Version,
        done: u64,
        total: u64,
        cancel: Arc<AtomicBool>,
    },
    /// Unpacked and its signature checked.
    Preparing {
        version: Version,
    },
    /// Downloaded and checked: it installs when Flux quits to restart.
    Ready(Prepared),
}

/// Who asked for a check, which decides how its answer shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Mode {
    /// The daily check: a new version gets a card, the rest stays quiet.
    Auto,
    /// Settings → Updates → Check Now: the answer shows on the page.
    Inline,
    /// Check for Updates…: the release notes, «up to date», or the error.
    Manual,
}

/// The updates of the app.
pub struct Updates {
    checking: bool,
    /// The most demanding check asked for while one runs.
    mode: Mode,
    outcome: Option<Outcome>,
    /// The newest version found that is newer than this one.
    release: Option<Manifest>,
    phase: Phase,
    /// Counts the downloads: the result of a cancelled one is dropped.
    download: u64,
    state: State,
    /// The version a card was shown for in this run: the daily check doesn't repeat it.
    notified: Option<Version>,
    /// Flux is quitting to install the update.
    restart_requested: bool,
    _loop: Task<()>,
}

struct GlobalUpdates(Entity<Updates>);

impl Global for GlobalUpdates {}

/// The updates of the app, once [`init`] has run.
pub fn updates(cx: &App) -> Option<Entity<Updates>> {
    cx.try_global::<GlobalUpdates>()
        .map(|global| global.0.clone())
}

pub fn init(cx: &mut App) {
    cx.on_action(|_: &CheckForUpdates, cx| with_updates(cx, |u, cx| u.check(Mode::Manual, cx)));
    cx.on_action(|_: &ShowReleaseNotes, cx| show_release_notes(cx));
    cx.on_action(|_: &InstallUpdate, cx| install_update(cx));
    cx.on_action(|_: &SkipUpdate, cx| skip_update(cx));
    cx.on_action(|_: &CancelUpdate, cx| with_updates(cx, |u, cx| u.cancel(cx)));
    cx.on_action(|_: &RestartToUpdate, cx| with_updates(cx, |u, cx| u.restart(cx)));
    cx.on_action(|_: &OpenReleasePage, cx| open_release_page(cx));

    let updates = cx.new(Updates::new);
    cx.set_global(GlobalUpdates(updates.clone()));
    // Every window's status bar shows the download.
    cx.observe_new(move |_: &mut Workspace, _, cx: &mut Context<Workspace>| {
        cx.observe(&updates, |_, _, cx| cx.notify()).detach();
    })
    .detach();
    // Quitting to restart: the helper swaps the bundles once this process has exited.
    cx.on_app_quit(|cx| {
        install_on_quit(cx);
        async {}
    })
    .detach();
}

fn with_updates(cx: &mut App, f: impl FnOnce(&mut Updates, &mut Context<Updates>)) {
    if let Some(updates) = updates(cx) {
        updates.update(cx, f);
    }
}

impl Updates {
    fn new(cx: &mut Context<Self>) -> Self {
        let checks = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(STARTUP_DELAY).await;
            let mut launch = true;
            loop {
                let launched = std::mem::take(&mut launch);
                let Ok(wait) = this.update(cx, |this, cx| this.tick(launched, cx)) else {
                    return;
                };
                cx.background_executor().timer(wait).await;
            }
        });
        #[allow(unused_mut)]
        let mut updates = Self {
            checking: false,
            mode: Mode::Auto,
            outcome: None,
            release: None,
            phase: Phase::Idle,
            download: 0,
            state: State::load(),
            notified: None,
            restart_requested: false,
            _loop: checks,
        };
        #[cfg(feature = "scenario")]
        scenario_phase(&mut updates);
        updates
    }

    pub fn checking(&self) -> bool {
        self.checking
    }

    pub fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_ref()
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// When Flux last asked for a new version (Unix seconds).
    pub fn last_check(&self) -> Option<u64> {
        self.state.last_check
    }

    /// The automatic check, when it is due: returns how long to sleep till the next look. The first
    /// look after a launch checks right away (see [`due`]).
    fn tick(&mut self, launch: bool, cx: &mut Context<Self>) -> Duration {
        if !automatic(cx) {
            return WAKE;
        }
        let now = now();
        let due = due(self.state.last_check, launch);
        if now < due {
            return Duration::from_secs(due - now).min(WAKE);
        }
        if !self.checking && matches!(self.phase, Phase::Idle) {
            self.check(Mode::Auto, cx);
        }
        WAKE
    }

    /// Asks GitHub for the newest release of the channel (off the UI thread).
    fn check(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.checking {
            self.mode = self.mode.max(mode);
            return;
        }
        // An update downloaded or on its way: asking again would only offer it again.
        if mode == Mode::Manual && !matches!(self.phase, Phase::Idle) {
            if let Some(manifest) = self.release.clone() {
                open_notes(manifest, cx);
            }
            return;
        }
        self.checking = true;
        self.mode = mode;
        cx.notify();
        let channel = settings::updates(cx).channel;
        let task = cx.background_spawn(async move { flux_update::latest(channel) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| this.checked(result, cx)).ok();
        })
        .detach();
    }

    fn checked(&mut self, result: Result<Release, CheckError>, cx: &mut Context<Self>) {
        self.checking = false;
        let mode = self.mode;
        self.state.last_check = Some(now());
        self.state.save();
        match result {
            Ok(release) if release.is_update => {
                let manifest = release.manifest;
                self.outcome = Some(Outcome::Found(manifest.clone()));
                self.release = Some(manifest.clone());
                match mode {
                    Mode::Manual => open_notes(manifest.clone(), cx),
                    Mode::Inline => {}
                    Mode::Auto => {
                        let version = manifest.version.to_string();
                        let skipped = self.state.skipped.as_deref() == Some(version.as_str());
                        if !skipped && self.notified.as_ref() != Some(&manifest.version) {
                            self.notified = Some(manifest.version.clone());
                            offer(available_card(&manifest), cx);
                        }
                    }
                }
            }
            Ok(_) => {
                self.outcome = Some(Outcome::Latest);
                if matches!(self.phase, Phase::Idle) {
                    self.release = None;
                }
                if mode == Mode::Manual {
                    offer(
                        Notification::success(trf(
                            "Flux {0} is up to date",
                            &[&flux_update::current_version()],
                        ))
                        .group(NotificationGroup::Updates)
                        .transient(),
                        cx,
                    );
                }
            }
            Err(error) => {
                log::warn!("updates: {error}");
                let reason = describe(&error);
                self.outcome = Some(Outcome::Failed(reason.clone()));
                if mode == Mode::Manual {
                    offer(
                        Notification::warning(tr("Couldn't check for updates"))
                            .body(reason)
                            .action(tr("Retry"), CheckForUpdates)
                            .group(NotificationGroup::Updates),
                        cx,
                    );
                }
            }
        }
        cx.notify();
    }

    /// The channel changed: what was found for the other one no longer applies; the page asks
    /// again.
    pub fn channel_changed(&mut self, cx: &mut Context<Self>) {
        if matches!(self.phase, Phase::Idle) {
            self.release = None;
            self.outcome = None;
            dismiss_cards(cx);
        }
        self.check(Mode::Inline, cx);
    }

    /// Check Now on Settings → Updates.
    pub fn check_now(&mut self, cx: &mut Context<Self>) {
        self.check(Mode::Inline, cx);
    }

    /// Update and Restart: download, check, then quit to install.
    fn install(&mut self, window: Option<gpui::AnyWindowHandle>, cx: &mut Context<Self>) {
        let Some(manifest) = self.release.clone() else {
            return;
        };
        match &self.phase {
            Phase::Ready(prepared) if prepared.version == manifest.version => {
                return self.restart(cx);
            }
            Phase::Idle => {}
            // Already on its way.
            _ => return,
        }
        if let Err(reason) = install::can_install() {
            explain_cannot_install(reason, manifest, window, cx);
            return;
        }
        dismiss_cards(cx);
        self.download += 1;
        let generation = self.download;
        let cancel = Arc::new(AtomicBool::new(false));
        let total = manifest.platform().map_or(0, |platform| platform.size);
        self.phase = Phase::Downloading {
            version: manifest.version.clone(),
            done: 0,
            total,
            cancel: cancel.clone(),
        };
        cx.notify();

        let (sender, mut steps) = mpsc::unbounded::<Step>();
        let work = cx.background_spawn({
            let manifest = manifest.clone();
            async move {
                // Every per cent, not every chunk: each step redraws the windows.
                let mut shown = None;
                let archive =
                    flux_update::download::download(&manifest, &cancel, &mut |done, total| {
                        let percent = (done * 100).checked_div(total);
                        if percent != shown || done == total {
                            shown = percent;
                            sender.unbounded_send(Step::Progress(done, total)).ok();
                        }
                    })?;
                if cancel.load(Ordering::Relaxed) {
                    return Err(flux_update::download::CANCELLED.to_string());
                }
                sender.unbounded_send(Step::Preparing).ok();
                install::prepare(&archive, &manifest.version)
            }
        });
        cx.spawn(async move |this, cx| {
            while let Some(step) = steps.next().await {
                let alive = this.update(cx, |this, cx| {
                    if this.download == generation {
                        this.step(step, cx);
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| {
                if this.download == generation {
                    this.downloaded(result, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn step(&mut self, step: Step, cx: &mut Context<Self>) {
        match (&mut self.phase, step) {
            (Phase::Downloading { done, total, .. }, Step::Progress(now, all)) => {
                *done = now;
                *total = all;
            }
            (Phase::Downloading { version, .. }, Step::Preparing) => {
                self.phase = Phase::Preparing {
                    version: version.clone(),
                };
            }
            _ => return,
        }
        cx.notify();
    }

    fn downloaded(&mut self, result: Result<Prepared, String>, cx: &mut Context<Self>) {
        match result {
            Ok(prepared) => {
                self.phase = Phase::Ready(prepared);
                cx.notify();
                self.restart(cx);
            }
            Err(error) if error == flux_update::download::CANCELLED => {
                self.phase = Phase::Idle;
                cx.notify();
            }
            Err(error) => {
                log::warn!("updates: {error}");
                self.phase = Phase::Idle;
                cx.notify();
                let title = match &self.release {
                    Some(manifest) => trf("Couldn't update to Flux {0}", &[&manifest.version]),
                    None => tr("Couldn't update Flux").to_string(),
                };
                offer(
                    Notification::error(title)
                        .body(error)
                        .action(tr("Retry"), InstallUpdate)
                        .action(tr("Open Release Page"), OpenReleasePage)
                        .group(NotificationGroup::Updates),
                    cx,
                );
            }
        }
    }

    /// Stops the download; the archive's part is dropped quietly.
    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Phase::Downloading { cancel, .. } = &self.phase {
            cancel.store(true, Ordering::Relaxed);
            self.download += 1;
            self.phase = Phase::Idle;
            cx.notify();
        }
    }

    /// Quits Flux the usual way to install the update; keeping Flux open leaves it ready.
    fn restart(&mut self, cx: &mut Context<Self>) {
        let Phase::Ready(prepared) = &self.phase else {
            return;
        };
        let version = prepared.version.clone();
        self.restart_requested = true;
        dismiss_cards(cx);
        let this = cx.entity();
        cx.defer(move |cx| {
            crate::workspace::quit_to_update(cx, move |cx| {
                this.update(cx, |this, cx| {
                    this.restart_requested = false;
                    cx.notify();
                    let mut card = Notification::info(tr("Restart Flux to finish the update"))
                        .body(trf(
                            "Flux {0} is downloaded and checked: it installs when Flux restarts.",
                            &[&version],
                        ))
                        .action(tr("Restart Now"), RestartToUpdate)
                        .group(NotificationGroup::Updates);
                    card.sticky = true;
                    offer(card, cx);
                })
            });
        });
    }

    fn skip(&mut self, cx: &mut Context<Self>) {
        let Some(manifest) = &self.release else {
            return;
        };
        self.state.skipped = Some(manifest.version.to_string());
        self.state.save();
        dismiss_cards(cx);
        cx.notify();
    }

    /// Whether the version found was skipped.
    pub fn skipped(&self, version: &Version) -> bool {
        self.state.skipped.as_deref() == Some(version.to_string().as_str())
    }
}

/// A step of a download, sent from its thread.
enum Step {
    Progress(u64, u64),
    Preparing,
}

/// The card of a new version.
fn available_card(manifest: &Manifest) -> Notification {
    let date = release_date(&manifest.date);
    let body = match Channel::of(&manifest.version) {
        Channel::Beta => trf("A beta of the next version · released {0}", &[&date]),
        Channel::Stable => trf("Released {0}", &[&date]),
    };
    let mut card = Notification::info(trf("Flux {0} is available", &[&manifest.version]))
        .body(body)
        .action(tr("What's New"), ShowReleaseNotes)
        .action(tr("Update and Restart"), InstallUpdate)
        .action(tr("Skip This Version"), SkipUpdate)
        .group(NotificationGroup::Updates);
    card.sticky = true;
    card
}

/// The reason of a failed check, in the interface language.
fn describe(error: &CheckError) -> String {
    match error {
        CheckError::Offline(_) => {
            tr("No connection to GitHub: check the network and try again").to_string()
        }
        CheckError::Server(message) => message.clone(),
        CheckError::UnsupportedFormat {
            version: Some(version),
        } => trf(
            "Flux {0} can't be installed by this version: download it from its release page",
            &[version],
        ),
        CheckError::UnsupportedFormat { version: None } => {
            tr("The new version can't be installed by this one: download it from its release page")
                .to_string()
        }
    }
}

/// "2026-10-20" → "October 20, 2026" («20 октября 2026»); anything else as it is.
pub fn release_date(date: &str) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let mut parts = date.trim().splitn(3, '-');
    let parsed = (|| {
        let year: u32 = parts.next()?.parse().ok()?;
        let month: usize = parts.next()?.parse().ok()?;
        let day: u32 = parts.next()?.parse().ok()?;
        Some((year, MONTHS.get(month.checked_sub(1)?)?, day))
    })();
    match parsed {
        Some((year, month, day)) => trf("{0} {1}, {2}", &[&month_name(month), &day, &year]),
        None => date.to_string(),
    }
}

fn month_name(month: &str) -> &'static str {
    match month {
        "January" => tr("January"),
        "February" => tr("February"),
        "March" => tr("March"),
        "April" => tr("April"),
        "May" => tr("May"),
        "June" => tr("June"),
        "July" => tr("July"),
        "August" => tr("August"),
        "September" => tr("September"),
        "October" => tr("October"),
        "November" => tr("November"),
        _ => tr("December"),
    }
}

// --- The actions ---

fn show_release_notes(cx: &mut App) {
    if let Some(manifest) = updates(cx).and_then(|updates| updates.read(cx).release.clone()) {
        open_notes(manifest, cx);
    }
}

/// The release notes of `manifest` in the active project window (after the current update: the
/// notes read [`Updates`]).
fn open_notes(manifest: Manifest, cx: &mut App) {
    in_active_workspace(cx, move |workspace, window, cx| {
        update_notes::open(workspace, manifest, window, cx)
    });
}

/// Update and Restart: from a card, the release notes, Settings.
pub fn install_update(cx: &mut App) {
    let window = active_workspace_window(cx);
    with_updates(cx, |updates, cx| updates.install(window, cx));
}

pub fn skip_update(cx: &mut App) {
    with_updates(cx, |updates, cx| updates.skip(cx));
}

pub fn restart_to_update(cx: &mut App) {
    with_updates(cx, |updates, cx| updates.restart(cx));
}

pub fn cancel_update(cx: &mut App) {
    with_updates(cx, |updates, cx| updates.cancel(cx));
}

fn open_release_page(cx: &mut App) {
    let Some(manifest) = updates(cx).and_then(|updates| updates.read(cx).release.clone()) else {
        return;
    };
    cx.open_url(&release_page(&manifest));
}

/// The page of a release: its notes, its downloads.
pub fn release_page(manifest: &Manifest) -> String {
    manifest
        .notes_url
        .clone()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| flux_update::release_page(&manifest.version))
}

/// Flux can't replace itself here: why, and the way to update by hand.
fn explain_cannot_install(
    reason: CannotInstall,
    manifest: Manifest,
    window: Option<gpui::AnyWindowHandle>,
    cx: &mut App,
) {
    let message = match &reason {
        CannotInstall::NotBundled => tr(
            "This Flux doesn't run from an app bundle (a build from the sources). Download the new version from its release page.",
        )
        .to_string(),
        CannotInstall::ReadOnly(folder) => trf(
            "Flux can't write to {0}. Download the new version and replace Flux yourself, or move Flux to a folder you can change, such as Applications.",
            &[&crate::workspace::tilde(folder)],
        ),
        CannotInstall::Translocated => tr(
            "Flux runs from the disk image or another read-only place. Move Flux to the Applications folder, open it from there and update again.",
        )
        .to_string(),
    };
    let download = manifest
        .platform()
        .and_then(|platform| platform.installer.clone())
        .unwrap_or_else(|| release_page(&manifest));
    let Some(window) = window.or_else(|| active_workspace_window(cx)) else {
        cx.open_url(&download);
        return;
    };
    // An action's handler runs while its window is being updated: the dialog comes after.
    cx.defer(move |cx| show_cannot_install(window, message, download, cx));
}

fn show_cannot_install(
    window: gpui::AnyWindowHandle,
    message: String,
    download: String,
    cx: &mut App,
) {
    window
        .update(cx, |_, window, cx| {
            let answer = Dialog::warning(tr("Flux can't update itself here"))
                .message(message)
                .primary(tr("Download…"))
                .cancel(tr("Cancel"))
                .show(window, cx);
            cx.spawn(async move |cx| {
                if answer.await == Some(0) {
                    cx.update(|cx| cx.open_url(&download)).ok();
                }
            })
            .detach();
        })
        .ok();
}

/// Right before the process exits: the update downloaded for a restart goes in.
fn install_on_quit(cx: &mut App) {
    let Some(updates) = updates(cx) else {
        return;
    };
    let updates = updates.read(cx);
    if !updates.restart_requested {
        return;
    }
    let Phase::Ready(prepared) = &updates.phase else {
        return;
    };
    match install::schedule(prepared, std::process::id(), &[]) {
        Ok(()) => log::info!(
            "updates: installing Flux {} after quitting",
            prepared.version
        ),
        Err(error) => log::error!("updates: can't install Flux {}: {error}", prepared.version),
    }
}

// --- Cards and windows ---

/// The project window in front, or the first one.
fn active_workspace_window(cx: &App) -> Option<gpui::AnyWindowHandle> {
    cx.active_window()
        .into_iter()
        .chain(cx.windows())
        .find(|window| window.downcast::<Workspace>().is_some())
}

fn in_active_workspace(
    cx: &mut App,
    f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) {
    let Some(window) =
        active_workspace_window(cx).and_then(|window| window.downcast::<Workspace>())
    else {
        return;
    };
    cx.defer(move |cx| {
        window.update(cx, f).ok();
    });
}

/// A card in the active project window; with none open yet (the welcome guide alone), in the
/// first one that opens. After the current update: an action's handler runs while its window is
/// being updated.
fn offer(notification: Notification, cx: &mut App) {
    cx.defer(move |cx| offer_now(notification, cx));
}

fn offer_now(notification: Notification, cx: &mut App) {
    if let Some(window) =
        active_workspace_window(cx).and_then(|window| window.downcast::<Workspace>())
    {
        window
            .update(cx, |workspace, _, cx| workspace.notify(notification, cx))
            .ok();
        return;
    }
    let pending = Rc::new(RefCell::new(Some(notification)));
    cx.observe_new(
        move |_: &mut Workspace, _: Option<&mut Window>, cx: &mut Context<Workspace>| {
            let Some(notification) = pending.borrow_mut().take() else {
                return;
            };
            cx.defer(move |cx| {
                if let Some(window) =
                    active_workspace_window(cx).and_then(|window| window.downcast::<Workspace>())
                {
                    window
                        .update(cx, |workspace, _, cx| workspace.notify(notification, cx))
                        .ok();
                }
            });
        },
    )
    .detach();
}

/// The update's cards no longer apply (it is on its way, skipped, or for another channel).
fn dismiss_cards(cx: &mut App) {
    cx.defer(dismiss_cards_now);
}

fn dismiss_cards_now(cx: &mut App) {
    let names = [
        "updates::InstallUpdate",
        "updates::RestartToUpdate",
        "updates::ShowReleaseNotes",
    ];
    for window in cx.windows() {
        if let Some(window) = window.downcast::<Workspace>() {
            window
                .update(cx, |workspace, _, cx| {
                    workspace.dismiss_notifications(&names, cx)
                })
                .ok();
        }
    }
}

/// Flux checks by itself: as the settings say; a scenario only with a feed of its own.
fn automatic(cx: &App) -> bool {
    if std::env::var_os("FLUX_SCENARIO").is_some()
        && std::env::var_os("FLUX_UPDATE_FEED").is_none_or(|feed| feed.is_empty())
    {
        return false;
    }
    settings::updates(cx).check_automatically
}

// --- The status bar ---

/// The download in the status bar: «Flux 0.2.0 · 42 %» over a thin bar, then «Restart to update»;
/// a click opens Cancel (or Restart Now) under it.
pub fn status_item(cx: &Context<Workspace>) -> Option<AnyElement> {
    let updates = updates(cx)?;
    let ui = Theme::ui(cx);
    let (icon_name, color, label, fraction, tooltip) = match updates.read(cx).phase() {
        Phase::Idle => return None,
        Phase::Downloading {
            version,
            done,
            total,
            ..
        } => {
            let fraction = if *total > 0 {
                (*done as f32 / *total as f32).clamp(0., 1.)
            } else {
                0.
            };
            (
                IconName::Download,
                ui.accent_text,
                format!("Flux {version} · {}%", (fraction * 100.).floor() as u32),
                Some(fraction),
                trf("Downloading Flux {0}", &[version]),
            )
        }
        Phase::Preparing { version } => (
            IconName::Download,
            ui.accent_text,
            tr("Checking the update…").to_string(),
            Some(1.),
            trf("Checking the download of Flux {0}", &[version]),
        ),
        Phase::Ready(prepared) => (
            IconName::Update,
            ui.success,
            tr("Restart to update").to_string(),
            None,
            trf("Flux {0} installs when Flux restarts", &[&prepared.version]),
        ),
    };
    let bar = fraction.map(|fraction| {
        div()
            .absolute()
            .left(px(6.))
            .right(px(6.))
            .bottom(px(1.))
            .h(px(2.))
            .rounded(px(1.))
            .bg(ui.input_border)
            // The download comes in steps: the bar glides from one to the next (`crate::motion`).
            .child(
                div()
                    .h_full()
                    .rounded(px(1.))
                    .bg(ui.accent)
                    .with_transition(
                        "download-fill",
                        fraction,
                        crate::motion::SMOOTH,
                        |fill, fraction| fill.w(relative(fraction)),
                    ),
            )
    });
    Some(
        div()
            .flex_none()
            .pr_1()
            .child(
                div()
                    .id("status-update")
                    .relative()
                    .h(px(20.))
                    .px_1p5()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .rounded(px(RADIUS_SM))
                    .cursor_pointer()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(color)
                    .hover(move |style| style.bg(ui.hover))
                    .tooltip(ui::tooltip(tooltip, None))
                    .on_click(cx.listener(|workspace, event: &ClickEvent, window, cx| {
                        open_status_menu(workspace, event, window, cx)
                    }))
                    .child(icon(icon_name, color).size(px(13.)))
                    .child(div().whitespace_nowrap().child(label))
                    .children(bar),
            )
            .into_any_element(),
    )
}

/// The status item's menu: Cancel while downloading, Restart Now once ready.
fn open_status_menu(
    workspace: &mut Workspace,
    event: &ClickEvent,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(updates) = updates(cx) else {
        return;
    };
    let phase = updates.read(cx).phase().clone();
    let menu = cx.new(|cx| {
        let menu = crate::context_menu::ContextMenu::new(window, cx);
        match phase {
            Phase::Downloading { .. } | Phase::Preparing { .. } => {
                menu.entry(tr("Cancel Update"), CancelUpdate)
            }
            Phase::Ready(_) => menu
                .entry(tr("Restart Now"), RestartToUpdate)
                .entry(tr("What's New"), ShowReleaseNotes),
            Phase::Idle => menu,
        }
    });
    // Above the status bar, as its other popups: the menu opens upwards from the click.
    let position = event.position();
    let position = gpui::point(
        position.x,
        position.y
            - px(if matches!(phase, Phase::Ready(_)) {
                74.
            } else {
                44.
            }),
    );
    workspace.show_menu(menu, position, window, cx);
}

// --- updates.json ---

/// What Flux remembers between launches about updates.
#[derive(Debug, Clone, Default, PartialEq)]
struct State {
    /// The version the user skipped ("0.2.0").
    skipped: Option<String>,
    /// When Flux last asked for a new version (Unix seconds).
    last_check: Option<u64>,
    path: Option<PathBuf>,
}

impl State {
    fn load() -> State {
        let path = state_path_for(
            std::env::var_os("FLUX_UPDATES_FILE"),
            std::env::var_os("FLUX_SCENARIO").is_some(),
            std::env::var_os("HOME"),
        );
        let mut state = path
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .map(|value| parse_state(&value))
            .unwrap_or_default();
        state.path = path;
        state
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let text = serde_json::to_string_pretty(&state_json(self)).unwrap_or_default();
        if let Err(error) = write_atomically(path, &text) {
            log::warn!("updates: can't write {}: {error}", path.display());
        }
    }
}

fn parse_state(value: &Value) -> State {
    State {
        skipped: value["skipped"].as_str().map(str::to_string),
        last_check: value["last_check"].as_u64(),
        path: None,
    }
}

fn state_json(state: &State) -> Value {
    json!({ "skipped": state.skipped, "last_check": state.last_check })
}

/// `FLUX_UPDATES_FILE`, else next to `settings.json`; a scenario without the variable keeps
/// nothing (as it keeps no settings).
fn state_path_for(
    explicit: Option<OsString>,
    scenario: bool,
    home: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(file) = explicit.filter(|file| !file.is_empty()) {
        return Some(file.into());
    }
    if scenario {
        return None;
    }
    Some(PathBuf::from(home?).join("Library/Application Support/flux/updates.json"))
}

/// Through a temporary file and a `rename`: never half-written.
fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("json.flux-tmp");
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, path).inspect_err(|_| {
        std::fs::remove_file(&temp).ok();
    })
}

/// When the next automatic check is due (Unix seconds): a day after the last one, or an hour after
/// it at launch.
fn due(last_check: Option<u64>, launch: bool) -> u64 {
    let interval = if launch { LAUNCH_FLOOR } else { INTERVAL };
    last_check.map_or(0, |last| last.saturating_add(interval.as_secs()))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// `FLUX_SCENARIO_UPDATE=downloading:0.42` (or `preparing`, `ready`) with `FLUX_UPDATE_FEED`: the
/// status bar shows that phase of the feed's version from the start — a scenario can't hold a real
/// download still for a screenshot.
#[cfg(feature = "scenario")]
fn scenario_phase(updates: &mut Updates) {
    let Ok(phase) = std::env::var("FLUX_SCENARIO_UPDATE") else {
        return;
    };
    let version = std::env::var("FLUX_UPDATE_FEED")
        .ok()
        .map(|feed| feed.trim_start_matches("file://").to_string())
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| Manifest::parse(&bytes).ok())
        .map(|manifest| {
            updates.release = Some(manifest.clone());
            manifest.version
        })
        .unwrap_or_else(|| Version::new(0, 2, 0));
    let (name, fraction) = phase.split_once(':').unwrap_or((phase.as_str(), "0"));
    updates.phase = match name {
        "downloading" => {
            let total = 31_457_280;
            let fraction: f64 = fraction.parse().unwrap_or(0.);
            Phase::Downloading {
                version,
                done: (total as f64 * fraction) as u64,
                total,
                cancel: Arc::new(AtomicBool::new(false)),
            }
        }
        "preparing" => Phase::Preparing { version },
        "ready" => Phase::Ready(Prepared {
            app: PathBuf::from("/tmp/Flux.app"),
            target: PathBuf::from("/Applications/Flux.app"),
            version,
        }),
        _ => Phase::Idle,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_file_round_trips() {
        let state = State {
            skipped: Some("0.2.0".into()),
            last_check: Some(1_760_000_000),
            path: None,
        };
        assert_eq!(parse_state(&state_json(&state)), state);
        assert_eq!(parse_state(&json!({})), State::default());
        assert_eq!(
            parse_state(&json!({ "skipped": 3, "last_check": "x" })),
            State::default()
        );
    }

    #[test]
    fn the_state_file_lives_next_to_the_settings() {
        assert_eq!(
            state_path_for(None, false, Some("/Users/me".into())),
            Some(PathBuf::from(
                "/Users/me/Library/Application Support/flux/updates.json"
            ))
        );
        assert_eq!(
            state_path_for(Some("/tmp/u.json".into()), true, None),
            Some(PathBuf::from("/tmp/u.json"))
        );
        assert_eq!(state_path_for(None, true, Some("/Users/me".into())), None);
    }

    #[test]
    fn release_dates_read_as_words() {
        assert_eq!(release_date("2026-10-20"), "October 20, 2026");
        assert_eq!(release_date("2026-1-5"), "January 5, 2026");
        assert_eq!(release_date("soon"), "soon");
        assert_eq!(release_date("2026-13-01"), "2026-13-01");
    }

    #[test]
    fn a_launch_checks_unless_the_last_check_is_fresh() {
        let last = 1_760_000_000;
        assert_eq!(due(None, true), 0);
        assert_eq!(due(None, false), 0);
        // At launch: an hour after the last check, not a day.
        assert_eq!(due(Some(last), true), last + 60 * 60);
        // Afterwards: once a day.
        assert_eq!(due(Some(last), false), last + 24 * 60 * 60);
    }

    #[test]
    fn a_check_asked_for_outranks_the_daily_one() {
        assert!(Mode::Manual > Mode::Inline && Mode::Inline > Mode::Auto);
    }
}
