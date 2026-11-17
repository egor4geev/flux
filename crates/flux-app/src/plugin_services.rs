//! The plugins' services (API 0.5, FluxGram part 1): a plugin with `service = true` runs one more
//! instance for the whole app next to the one in each window ([`InstanceRole::Service`]). Its
//! messages to the windows' instances and theirs to it go thread to thread
//! (`flux_plugin::services`); this module owns the service instances and carries out the calls a
//! service makes to the app.
//!
//! - It starts when the first window starts the plugin ([`window_started`]; a window that starts it
//!   later finds it running) — not at launch: a plugin no window runs (all of them closed before it
//!   was turned on) costs nothing. A window that starts a new version of the plugin (a dev reload,
//!   an update, an install over it) starts the service again with it; a stopped service starts
//!   again when a window (re)starts the plugin — the Restart of its notification.
//! - It stops when the plugin is turned off or removed ([`stop`]), and when Flux quits (its
//!   `deactivate` gets up to [`QUIT_WAIT`] to close what it holds). Closing the last window
//!   doesn't stop it.
//! - A service that stops by itself (a panic, a limit) is told like a window's instance: a
//!   notification in the active window with Restart, Disable and Details.
//! - Its notifications go to the window the user works in (and their updates to that window);
//!   the `status` lines of its settings page to every window; `settings-changed` reaches it once
//!   per change; its settings go to `settings.json`; opening a link and copying text go through
//!   the active window. It has no project.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use flux_plugin::api::events::Event;
use flux_plugin::log::{Level, PluginLog};
use flux_plugin::registry::PluginEntry;
use flux_plugin::runtime::{
    HostCall, Instance, InstanceConfig, InstanceRole, MessageKind, PluginMessage,
};
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::{
    App, AppContext as _, Context, Entity, EntityId, Global, SharedString, Task, WeakEntity,
};
use serde_json::{Map, Value};

use crate::i18n::{tr, trf};
use crate::notification_center::{NotificationGroup, NotificationId};
use crate::notifications::{Notification, Progress};
use crate::plugins::{DisablePlugin, PluginStore, RestartPlugin, ShowStopDetails};
use crate::workspace::Workspace;

/// How long Flux waits at quit for the services' `deactivate`.
const QUIT_WAIT: Duration = Duration::from_secs(1);

/// The owner of the services.
pub struct PluginServices {
    services: HashMap<Arc<str>, Service>,
    messages: UnboundedSender<PluginMessage>,
    /// The services' notifications: the window's store that has each.
    notifications: HashMap<(Arc<str>, u64), WeakEntity<PluginStore>>,
    _pump: Task<()>,
    _quit: gpui::Subscription,
}

struct GlobalServices(Entity<PluginServices>);

impl Global for GlobalServices {}

/// A plugin's service.
struct Service {
    /// None once it stopped by itself.
    instance: Option<Instance>,
    /// What it runs: a window that starts another version starts it again.
    version: Version,
    name: String,
    log: PluginLog,
    stopped: Option<(SharedString, SharedString)>,
}

/// The version of a plugin a service runs: its manifest, folder and component.
#[derive(PartialEq)]
struct Version {
    manifest: flux_plugin::manifest::Manifest,
    dir: Option<std::path::PathBuf>,
    component: Option<SystemTime>,
}

impl Version {
    fn of(entry: &PluginEntry) -> Self {
        Version {
            manifest: entry.manifest.clone(),
            dir: entry.files.dir().map(std::path::Path::to_path_buf),
            component: flux_plugin::dev::component_mtime(entry),
        }
    }
}

pub fn init(cx: &mut App) {
    let services = cx.new(|cx: &mut Context<PluginServices>| {
        let (messages, mut receiver) = unbounded::<PluginMessage>();
        let pump = cx.spawn(async move |this, cx| {
            while let Some(message) = receiver.next().await {
                if this
                    .update(cx, |this, cx| this.handle_message(message, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let quit = cx.on_app_quit(|this, _| {
            this.stop_all();
            async {}
        });
        PluginServices {
            services: HashMap::new(),
            messages,
            notifications: HashMap::new(),
            _pump: pump,
            _quit: quit,
        }
    });
    cx.set_global(GlobalServices(services));
}

fn services(cx: &App) -> Option<Entity<PluginServices>> {
    cx.try_global::<GlobalServices>()
        .map(|global| global.0.clone())
}

/// A window starts the plugin (`entry`, with `service = true`): its service starts unless it runs
/// this version already. `log` is the window's log of the plugin: the service writes there too.
pub(crate) fn window_started(
    entry: &Arc<PluginEntry>,
    log: &PluginLog,
    settings: Map<String, Value>,
    cx: &mut App,
) {
    if !entry.manifest.service || entry.manifest.wasm.is_none() {
        return;
    }
    let Some(services) = services(cx) else {
        return;
    };
    services.update(cx, |this, _| this.start(entry, log, settings));
}

/// The plugin was turned off or removed: its service stops.
pub(crate) fn stop(plugin: &str, cx: &mut App) {
    let Some(services) = services(cx) else {
        return;
    };
    services.update(cx, |this, _| {
        if let Some(service) = this.services.remove(plugin) {
            service
                .log
                .write(Level::Info, "The service stopped: the plugin is off");
        }
    });
}

/// The plugin's settings changed (Settings, once per change): the service gets the new values and
/// `settings-changed`.
pub(crate) fn settings_changed(plugin: &str, values: Map<String, Value>, cx: &mut App) {
    let Some(services) = services(cx) else {
        return;
    };
    let services = services.read(cx);
    if let Some(instance) = services
        .services
        .get(plugin)
        .and_then(|service| service.instance.as_ref())
    {
        instance.set_settings(values);
        instance.send_event(Event::SettingsChanged);
    }
}

/// Why the plugin's service stopped by itself: the line and the whole story.
pub(crate) fn stop_reason(plugin: &str, cx: &App) -> Option<(SharedString, SharedString)> {
    services(cx)?.read(cx).services.get(plugin)?.stopped.clone()
}

impl PluginServices {
    fn start(&mut self, entry: &Arc<PluginEntry>, log: &PluginLog, settings: Map<String, Value>) {
        let id: Arc<str> = entry.id().into();
        let version = Version::of(entry);
        if let Some(service) = self.services.get(&id)
            && service.instance.is_some()
            && service.version == version
        {
            return;
        }
        // Another version, or stopped: the old one goes first (its thread ends after its
        // `deactivate`; the new one starts at once).
        self.services.remove(&id);
        log.write(Level::Info, "Starting the service");
        let instance = Instance::start(InstanceConfig {
            entry: entry.clone(),
            role: InstanceRole::Service,
            project_root: None,
            language: crate::i18n::lang_code().to_string(),
            settings,
            messages: self.messages.clone(),
            log: log.clone(),
        });
        self.services.insert(
            id,
            Service {
                instance: Some(instance),
                version,
                name: entry.manifest.name.clone(),
                log: log.clone(),
                stopped: None,
            },
        );
    }

    /// Flux quits: every service gets its `deactivate`, together, for up to [`QUIT_WAIT`].
    fn stop_all(&mut self) {
        let instances: Vec<Instance> = self
            .services
            .drain()
            .filter_map(|(_, service)| service.instance)
            .collect();
        if instances.is_empty() {
            return;
        }
        let (done, ended) = std::sync::mpsc::channel();
        let count = instances.len();
        for instance in instances {
            let done = done.clone();
            std::thread::spawn(move || {
                instance.stop_and_wait(QUIT_WAIT);
                let _ = done.send(());
            });
        }
        let deadline = Instant::now() + QUIT_WAIT;
        for _ in 0..count {
            let left = deadline.saturating_duration_since(Instant::now());
            if ended.recv_timeout(left).is_err() {
                break;
            }
        }
    }

    fn handle_message(&mut self, message: PluginMessage, cx: &mut Context<Self>) {
        let plugin = message.plugin;
        // A message of a service that was stopped since: nobody to talk to.
        if !self
            .services
            .get(&plugin)
            .is_some_and(|service| service.instance.is_some())
        {
            return;
        }
        match message.kind {
            MessageKind::Started => {
                if let Some(service) = self.services.get(&plugin) {
                    service.log.write(Level::Info, "The service runs");
                }
            }
            MessageKind::Logged => {}
            MessageKind::Stopped { error, details } => {
                let Some(service) = self.services.get_mut(&plugin) else {
                    return;
                };
                service.instance = None;
                service.stopped = Some((error.clone().into(), details.into()));
                let name = service.name.clone();
                let id: SharedString = plugin.to_string().into();
                let notification = Notification::error(trf("Plugin «{0}» stopped", &[&name]))
                    .body(error)
                    .group(NotificationGroup::Plugins)
                    .action(tr("Restart"), RestartPlugin(id.clone()))
                    .action(tr("Disable"), DisablePlugin(id.clone()))
                    .action(tr("Details"), ShowStopDetails(id));
                if let Some(store) = active_store(cx) {
                    store.update(cx, |store, cx| store.notify_for_service(notification, cx));
                }
            }
            MessageKind::Call(call) => self.handle_call(plugin, call, cx),
        }
    }

    /// A call of a service (only the ones it may make reach here: `HostState::service_call`).
    fn handle_call(&mut self, plugin: Arc<str>, call: HostCall, cx: &mut Context<Self>) {
        match call {
            HostCall::Notify { id, .. } => {
                let Some(store) = active_store(cx) else {
                    return;
                };
                self.notifications
                    .insert((plugin.clone(), id), store.downgrade());
                store.update(cx, |store, cx| store.service_call(plugin, call, cx));
            }
            HostCall::UpdateNotification { id, .. }
            | HostCall::SetProgress { id, .. }
            | HostCall::ExpireNotification { id }
            | HostCall::RemoveNotification { id } => {
                let key = (plugin.clone(), id);
                let store = match &call {
                    HostCall::RemoveNotification { .. } => self.notifications.remove(&key),
                    _ => self.notifications.get(&key).cloned(),
                };
                if let Some(store) = store.and_then(|store| store.upgrade()) {
                    store.update(cx, |store, cx| store.service_call(plugin, call, cx));
                }
            }
            HostCall::SetSettingStatus { key, status } => {
                for store in all_stores(cx) {
                    let call = HostCall::SetSettingStatus {
                        key: key.clone(),
                        status: status.clone(),
                    };
                    let plugin = plugin.clone();
                    store.update(cx, |store, cx| store.service_call(plugin, call, cx));
                }
            }
            HostCall::SetSetting { key, value } => {
                crate::settings::set_plugin_value(&plugin, &key, value, cx);
            }
            call @ (HostCall::HelperProgress { .. } | HostCall::Window { .. }) => {
                if let Some(store) = active_store(cx) {
                    store.update(cx, |store, cx| store.service_call(plugin, call, cx));
                }
            }
            other => {
                if let Some(service) = self.services.get(&plugin) {
                    service.log.write(
                        Level::Warn,
                        &format!("The service's call isn't carried out: {other:?}"),
                    );
                }
            }
        }
    }
}

/// The plugins' store of the window the user works in (or of the first one).
fn active_store(cx: &App) -> Option<Entity<PluginStore>> {
    cx.active_window()
        .into_iter()
        .chain(cx.windows())
        .find_map(|window| window.downcast::<Workspace>())
        .and_then(|window| {
            window
                .read(cx)
                .ok()
                .map(|workspace| workspace.plugins.clone())
        })
}

/// The plugins' stores of every window.
fn all_stores(cx: &App) -> Vec<Entity<PluginStore>> {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<Workspace>())
        .filter_map(|window| {
            window
                .read(cx)
                .ok()
                .map(|workspace| workspace.plugins.clone())
        })
        .collect()
}

// --- A helper's download in a window ---

/// The notifications of helpers being downloaded: by (the window's store, the plugin, the
/// helper).
#[derive(Default)]
struct HelperDownloads(HashMap<(EntityId, Arc<str>, String), NotificationId>);

impl Global for HelperDownloads {}

/// `process.helper` has been downloading for over a second (`progress`: bytes so far, all bytes),
/// or has finished (none): a notification with the progress, gone when it ends.
pub(crate) fn helper_progress(
    store: &mut PluginStore,
    plugin: &Arc<str>,
    helper: &str,
    progress: Option<(u64, u64)>,
    cx: &mut Context<PluginStore>,
) {
    let key = (cx.entity_id(), plugin.clone(), helper.to_string());
    let Some(center) = store.notification_center() else {
        return;
    };
    let Some((done, total)) = progress else {
        if let Some(id) = cx.default_global::<HelperDownloads>().0.remove(&key) {
            center.update(cx, |center, cx| center.remove(id, cx));
        }
        return;
    };
    let fraction = (total > 0).then(|| (done as f32 / total as f32).clamp(0., 1.));
    let title = match fraction {
        Some(fraction) => {
            let percent = ((fraction * 100.).floor() as u32).to_string();
            trf("Downloading {0}… {1} %", &[&helper, &percent])
        }
        None => trf("Downloading {0}…", &[&helper]),
    };
    let progress = fraction.map_or(Progress::Indeterminate, Progress::Fraction);
    let known = cx.default_global::<HelperDownloads>().0.get(&key).copied();
    match known {
        Some(id) => center.update(cx, |center, cx| {
            center.update(id, |notification| notification.title = title.into(), cx);
            center.set_progress(id, Some(progress), cx);
        }),
        None => {
            let notification = Notification::info(title)
                .progress(progress)
                .group(NotificationGroup::Plugin(plugin.to_string().into()));
            let id = center.update(cx, |center, cx| center.notify(notification, cx));
            cx.default_global::<HelperDownloads>().0.insert(key, id);
        }
    }
}
