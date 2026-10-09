//! Language servers of a window: one per server config and workspace root, started when the first
//! document it serves is opened. Keeps each editor's document open on its server (`didOpen`,
//! `didChange`, `didSave`, `didClose`), routes diagnostics to editors, and reports server status for
//! the status bar. Features (completion, hover, navigation) ask [`document_server`] for the server.
//!
//! The workspace root of a server is the project root; without one (files opened from the start
//! screen), the repository of the file, otherwise its directory. Documents outside the project
//! root (a dependency reached by go to definition) stay on the project's server: servers know
//! their dependencies, and a second rust-analyzer per dependency would be heavy.
//!
//! A server that is not on the machine is installed by Flux, if it knows how (`flux_lsp::install`:
//! npm packages with their own Node.js, GitHub releases, `go install`), with the progress in the
//! status bar; one it can't install says why, quietly. One that exits, or fails to install, is
//! shown in the status bar and is started again by the next document it serves, a click on the
//! status, or the "Restart" command.
//!
//! A document may have several servers (Python: pyright for the language, ruff for formatting and
//! linting): it is open on all of them, the first is the main one (completion, hover, navigation),
//! diagnostics are kept per server.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use flux_core::TextChange;
use flux_lsp::config::{self, ServerConfig};
use flux_lsp::lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
};
use flux_lsp::lsp_types::{
    self, DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, MessageType, PublishDiagnosticsParams, ServerCapabilities,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem, TextDocumentSyncKind,
    Uri, VersionedTextDocumentIdentifier,
};
use flux_lsp::{LanguageServer, ServerEvent, install, position, sync};
use futures::StreamExt;
use futures::channel::mpsc::{self, UnboundedReceiver};
use gpui::{
    App, AppContext, AsyncApp, Context, Div, Entity, EntityId, InteractiveElement, IntoElement,
    ParentElement, Stateful, StatefulInteractiveElement, Styled, Subscription, Task, WeakEntity,
    actions, div, px,
};

use crate::diagnostics;
use crate::editor::Editor;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::theme::UiColors;
use crate::ui;
use crate::workspace::Workspace;

actions!(language_server, [Restart]);

/// How long an error or warning from a server (`window/showMessage`) stays in the status bar.
const MESSAGE_TIMEOUT: Duration = Duration::from_secs(10);
/// Longer status texts are shortened with "…": the status bar shares its width with the position,
/// language, and line endings.
const STATUS_MAX_CHARS: usize = 48;

pub fn init(_cx: &mut App) {
    if let Some(dir) = servers_dir_for(
        std::env::var_os("FLUX_SERVERS_DIR"),
        std::env::var_os("HOME"),
    ) {
        install::set_dir(dir);
    }
}

/// Where Flux installs language servers: `FLUX_SERVERS_DIR` (another directory, for checks),
/// otherwise next to the recent projects in Application Support.
fn servers_dir_for(explicit: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    if let Some(dir) = explicit.filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    Some(PathBuf::from(home?).join("Library/Application Support/flux/servers"))
}

pub struct LspStore {
    /// For the status bar: a click on a stopped server restarts it.
    this: WeakEntity<LspStore>,
    /// Project root: the workspace root of every server, when there is one.
    root: Option<PathBuf>,
    configs: Vec<ServerConfig>,
    servers: Vec<Server>,
    /// Registered editors: each has one release observer, whatever its path changes.
    registered: HashSet<EntityId>,
    next_server_id: u64,
    _subscriptions: Vec<Subscription>,
}

/// Which server: the config and the workspace root.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ServerKey {
    name: String,
    root: PathBuf,
}

struct Server {
    /// Unique per start: events of a server that was replaced are ignored.
    id: u64,
    key: ServerKey,
    status: Status,
    /// Set once the process is spawned (before `initialize` is answered).
    handle: Option<LanguageServer>,
    /// Editors whose documents this server serves; they are opened once it is initialized.
    editors: Vec<WeakEntity<Editor>>,
    /// Diagnostics published for files that are not open in an editor (`cargo check` reports the
    /// whole crate): applied when such a file is opened.
    unopened: HashMap<PathBuf, Vec<lsp_types::Diagnostic>>,
    /// Work in progress (`$/progress`), in order of start.
    progress: Vec<Progress>,
    /// The last error or warning the server showed (`window/showMessage`), for a few seconds.
    message: Option<(MessageType, String)>,
    message_timer: Option<Task<()>>,
    /// Started by the "Restart" command: if it turns out not to be installed, say so.
    announce_unavailable: bool,
    /// Stops an install in progress when the server is dropped (window closed, restarted).
    cancel: Arc<AtomicBool>,
    /// Installs and starts the server, then reads its events; dropping it stops all that.
    _task: Task<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Status {
    /// Spawning, or `initialize` in flight.
    Starting,
    /// Not on the machine: Flux is installing it; the latest progress.
    Installing(Option<install::Progress>),
    Running,
    /// Not installed, and Flux has no way to install it: nothing to show.
    Unavailable,
    /// Not installed, and Flux can't install it here (gopls without Go): a quiet reason.
    CannotInstall(String),
    /// Not installed, and automatic installation is off (Settings): a quiet hint; a click opens
    /// Settings.
    NotInstalled,
    /// Installing failed (no network): an error; tried again like a stopped server.
    InstallFailed(String),
    /// Could not start, or exited; the reason is for the status bar.
    Failed(String),
}

impl Status {
    /// Opening a document the server serves starts it again: it stopped, failed to install, or
    /// couldn't be installed (Go may be there by now).
    fn retried_on_open(&self) -> bool {
        matches!(
            self,
            Status::Failed(_)
                | Status::InstallFailed(_)
                | Status::CannotInstall(_)
                | Status::NotInstalled
        )
    }
}

#[derive(Debug, Clone)]
struct Progress {
    token: String,
    title: Option<String>,
    message: Option<String>,
    percentage: Option<u32>,
}

/// The editor's document on one of its servers (`Editor::lsp`, the main server first): `didOpen`
/// sent, the server initialized. The store itself is `Editor::lsp_store`.
pub struct LspDocument {
    /// The store's id of the server (unique per start).
    pub(crate) server_id: u64,
    pub(crate) server: LanguageServer,
    pub(crate) uri: Uri,
    /// Version of the text the server has: 0 at `didOpen`, +1 per `didChange`.
    pub(crate) version: i32,
    /// Place of the server among the document's servers (`servers_for_path`): 0 — the main one.
    pub(crate) rank: usize,
    /// How the server wants `didChange` (from its capabilities).
    pub(crate) sync: TextDocumentSyncKind,
    /// `didSave`: `None` — not wanted; `Some(true)` — with the text.
    pub(crate) save: Option<bool>,
}

impl LspStore {
    pub fn new(root: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            // The window closed or the project root changed: the servers are no longer needed.
            cx.on_release(|this, cx| {
                for mut server in this.servers.drain(..) {
                    if let Some(handle) = server.handle.take() {
                        cx.background_executor().spawn(handle.shutdown()).detach();
                    }
                }
            }),
            // Quitting: gpui gives quit handlers a short time; a server that doesn't make it sees
            // its stdin closed when flux exits.
            cx.on_app_quit(|this, _| {
                let shutdowns: Vec<_> = this
                    .servers
                    .iter_mut()
                    .filter_map(|server| server.handle.take())
                    .map(|handle| handle.shutdown())
                    .collect();
                async move {
                    futures::future::join_all(shutdowns).await;
                }
            }),
        ];
        Self {
            this: cx.weak_entity(),
            root,
            configs: config::default_servers(),
            servers: Vec::new(),
            registered: HashSet::new(),
            next_server_id: 0,
            _subscriptions: subscriptions,
        }
    }

    /// Opens the editor's document on its servers, starting (or installing) them if needed. Called
    /// by Workspace for every new tab; again (deferred) when the document's path changes.
    pub fn register(&mut self, editor: &Entity<Editor>, cx: &mut Context<Self>) {
        let id = editor.entity_id();
        if self.registered.insert(id) {
            cx.observe_release(editor, move |this, editor, cx| {
                this.released(id, editor, cx)
            })
            .detach();
        }
        for server in &mut self.servers {
            server.editors.retain(|e| e.entity_id() != id);
        }
        let store = cx.weak_entity();
        let path = editor.update(cx, |editor, cx| {
            editor.lsp_store = Some(store);
            // A document still open elsewhere (the store of a previous project root, a restart) is
            // closed; the servers will publish their own diagnostics.
            if !editor.lsp.is_empty() {
                close_documents(editor);
                editor.diagnostics.clear();
                cx.notify();
            }
            editor.document.path().map(Path::to_path_buf)
        });
        let Some(path) = path else {
            return;
        };
        let configs: Vec<ServerConfig> = config::servers_for_path(&self.configs, &path)
            .into_iter()
            .cloned()
            .collect();
        let root = self.server_root(&path);
        for (rank, config) in configs.into_iter().enumerate() {
            let key = ServerKey {
                name: config.name.clone(),
                root: root.clone(),
            };
            let index = match self.servers.iter().position(|server| server.key == key) {
                // Exited, failed to install, or couldn't be installed earlier: opening a document
                // it serves tries again, with its old documents.
                Some(index) if self.servers[index].status.retried_on_open() => {
                    let mut old = self.servers.remove(index);
                    let mut server = self.start_server(config, key, cx);
                    server.editors = std::mem::take(&mut old.editors);
                    self.servers.push(server);
                    self.servers.len() - 1
                }
                Some(index) => index,
                None => {
                    let server = self.start_server(config, key, cx);
                    self.servers.push(server);
                    self.servers.len() - 1
                }
            };
            let server = &mut self.servers[index];
            server.editors.push(editor.downgrade());
            if server.status == Status::Running {
                open_document(server, editor, rank, cx);
            }
        }
        cx.notify();
    }

    /// Stops the server of the editor's document and starts it again (also finds a server installed
    /// since the last attempt). Its documents are opened on the new one.
    pub fn restart(&mut self, editor: &Entity<Editor>, cx: &mut Context<Self>) {
        let Some(path) = editor.read(cx).document.path().map(Path::to_path_buf) else {
            return;
        };
        let names: Vec<String> = config::servers_for_path(&self.configs, &path)
            .iter()
            .map(|config| config.name.clone())
            .collect();
        if names.is_empty() {
            editor.update(cx, |editor, cx| {
                editor.show_status(tr("No language server for this file").into(), cx)
            });
            return;
        }
        let root = self.server_root(&path);
        for name in &names {
            let key = ServerKey {
                name: name.clone(),
                root: root.clone(),
            };
            self.restart_server(&key, vec![editor.downgrade()], cx);
        }
        let message = trf("Restarting {0}…", &[&names.join(", ")]);
        editor.update(cx, |editor, cx| editor.show_status(message.into(), cx));
    }

    /// Every server starts again (also those that were not installed).
    fn restart_all(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<ServerKey> = self
            .servers
            .iter()
            .map(|server| server.key.clone())
            .collect();
        for key in keys {
            self.restart_server(&key, Vec::new(), cx);
        }
    }

    /// The stopped servers, and those that failed to install, start again (a click on the status
    /// bar).
    fn restart_failed(&mut self, cx: &mut Context<Self>) {
        let failed: Vec<ServerKey> = self
            .servers
            .iter()
            .filter(|server| matches!(server.status, Status::Failed(_) | Status::InstallFailed(_)))
            .map(|server| server.key.clone())
            .collect();
        for key in failed {
            self.restart_server(&key, Vec::new(), cx);
        }
    }

    /// Stops the server `key`, if there is one, and registers its editors and `editors` again: on a
    /// new process, found anew in `PATH`.
    fn restart_server(
        &mut self,
        key: &ServerKey,
        mut editors: Vec<WeakEntity<Editor>>,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.servers.iter().position(|server| server.key == *key) {
            let mut server = self.servers.remove(index);
            let id = server.id;
            for editor in server.editors.iter().filter_map(WeakEntity::upgrade) {
                editor.update(cx, |editor, cx| {
                    editor.lsp.retain(|doc| doc.server_id != id);
                    editor.diagnostics.clear_owner(id);
                    cx.notify();
                });
            }
            if let Some(handle) = server.handle.take() {
                cx.background_spawn(handle.shutdown()).detach();
            }
            editors.extend(std::mem::take(&mut server.editors));
        }
        let mut seen = HashSet::new();
        for editor in editors.iter().filter_map(WeakEntity::upgrade) {
            if seen.insert(editor.entity_id()) {
                self.register(&editor, cx);
            }
        }
        if let Some(server) = self.servers.iter_mut().find(|server| server.key == *key) {
            server.announce_unavailable = true;
        }
        cx.notify();
    }

    /// Servers named `name` start again, with their documents: after Settings installed or updated
    /// it.
    pub fn restart_named(&mut self, name: &str, cx: &mut Context<Self>) {
        let keys: Vec<ServerKey> = self
            .servers
            .iter()
            .filter(|server| server.key.name == name)
            .map(|server| server.key.clone())
            .collect();
        for key in keys {
            self.restart_server(&key, Vec::new(), cx);
        }
    }

    /// Servers that weren't installed while automatic installation was off try again: it was just
    /// turned on.
    pub fn retry_not_installed(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<ServerKey> = self
            .servers
            .iter()
            .filter(|server| server.status == Status::NotInstalled)
            .map(|server| server.key.clone())
            .collect();
        for key in keys {
            self.restart_server(&key, Vec::new(), cx);
        }
    }

    /// Stops the servers named `name` (Settings deleted it): their documents stay without it; a
    /// file opened later starts it again (and installs it, if automatic installation is on).
    pub fn stop_named(&mut self, name: &str, cx: &mut Context<Self>) {
        let (stopped, kept): (Vec<Server>, Vec<Server>) = std::mem::take(&mut self.servers)
            .into_iter()
            .partition(|server| server.key.name == name);
        self.servers = kept;
        for mut server in stopped {
            let id = server.id;
            for editor in server.editors.iter().filter_map(WeakEntity::upgrade) {
                editor.update(cx, |editor, cx| {
                    editor.lsp.retain(|doc| doc.server_id != id);
                    editor.diagnostics.clear_owner(id);
                    cx.notify();
                });
            }
            if let Some(handle) = server.handle.take() {
                cx.background_spawn(handle.shutdown()).detach();
            }
        }
        cx.notify();
    }

    /// The workspace root of a server for `path`.
    fn server_root(&self, path: &Path) -> PathBuf {
        if let Some(root) = &self.root {
            return root.clone();
        }
        let dir = path.parent().unwrap_or(path);
        flux_search::find_vcs_root(dir).unwrap_or_else(|| dir.to_path_buf())
    }

    /// Spawns the server in the background (looking for the executable takes a few file checks);
    /// one that is not on the machine is installed first, if Flux can. Then reads its events.
    fn start_server(
        &mut self,
        config: ServerConfig,
        key: ServerKey,
        cx: &mut Context<Self>,
    ) -> Server {
        let id = self.next_server_id;
        self.next_server_id += 1;
        let root = key.root.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let task = cx.spawn({
            let cancel = cancel.clone();
            async move |this, cx| {
                let mut started = launch(&config, &root, cx).await;
                if is_not_found(&started) {
                    if !install_server(id, &config, &cancel, &this, cx).await {
                        return;
                    }
                    started = launch(&config, &root, cx).await;
                    if is_not_found(&started) {
                        let reason = "installed, but the server was not found".to_string();
                        this.update(cx, |this, cx| {
                            this.set_status(id, Status::InstallFailed(reason), cx)
                        })
                        .ok();
                        return;
                    }
                }
                let mut events = match started {
                    Err(err) => {
                        this.update(cx, |this, cx| this.exited(id, err.to_string(), cx))
                            .ok();
                        return;
                    }
                    Ok((handle, events)) => {
                        let spawned = this.update(cx, |this, _| match this.server_mut(id) {
                            Some(server) => {
                                server.handle = Some(handle.clone());
                                true
                            }
                            None => false,
                        });
                        // Replaced or the store is gone while the process was starting.
                        if !matches!(spawned, Ok(true)) {
                            cx.background_spawn(handle.shutdown()).detach();
                            return;
                        }
                        events
                    }
                };
                while let Some(event) = events.next().await {
                    if this
                        .update(cx, |this, cx| this.server_event(id, event, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        });
        Server {
            id,
            key,
            status: Status::Starting,
            handle: None,
            editors: Vec::new(),
            unopened: HashMap::new(),
            progress: Vec::new(),
            message: None,
            message_timer: None,
            announce_unavailable: false,
            cancel,
            _task: task,
        }
    }

    fn set_status(&mut self, id: u64, status: Status, cx: &mut Context<Self>) {
        if let Some(server) = self.server_mut(id) {
            server.status = status;
            cx.notify();
        }
    }

    fn server_mut(&mut self, id: u64) -> Option<&mut Server> {
        self.servers.iter_mut().find(|server| server.id == id)
    }

    /// The executable was not found: silence, unless the user asked for this server explicitly.
    fn unavailable(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(server) = self.server_mut(id) else {
            return;
        };
        server.status = Status::Unavailable;
        if server.announce_unavailable {
            let message = trf("{0} is not installed", &[&server.key.name]);
            for editor in server.editors.iter().filter_map(WeakEntity::upgrade) {
                let message = message.clone().into();
                editor.update(cx, |editor, cx| editor.show_status(message, cx));
            }
        }
        cx.notify();
    }

    fn server_event(&mut self, id: u64, event: ServerEvent, cx: &mut Context<Self>) {
        match event {
            ServerEvent::Initialized => {
                let configs = &self.configs;
                let Some(server) = self.servers.iter_mut().find(|server| server.id == id) else {
                    return;
                };
                server.status = Status::Running;
                let editors: Vec<_> = server
                    .editors
                    .iter()
                    .filter_map(WeakEntity::upgrade)
                    .collect();
                for editor in &editors {
                    let rank = editor
                        .read(cx)
                        .document
                        .path()
                        .map_or(0, |path| rank_of(configs, path, &server.key.name));
                    open_document(server, editor, rank, cx);
                }
                cx.notify();
            }
            ServerEvent::Diagnostics(params) => self.publish_diagnostics(id, params, cx),
            ServerEvent::Progress {
                token,
                title,
                message,
                percentage,
                done,
            } => {
                let Some(server) = self.server_mut(id) else {
                    return;
                };
                let index = server.progress.iter().position(|p| p.token == token);
                match (index, done) {
                    (Some(index), true) => {
                        server.progress.remove(index);
                    }
                    (None, true) => {}
                    (Some(index), false) => {
                        let progress = &mut server.progress[index];
                        progress.title = title.or(progress.title.take());
                        progress.message = message.or(progress.message.take());
                        progress.percentage = percentage.or(progress.percentage);
                    }
                    (None, false) => server.progress.push(Progress {
                        token,
                        title,
                        message,
                        percentage,
                    }),
                }
                cx.notify();
            }
            ServerEvent::Message { kind, text } => {
                if kind != MessageType::ERROR && kind != MessageType::WARNING {
                    return;
                }
                let timer = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(MESSAGE_TIMEOUT).await;
                    this.update(cx, |this, cx| {
                        if let Some(server) = this.server_mut(id) {
                            server.message = None;
                            cx.notify();
                        }
                    })
                    .ok();
                });
                let Some(server) = self.server_mut(id) else {
                    return;
                };
                server.message = Some((kind, text));
                server.message_timer = Some(timer);
                cx.notify();
            }
            ServerEvent::Exited { reason } => self.exited(id, reason, cx),
        }
    }

    /// The server is gone: its documents are no longer open anywhere, their diagnostics are stale.
    fn exited(&mut self, id: u64, reason: String, cx: &mut Context<Self>) {
        let Some(server) = self.server_mut(id) else {
            return;
        };
        server.status = Status::Failed(reason);
        server.handle = None;
        server.progress.clear();
        server.unopened.clear();
        let editors: Vec<_> = server
            .editors
            .iter()
            .filter_map(WeakEntity::upgrade)
            .collect();
        for editor in editors {
            editor.update(cx, |editor, cx| {
                editor.lsp.retain(|doc| doc.server_id != id);
                editor.diagnostics.clear_owner(id);
                cx.notify();
            });
        }
        cx.notify();
    }

    fn publish_diagnostics(
        &mut self,
        id: u64,
        params: PublishDiagnosticsParams,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = position::path_from_uri(&params.uri) else {
            return;
        };
        let Some(server) = self.server_mut(id) else {
            return;
        };
        let editor = server
            .editors
            .iter()
            .filter_map(WeakEntity::upgrade)
            .find(|editor| {
                let editor = editor.read(cx);
                editor.lsp.iter().any(|doc| doc.server_id == id)
                    && editor.document.path() == Some(path.as_path())
            });
        let Some(editor) = editor else {
            if params.diagnostics.is_empty() {
                server.unopened.remove(&path);
            } else {
                server.unopened.insert(path, params.diagnostics);
            }
            return;
        };
        editor.update(cx, |editor, cx| {
            let Some(document) = editor.lsp.iter().find(|doc| doc.server_id == id) else {
                return;
            };
            // For an older text: the server will publish again for the current one; until then
            // the old diagnostics follow the edits.
            if params.version.is_some_and(|v| v != document.version) {
                return;
            }
            let items = diagnostics::from_lsp(editor.document.text(), &params.diagnostics);
            editor.diagnostics.set(id, items);
            cx.notify();
        });
    }

    /// A tab was closed (the editor released): its document is closed on the server.
    fn released(&mut self, id: EntityId, editor: &mut Editor, cx: &mut Context<Self>) {
        self.registered.remove(&id);
        close_documents(editor);
        for server in &mut self.servers {
            server.editors.retain(|e| e.entity_id() != id);
        }
        cx.notify();
    }

    /// What the status bar shows about the servers of the active document (`editor`), or of all
    /// documents (those not installed and not installable say nothing).
    fn status(&self, editor: Option<EntityId>) -> Option<StatusLine> {
        let summaries: Vec<ServerSummary> = self
            .servers
            .iter()
            .filter(|server| server.status != Status::Unavailable)
            .filter(|server| {
                editor.is_none_or(|id| server.editors.iter().any(|e| e.entity_id() == id))
            })
            .map(|server| ServerSummary {
                name: &server.key.name,
                status: &server.status,
                progress: server.progress.first(),
                message: server.message.as_ref(),
            })
            .collect();
        status_line(&summaries)
    }

    /// Why the editor's document has no server to ask yet — starting, being installed, not
    /// installable — for a command that needs one; `None` if no server serves it at all.
    pub fn waiting(&self, editor: EntityId) -> Option<String> {
        let serving = || {
            self.servers
                .iter()
                .filter(move |server| server.editors.iter().any(|e| e.entity_id() == editor))
        };
        // The one that is not running is the reason (pyright being installed next to a running
        // ruff).
        let server = serving()
            .find(|server| server.status != Status::Running)
            .or_else(|| serving().next())?;
        let name = &server.key.name;
        Some(match &server.status {
            Status::Starting | Status::Running => tr("The language server is starting…").into(),
            Status::Installing(_) => trf("Installing {0}…", &[name]),
            Status::Unavailable | Status::CannotInstall(_) | Status::NotInstalled => {
                trf("{0} is not installed", &[name])
            }
            Status::InstallFailed(_) => trf("Couldn't install {0}", &[name]),
            Status::Failed(_) => trf("{0} stopped", &[name]),
        })
    }
}

/// What the status bar needs to know about a server.
struct ServerSummary<'a> {
    name: &'a str,
    status: &'a Status,
    /// The oldest work in progress.
    progress: Option<&'a Progress>,
    message: Option<&'a (MessageType, String)>,
}

/// The most important state among the servers: a stopped server, then a message from a server,
/// then work in progress, then starting; otherwise the names of the running servers.
fn status_line(servers: &[ServerSummary]) -> Option<StatusLine> {
    if let Some((server, reason)) = servers.iter().find_map(|server| match server.status {
        Status::Failed(reason) => Some((server, reason)),
        _ => None,
    }) {
        return Some(StatusLine {
            text: trf("{0} stopped", &[&server.name]),
            tooltip: Some(format!("{reason}\n{}", tr("Click to restart"))),
            tone: Tone::Error,
            restart: true,
            open_settings: false,
        });
    }
    if let Some((server, reason)) = servers.iter().find_map(|server| match server.status {
        Status::InstallFailed(reason) => Some((server, reason)),
        _ => None,
    }) {
        return Some(StatusLine {
            text: trf("Couldn't install {0}", &[&server.name]),
            tooltip: Some(format!(
                "{}\n{}",
                install_reason(reason),
                tr("Click to retry")
            )),
            tone: Tone::Error,
            restart: true,
            open_settings: false,
        });
    }
    if let Some((server, (kind, text))) = servers
        .iter()
        .find_map(|server| server.message.map(|message| (server, message)))
    {
        let first_line = text.lines().next().unwrap_or_default();
        return Some(StatusLine {
            text: format!("{}: {first_line}", server.name),
            tooltip: Some(text.clone()),
            tone: if *kind == MessageType::ERROR {
                Tone::Error
            } else {
                Tone::Warning
            },
            restart: false,
            open_settings: false,
        });
    }
    if let Some((server, progress)) = servers.iter().find_map(|server| match server.status {
        Status::Installing(progress) => Some((server, progress)),
        _ => None,
    }) {
        let mut text = trf("Installing {0}…", &[&server.name]);
        if let Some(fraction) = progress.as_ref().and_then(|progress| progress.fraction) {
            text = format!("{text} {}%", (fraction.clamp(0., 1.) * 100.).round() as u32);
        }
        return Some(StatusLine {
            text,
            // What is going on right now ("Downloading Node.js"), from the installer.
            tooltip: progress.as_ref().map(|progress| progress.text.clone()),
            tone: Tone::Quiet,
            restart: false,
            open_settings: false,
        });
    }
    if let Some((server, progress)) = servers
        .iter()
        .find_map(|server| server.progress.map(|progress| (server, progress)))
    {
        let detail = match (progress.percentage, &progress.message) {
            (Some(percentage), _) => format!("{percentage}%"),
            (None, Some(message)) => message.clone(),
            (None, None) => String::new(),
        };
        let what = [progress.title.as_deref().unwrap_or_default(), &detail]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        return Some(StatusLine {
            text: if what.is_empty() {
                server.name.to_string()
            } else {
                format!("{} · {what}", server.name)
            },
            tooltip: None,
            tone: Tone::Quiet,
            restart: false,
            open_settings: false,
        });
    }
    if let Some(server) = servers
        .iter()
        .find(|server| *server.status == Status::Starting)
    {
        return Some(StatusLine {
            text: format!("{} · {}", server.name, tr("Starting…")),
            tooltip: None,
            tone: Tone::Quiet,
            restart: false,
            open_settings: false,
        });
    }
    if let Some((server, reason)) = servers.iter().find_map(|server| match server.status {
        Status::CannotInstall(reason) => Some((server, reason)),
        _ => None,
    }) {
        return Some(StatusLine {
            text: format!("{}: {}", server.name, install_reason(reason)),
            tooltip: None,
            tone: Tone::Quiet,
            restart: false,
            open_settings: false,
        });
    }
    if let Some(server) = servers
        .iter()
        .find(|server| *server.status == Status::NotInstalled)
    {
        return Some(StatusLine {
            text: trf("{0} is not installed", &[&server.name]),
            tooltip: Some(tr("Automatic installation is off: install it in Settings").to_string()),
            tone: Tone::Quiet,
            restart: false,
            open_settings: true,
        });
    }
    let mut names: Vec<&str> = servers
        .iter()
        .filter(|server| *server.status == Status::Running)
        .map(|server| server.name)
        .collect();
    names.sort_unstable();
    names.dedup();
    (!names.is_empty()).then(|| StatusLine {
        text: names.join(" · "),
        tooltip: None,
        tone: Tone::Quiet,
        restart: false,
        open_settings: false,
    })
}

/// The installer's reasons (its constants), in the UI language; others — as they are.
pub(crate) fn install_reason(reason: &str) -> String {
    match reason {
        install::NEEDS_GO => tr("Needs Go").into(),
        install::UNSUPPORTED => tr("Can't be installed on this system").into(),
        install::NO_INSTALLER => tr("Flux can't install it").into(),
        install::CANCELED => tr("Installation canceled").into(),
        other => other.to_string(),
    }
}

/// Spawns the server off the UI thread (looking for the executable takes a few file checks).
async fn launch(
    config: &ServerConfig,
    root: &Path,
    cx: &mut AsyncApp,
) -> io::Result<(LanguageServer, UnboundedReceiver<ServerEvent>)> {
    let (config, root) = (config.clone(), root.to_path_buf());
    cx.background_spawn(async move { LanguageServer::start(&config, &root) })
        .await
}

fn is_not_found<T>(started: &io::Result<T>) -> bool {
    matches!(started, Err(err) if err.kind() == io::ErrorKind::NotFound)
}

/// A server that is not on the machine: installs it if Flux can, with the progress in the status
/// bar. `true` — installed, start it; otherwise the status says why not.
async fn install_server(
    id: u64,
    config: &ServerConfig,
    cancel: &Arc<AtomicBool>,
    this: &WeakEntity<LspStore>,
    cx: &mut AsyncApp,
) -> bool {
    if config.install.is_none() {
        this.update(cx, |this, cx| this.unavailable(id, cx)).ok();
        return false;
    }
    let automatic = this
        .update(cx, |_, cx| crate::settings::auto_install_servers(cx))
        .unwrap_or(false);
    if !automatic {
        this.update(cx, |this, cx| this.set_status(id, Status::NotInstalled, cx))
            .ok();
        return false;
    }
    let checked = cx
        .background_spawn({
            let config = config.clone();
            async move { install::check(&config) }
        })
        .await;
    if let Err(reason) = checked {
        this.update(cx, |this, cx| {
            this.set_status(id, Status::CannotInstall(reason), cx)
        })
        .ok();
        return false;
    }
    let installing = this.update(cx, |this, cx| {
        this.set_status(id, Status::Installing(None), cx)
    });
    if installing.is_err() {
        return false;
    }
    let (sender, mut receiver) = mpsc::unbounded();
    let installed = cx.background_spawn({
        let config = config.clone();
        let cancel = cancel.clone();
        async move {
            let progress = move |progress: install::Progress| {
                sender.unbounded_send(progress).ok();
            };
            install::install(&config, &progress, &cancel)
        }
    });
    // Until the install is over (the sender goes with it): of a burst of updates, the latest.
    while let Some(mut progress) = receiver.next().await {
        while let Ok(newer) = receiver.try_recv() {
            progress = newer;
        }
        let shown = this.update(cx, |this, cx| {
            this.set_status(id, Status::Installing(Some(progress)), cx)
        });
        if shown.is_err() {
            return false;
        }
    }
    match installed.await {
        Ok(()) => true,
        Err(reason) => {
            this.update(cx, |this, cx| {
                this.set_status(id, Status::InstallFailed(reason), cx)
            })
            .ok();
            false
        }
    }
}

/// Place of server `name` among the servers of `path`: 0 — the main one.
fn rank_of(configs: &[ServerConfig], path: &Path, name: &str) -> usize {
    config::servers_for_path(configs, path)
        .iter()
        .position(|config| config.name == name)
        .unwrap_or(usize::MAX)
}

/// Sends `didOpen` for the editor's document and attaches it to the server (in rank order among
/// the document's servers), with the diagnostics published before it was opened.
fn open_document(
    server: &mut Server,
    editor: &Entity<Editor>,
    rank: usize,
    cx: &mut Context<LspStore>,
) {
    let Some(handle) = server.handle.clone() else {
        return;
    };
    let capabilities = handle.capabilities().unwrap_or_default();
    let sync = sync::change_kind(&capabilities);
    let save = sync::save_include_text(&capabilities);
    let server_id = server.id;
    let unopened = &mut server.unopened;
    editor.update(cx, |editor, cx| {
        let Some(path) = editor.document.path().map(Path::to_path_buf) else {
            return;
        };
        if editor.lsp.iter().any(|doc| doc.server_id == server_id) {
            return;
        }
        let uri = position::uri_from_path(&path);
        handle.notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: config::language_id(&path).to_string(),
                version: 0,
                text: editor.document.text().to_string(),
            },
        });
        let at = editor.lsp.partition_point(|doc| doc.rank <= rank);
        editor.lsp.insert(
            at,
            LspDocument {
                server_id,
                server: handle,
                uri,
                version: 0,
                rank,
                sync,
                save,
            },
        );
        if let Some(published) = unopened.remove(&path) {
            let items = diagnostics::from_lsp(editor.document.text(), &published);
            editor.diagnostics.set(server_id, items);
            cx.notify();
        }
    });
}

/// Sends `didClose` for the editor's document to all its servers and detaches it.
fn close_documents(editor: &mut Editor) {
    for document in editor.lsp.drain(..) {
        document
            .server
            .notify::<DidCloseTextDocument>(DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier { uri: document.uri },
            });
    }
}

/// The main server of the editor's document and the document's URI, if the document is open on
/// it (while pyright is being installed, ruff alone is no substitute: completion, hover, and
/// navigation wait).
pub fn document_server(editor: &Editor) -> Option<(LanguageServer, Uri)> {
    let doc = editor.lsp.first().filter(|doc| doc.rank == 0)?;
    Some((doc.server.clone(), doc.uri.clone()))
}

/// The first of the document's servers whose capabilities fit (formatting: ruff for Python).
pub fn document_server_with(
    editor: &Editor,
    fits: impl Fn(&ServerCapabilities) -> bool,
) -> Option<(LanguageServer, Uri)> {
    let doc = editor.lsp.iter().find(|doc| {
        doc.server
            .capabilities()
            .is_some_and(|capabilities| fits(&capabilities))
    })?;
    Some((doc.server.clone(), doc.uri.clone()))
}

/// Version of the document on its main server: it changes with every edit, so a request can
/// tell whether its answer is about the current text.
pub fn document_version(editor: &Editor) -> Option<i32> {
    editor.lsp.first().map(|doc| doc.version)
}

/// From `Editor::text_changed`: sends `didChange` to every server of the document right away, so
/// that a request made in the same update already sees the edit.
pub fn text_changed(editor: &mut Editor, changes: &[TextChange]) {
    let mut incremental: Option<Vec<TextDocumentContentChangeEvent>> = None;
    let mut full: Option<TextDocumentContentChangeEvent> = None;
    for index in 0..editor.lsp.len() {
        let content_changes = match editor.lsp[index].sync {
            TextDocumentSyncKind::FULL => {
                vec![
                    full.get_or_insert_with(|| sync::full_change(editor.document.text()))
                        .clone(),
                ]
            }
            TextDocumentSyncKind::INCREMENTAL => incremental
                .get_or_insert_with(|| changes.iter().flat_map(sync::incremental_changes).collect())
                .clone(),
            _ => continue,
        };
        if content_changes.is_empty() {
            continue;
        }
        let doc = &mut editor.lsp[index];
        doc.version += 1;
        doc.server
            .notify::<DidChangeTextDocument>(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: doc.uri.clone(),
                    version: doc.version,
                },
                content_changes,
            });
    }
}

/// From `Editor::save_now` after a successful write.
pub fn saved(editor: &Editor) {
    for doc in &editor.lsp {
        let Some(include_text) = doc.save else {
            continue;
        };
        let text = include_text.then(|| editor.document.text().to_string());
        doc.server
            .notify::<DidSaveTextDocument>(DidSaveTextDocumentParams {
                text_document: TextDocumentIdentifier {
                    uri: doc.uri.clone(),
                },
                text,
            });
    }
}

/// From `Editor::set_path`: the document moved (Save As, rename in the tree). It is closed under the
/// old path right away and opened under the new one — maybe on another server — once the editor's
/// update is over.
pub fn path_changed(editor: &mut Editor, cx: &mut Context<Editor>) {
    close_documents(editor);
    editor.diagnostics.clear();
    let Some(store) = editor.lsp_store.clone() else {
        return;
    };
    let editor = cx.entity();
    cx.defer(move |cx| {
        store
            .update(cx, |store, cx| store.register(&editor, cx))
            .ok();
    });
}

/// The language server actions of a window: "Restart" (from the command palette, wherever the
/// focus is) restarts the server of the active document, or all of them without one.
pub fn workspace_actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(cx.listener(|workspace, _: &Restart, _, cx| {
        let editor = workspace.active_editor();
        workspace.lsp.update(cx, |store, cx| match &editor {
            Some(editor) => store.restart(editor, cx),
            None => store.restart_all(cx),
        });
    }))
}

/// One line of server status for the status bar.
#[derive(Debug, PartialEq)]
struct StatusLine {
    text: String,
    tooltip: Option<String>,
    tone: Tone,
    /// A click restarts the server (it has stopped).
    restart: bool,
    /// A click opens Settings (automatic installation is off).
    open_settings: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tone {
    Quiet,
    Warning,
    Error,
}

/// Status of the active document's servers (`editor`; all servers without one) for the window
/// status bar ("rust-analyzer · Indexing 40%", "Installing pyright… 40%"), if any.
pub fn status_item(
    store: &LspStore,
    editor: Option<EntityId>,
    ui: UiColors,
) -> Option<impl IntoElement + use<>> {
    let status = store.status(editor)?;
    let (color, icon_name) = match status.tone {
        Tone::Quiet => (ui.dim, None),
        Tone::Warning => (ui.warning, Some(IconName::Warning)),
        Tone::Error => (ui.error, Some(IconName::Error)),
    };
    let item: Stateful<Div> = div()
        .id("lsp-status")
        .flex_none()
        .flex()
        .items_center()
        .gap_1p5()
        .whitespace_nowrap()
        .text_color(color)
        .children(icon_name.map(|name| icon(name, color).size(px(13.))))
        .child(shorten(&status.text, STATUS_MAX_CHARS));
    let item = match status.tooltip {
        Some(tooltip) => item.tooltip(ui::tooltip(tooltip, None)),
        None => item,
    };
    Some(if status.restart {
        let store = store.this.clone();
        item.cursor_pointer().on_click(move |_, _, cx| {
            store.update(cx, |store, cx| store.restart_failed(cx)).ok();
        })
    } else if status.open_settings {
        item.cursor_pointer().on_click(|_, window, cx| {
            window.dispatch_action(Box::new(crate::settings_view::Toggle), cx)
        })
    } else {
        item
    })
}

/// `text` cut to `max` characters with "…".
fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut short: String = text.chars().take(max.saturating_sub(1)).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary<'a>(name: &'a str, status: &'a Status) -> ServerSummary<'a> {
        ServerSummary {
            name,
            status,
            progress: None,
            message: None,
        }
    }

    #[test]
    fn status_shows_the_most_important_state() {
        let running = Status::Running;
        let starting = Status::Starting;
        let failed = Status::Failed("exit status: 1".into());
        assert_eq!(status_line(&[]), None);

        let line = status_line(&[summary("rust-analyzer", &running)]).unwrap();
        assert_eq!(
            (line.text.as_str(), line.tone),
            ("rust-analyzer", Tone::Quiet)
        );
        let line = status_line(&[
            summary("gopls", &running),
            summary("rust-analyzer", &running),
        ]);
        assert_eq!(line.unwrap().text, "gopls · rust-analyzer");

        let line = status_line(&[summary("gopls", &starting)]).unwrap();
        assert_eq!(line.text, "gopls · Starting…");

        // A stopped server wins over everything and can be restarted with a click.
        let line = status_line(&[
            summary("gopls", &running),
            summary("rust-analyzer", &failed),
        ])
        .unwrap();
        assert_eq!(line.text, "rust-analyzer stopped");
        assert_eq!((line.tone, line.restart), (Tone::Error, true));
        assert!(line.tooltip.unwrap().starts_with("exit status: 1"));
    }

    #[test]
    fn status_shows_progress_and_messages() {
        let running = Status::Running;
        let indexing = Progress {
            token: "t".into(),
            title: Some("Indexing".into()),
            message: Some("3/40 (serde)".into()),
            percentage: Some(40),
        };
        let fetching = Progress {
            percentage: None,
            title: Some("Fetching".into()),
            message: Some("cargo metadata".into()),
            ..indexing.clone()
        };
        let busy = |progress| ServerSummary {
            progress: Some(progress),
            ..summary("rust-analyzer", &running)
        };
        assert_eq!(
            status_line(&[busy(&indexing)]).unwrap().text,
            "rust-analyzer · Indexing 40%"
        );
        assert_eq!(
            status_line(&[busy(&fetching)]).unwrap().text,
            "rust-analyzer · Fetching cargo metadata"
        );

        let message = (MessageType::WARNING, "Failed to load\ndetails".to_string());
        let line = status_line(&[ServerSummary {
            message: Some(&message),
            ..busy(&indexing)
        }])
        .unwrap();
        assert_eq!(line.text, "rust-analyzer: Failed to load");
        assert_eq!(line.tone, Tone::Warning);
        assert_eq!(line.tooltip.as_deref(), Some("Failed to load\ndetails"));
    }

    #[test]
    fn status_shows_installs() {
        let installing = Status::Installing(Some(install::Progress {
            text: "Downloading Node.js".into(),
            fraction: Some(0.4),
        }));
        let line = status_line(&[summary("pyright", &installing)]).unwrap();
        assert_eq!(line.text, "Installing pyright… 40%");
        assert_eq!(line.tooltip.as_deref(), Some("Downloading Node.js"));
        assert_eq!((line.tone, line.restart), (Tone::Quiet, false));

        let just_started = Status::Installing(None);
        let line = status_line(&[summary("pyright", &just_started)]).unwrap();
        assert_eq!(line.text, "Installing pyright…");

        // Can't be installed here: quiet, with the reason.
        let no_go = Status::CannotInstall(install::NEEDS_GO.into());
        let line = status_line(&[summary("gopls", &no_go)]).unwrap();
        assert_eq!(
            (line.text.as_str(), line.tone),
            ("gopls: Needs Go", Tone::Quiet)
        );

        // A failed install is an error, retried with a click; it wins over an install elsewhere.
        let offline = Status::InstallFailed("no network".into());
        let line =
            status_line(&[summary("ruff", &installing), summary("pyright", &offline)]).unwrap();
        assert_eq!(line.text, "Couldn't install pyright");
        assert_eq!((line.tone, line.restart), (Tone::Error, true));
        assert!(line.tooltip.unwrap().starts_with("no network"));
    }

    #[test]
    fn stopped_and_uninstallable_servers_are_retried_on_open() {
        assert!(Status::Failed("exit".into()).retried_on_open());
        assert!(Status::InstallFailed("offline".into()).retried_on_open());
        assert!(Status::CannotInstall("needs Go".into()).retried_on_open());
        assert!(!Status::Running.retried_on_open());
        assert!(!Status::Installing(None).retried_on_open());
        assert!(!Status::Unavailable.retried_on_open());
    }

    #[test]
    fn servers_live_in_application_support_unless_told_otherwise() {
        let home = Some(OsString::from("/Users/me"));
        assert_eq!(
            servers_dir_for(None, home.clone()),
            Some(PathBuf::from(
                "/Users/me/Library/Application Support/flux/servers"
            ))
        );
        assert_eq!(
            servers_dir_for(Some("/tmp/servers".into()), home.clone()),
            Some(PathBuf::from("/tmp/servers"))
        );
        assert_eq!(
            servers_dir_for(Some("".into()), home),
            Some(PathBuf::from(
                "/Users/me/Library/Application Support/flux/servers"
            ))
        );
        assert_eq!(servers_dir_for(None, None), None);
    }

    #[test]
    fn long_status_is_shortened() {
        assert_eq!(shorten("rust-analyzer", 20), "rust-analyzer");
        assert_eq!(shorten("abcdef", 4), "abc…");
    }
}
