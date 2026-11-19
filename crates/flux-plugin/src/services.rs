//! The plugins' services (API 0.5): who takes a message. A plugin with `service = true` runs one
//! more instance for the whole app — the service — next to the instance in each window; they
//! talk by messages (`service.send`) that go straight from one instance's thread into the other's
//! queue ([`EventSender`]), in order, without the UI thread.
//!
//! Every instance of such a plugin joins the plugin's members when its thread starts — before
//! `activate`, so what it sends there already reaches the others — and leaves when the thread
//! ends. The service hears `window-opened` when a window's instance joins (and `window-closed`
//! when it leaves, or when its plugin restarted on a new project); every window's instance hears
//! `service-started` once the service's `activate` has returned. A service that starts after the
//! windows finds them with `service.windows()`.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::api::events::Event;
use crate::runtime::{EventSender, InstanceRole};

/// The instances of the plugins with a service, by plugin id.
fn members() -> &'static Mutex<HashMap<Arc<str>, Members>> {
    static MEMBERS: OnceLock<Mutex<HashMap<Arc<str>, Members>>> = OnceLock::new();
    MEMBERS.get_or_init(Default::default)
}

#[derive(Default)]
struct Members {
    service: Option<Member>,
    /// By window id.
    windows: BTreeMap<u64, Member>,
}

/// An instance's queue; the token tells an instance from the one that replaced it.
struct Member {
    token: u64,
    events: EventSender,
}

/// An instance's place among its plugin's members: it leaves when dropped (the thread ended).
pub(crate) struct Membership {
    plugin: Arc<str>,
    role: InstanceRole,
    token: u64,
}

/// The instance of a plugin with a service joins: a window's instance is told to the service
/// (`window-opened`; one that replaces an instance of the same window — a restart — is told as
/// `window-closed` first).
pub(crate) fn join(plugin: &Arc<str>, role: InstanceRole, events: &EventSender) -> Membership {
    static TOKENS: AtomicU64 = AtomicU64::new(1);
    let token = TOKENS.fetch_add(1, Ordering::Relaxed);
    let member = Member {
        token,
        events: events.clone(),
    };
    let mut all = members().lock().unwrap();
    let members = all.entry(plugin.clone()).or_default();
    match role {
        InstanceRole::Service => members.service = Some(member),
        InstanceRole::Window(window) => {
            let replaced = members.windows.insert(window, member).is_some();
            if let Some(service) = &members.service {
                if replaced {
                    service.events.send(Event::WindowClosed(window));
                }
                service.events.send(Event::WindowOpened(window));
            }
        }
    }
    Membership {
        plugin: plugin.clone(),
        role,
        token,
    }
}

impl Membership {
    /// The instance's `activate` returned: the service's start is told to every window's
    /// instance (`service-started`).
    pub(crate) fn started(&self) {
        if self.role != InstanceRole::Service {
            return;
        }
        let all = members().lock().unwrap();
        let Some(members) = all.get(&self.plugin) else {
            return;
        };
        if members
            .service
            .as_ref()
            .is_some_and(|service| service.token == self.token)
        {
            for window in members.windows.values() {
                window.events.send(Event::ServiceStarted);
            }
        }
    }

    /// A window's instance started again in a new sandbox (a new project root): for the service
    /// it is a new instance — `window-closed`, then `window-opened`.
    pub(crate) fn restarted(&self) {
        let InstanceRole::Window(window) = self.role else {
            return;
        };
        let all = members().lock().unwrap();
        if let Some(service) = all.get(&self.plugin).and_then(|m| m.service.as_ref()) {
            service.events.send(Event::WindowClosed(window));
            service.events.send(Event::WindowOpened(window));
        }
    }
}

impl Drop for Membership {
    fn drop(&mut self) {
        let mut all = members().lock().unwrap();
        let Some(members) = all.get_mut(&self.plugin) else {
            return;
        };
        match self.role {
            InstanceRole::Service => {
                if members
                    .service
                    .as_ref()
                    .is_some_and(|service| service.token == self.token)
                {
                    members.service = None;
                }
            }
            InstanceRole::Window(window) => {
                if members
                    .windows
                    .get(&window)
                    .is_some_and(|member| member.token == self.token)
                {
                    members.windows.remove(&window);
                    if let Some(service) = &members.service {
                        service.events.send(Event::WindowClosed(window));
                    }
                }
            }
        }
        if members.service.is_none() && members.windows.is_empty() {
            all.remove(&self.plugin);
        }
    }
}

/// Sends a message from a window's instance to the service: it comes as
/// `service-message(some(window), message)`.
pub(crate) fn to_service(plugin: &str, window: u64, message: String) -> Result<(), String> {
    let all = members().lock().unwrap();
    let service = all.get(plugin).and_then(|members| members.service.as_ref());
    match service {
        Some(service)
            if service
                .events
                .send(Event::ServiceMessage((Some(window), message))) =>
        {
            Ok(())
        }
        _ => Err("The plugin's service isn't running".into()),
    }
}

/// Sends a message from the service to a window's instance, or to every one (none): it comes as
/// `service-message(none, message)`. A message to every window when there is none does nothing.
pub(crate) fn to_windows(plugin: &str, window: Option<u64>, message: String) -> Result<(), String> {
    let all = members().lock().unwrap();
    let windows = all.get(plugin).map(|members| &members.windows);
    match window {
        Some(window) => match windows.and_then(|windows| windows.get(&window)) {
            Some(member) if member.events.send(Event::ServiceMessage((None, message))) => Ok(()),
            _ => Err(format!("The window {window} is gone")),
        },
        None => {
            for member in windows.into_iter().flat_map(BTreeMap::values) {
                member
                    .events
                    .send(Event::ServiceMessage((None, message.clone())));
            }
            Ok(())
        }
    }
}

/// The windows with a running instance of the plugin, by id.
pub(crate) fn windows(plugin: &str) -> Vec<u64> {
    let all = members().lock().unwrap();
    all.get(plugin)
        .map(|members| members.windows.keys().copied().collect())
        .unwrap_or_default()
}

/// The plugin's service has joined (it may still be in `activate`).
pub fn service_running(plugin: &str) -> bool {
    let all = members().lock().unwrap();
    all.get(plugin)
        .is_some_and(|members| members.service.is_some())
}
