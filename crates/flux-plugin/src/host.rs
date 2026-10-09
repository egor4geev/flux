//! The plugin API on Flux's side: the functions of the WIT interfaces a plugin calls, run on the
//! plugin's thread. What needs the window becomes a [`HostCall`] message (documents wait for the
//! window's reply); the rest — the log, translations, the project search, storage, settings — is
//! answered right here. Calls that name something the manifest doesn't declare (a command, a
//! status bar item, a tool window) are skipped with a warning in the plugin's log: that's where
//! the plugin's author looks.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::channel::mpsc::UnboundedSender;
use serde_json::{Map, Value};
use wasmtime_wasi::cli::{IsTerminal, StdoutStream};
use wasmtime_wasi::p2::{OutputStream, Pollable, StreamResult};

use crate::api::bindings::flux::plugin::{
    commands, dialogs, editors, events, i18n, log, notifications, project, settings, status_bar,
    storage, types, ui,
};
use crate::api::types::{EditorInfo, Range};
use crate::log::{Level, PluginLog};
use crate::manifest::ProjectAccess;
use crate::registry::PluginEntry;
use crate::runtime::{EditorCall, EditorReply, HostCall, MessageKind, PluginMessage, State};

/// How long the plugin waits for the window to answer about documents.
const EDITOR_TIMEOUT: Duration = Duration::from_secs(5);
/// The window hears about new log lines at most this often; the rest at the end of the call.
const LOGGED_EVERY: Duration = Duration::from_millis(100);
/// The stderr lines kept for the story of a failed call.
const RECENT_LINES: usize = 40;
/// A line longer than this without a newline is logged as it is.
const MAX_PARTIAL: usize = 64 * 1024;
/// The storage file in the plugin's folder.
const STORAGE: &str = "storage.json";

/// What the API's functions need: the plugin, its window's channel, its settings and storage.
pub(crate) struct HostState {
    pub id: Arc<str>,
    pub entry: Arc<PluginEntry>,
    pub language: String,
    pub root: Option<PathBuf>,
    pub settings: Map<String, Value>,
    pub messages: UnboundedSender<PluginMessage>,
    pub log: PluginLog,
    pub signal: Arc<LogSignal>,
    /// The last id given to a notification or a question.
    pub next_id: u64,
    /// The storage file's content, read on first use.
    pub storage: Option<Map<String, Value>>,
    pub data_dir: PathBuf,
    /// Set when the instance is stopped: a search in progress gives up.
    pub cancel: Arc<AtomicBool>,
}

impl HostState {
    /// A call for the window; dropped once the window has let the instance go (a document call
    /// then gets no reply at once).
    fn call(&self, call: HostCall) {
        if self.cancel.load(Ordering::Relaxed) {
            return;
        }
        let _ = self.messages.unbounded_send(PluginMessage {
            plugin: self.id.clone(),
            kind: MessageKind::Call(call),
        });
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn warn(&self, message: &str) {
        self.log.write(Level::Warn, message);
        self.signal.logged();
    }

    /// Asks the window about documents and waits for the reply; none if the window is gone or
    /// doesn't answer in time.
    fn editor(&self, call: EditorCall) -> Option<EditorReply> {
        let (reply, answer) = std::sync::mpsc::channel();
        self.call(HostCall::Editor { call, reply });
        answer.recv_timeout(EDITOR_TIMEOUT).ok()
    }

    fn done(&self, call: EditorCall) -> Result<(), String> {
        match self.editor(call) {
            Some(EditorReply::Done(result)) => result,
            _ => Err(no_answer()),
        }
    }

    /// Whether the manifest declares the command; warns about one it doesn't.
    fn known_command(&self, command: &str, what: &str) -> bool {
        let known = self.entry.manifest.command(command).is_some();
        if !known {
            self.warn(&format!(
                "{what}: the command \"{command}\" isn't in flux-plugin.toml"
            ));
        }
        known
    }

    fn known_window(&self, window: &str, what: &str) -> bool {
        let known = self.entry.manifest.tool_window(window).is_some();
        if !known {
            self.warn(&format!(
                "{what}: the tool window \"{window}\" isn't in flux-plugin.toml"
            ));
        }
        known
    }

    /// The notification without actions of commands the manifest doesn't declare.
    fn checked(
        &self,
        mut notification: notifications::Notification,
    ) -> notifications::Notification {
        notification
            .actions
            .retain(|action| self.known_command(&action.command, "a notification's action"));
        notification
    }

    fn storage(&mut self) -> &mut Map<String, Value> {
        let file = self.data_dir.join(STORAGE);
        self.storage.get_or_insert_with(|| {
            std::fs::read_to_string(&file)
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .and_then(|value| match value {
                    Value::Object(map) => Some(map),
                    _ => None,
                })
                .unwrap_or_default()
        })
    }

    /// Writes the storage through a temporary file: it is never left half-written.
    fn save_storage(&mut self) {
        let file = self.data_dir.join(STORAGE);
        let temp = self.data_dir.join(format!("{STORAGE}.tmp"));
        let text = serde_json::to_string_pretty(self.storage()).unwrap_or_default();
        let written = std::fs::write(&temp, text).and_then(|()| std::fs::rename(&temp, &file));
        if let Err(err) = written {
            let _ = std::fs::remove_file(&temp);
            self.warn(&format!("Couldn't save the storage: {err}"));
        }
    }
}

fn no_answer() -> String {
    "Flux didn't answer".into()
}

impl types::Host for State {}

impl events::Host for State {}

impl log::Host for State {
    fn write(&mut self, level: log::Level, message: String) {
        self.host.log.write(level, &message);
        self.host.signal.logged();
    }
}

impl i18n::Host for State {
    fn language(&mut self) -> String {
        self.host.language.clone()
    }

    fn translate(&mut self, text: String) -> String {
        let host = &self.host;
        host.entry.translate(&host.language, &text).to_string()
    }
}

impl commands::Host for State {
    fn set_enabled(&mut self, command: String, enabled: bool) {
        if self.host.known_command(&command, "commands.set-enabled") {
            self.host
                .call(HostCall::SetCommandEnabled { command, enabled });
        }
    }
}

impl notifications::Host for State {
    fn notify(&mut self, notification: notifications::Notification) -> u64 {
        let id = self.host.next_id();
        let notification = self.host.checked(notification);
        self.host.call(HostCall::Notify { id, notification });
        id
    }

    fn update(&mut self, id: u64, notification: notifications::Notification) {
        let notification = self.host.checked(notification);
        self.host
            .call(HostCall::UpdateNotification { id, notification });
    }

    fn set_progress(&mut self, id: u64, value: Option<notifications::Progress>) {
        self.host.call(HostCall::SetProgress {
            id,
            progress: value,
        });
    }

    fn expire(&mut self, id: u64) {
        self.host.call(HostCall::ExpireNotification { id });
    }

    fn remove(&mut self, id: u64) {
        self.host.call(HostCall::RemoveNotification { id });
    }
}

impl dialogs::Host for State {
    fn ask(&mut self, question: dialogs::Question) -> u64 {
        let id = self.host.next_id();
        self.host.call(HostCall::Ask { id, question });
        id
    }

    fn ask_text(&mut self, question: dialogs::TextQuestion) -> u64 {
        let id = self.host.next_id();
        self.host.call(HostCall::AskText { id, question });
        id
    }
}

impl editors::Host for State {
    fn active(&mut self) -> Option<EditorInfo> {
        match self.host.editor(EditorCall::Active) {
            Some(EditorReply::Editor(info)) => info,
            _ => None,
        }
    }

    fn list(&mut self) -> Vec<EditorInfo> {
        match self.host.editor(EditorCall::List) {
            Some(EditorReply::Editors(editors)) => editors,
            _ => Vec::new(),
        }
    }

    fn text(&mut self, editor: u64) -> Option<String> {
        match self.host.editor(EditorCall::Text(editor)) {
            Some(EditorReply::Text(text)) => text,
            _ => None,
        }
    }

    fn selections(&mut self, editor: u64) -> Vec<Range> {
        match self.host.editor(EditorCall::Selections(editor)) {
            Some(EditorReply::Selections(selections)) => selections,
            _ => Vec::new(),
        }
    }

    fn set_selections(&mut self, editor: u64, selections: Vec<Range>) -> Result<(), String> {
        self.host
            .done(EditorCall::SetSelections(editor, selections))
    }

    fn edit(&mut self, editor: u64, edits: Vec<editors::TextEdit>) -> Result<(), String> {
        self.host.done(EditorCall::Edit(editor, edits))
    }

    fn open(&mut self, path: String, selection: Option<Range>) -> Result<u64, String> {
        match self.host.editor(EditorCall::Open(path, selection)) {
            Some(EditorReply::Opened(result)) => result,
            _ => Err(no_answer()),
        }
    }

    fn save(&mut self, editor: u64) -> Result<(), String> {
        self.host.done(EditorCall::Save(editor))
    }
}

impl project::Host for State {
    fn root(&mut self) -> Option<String> {
        self.host
            .root
            .as_ref()
            .map(|root| root.to_string_lossy().into_owned())
    }

    fn search(
        &mut self,
        query: project::Query,
        max_matches: u32,
    ) -> Result<project::SearchResult, String> {
        let host = &self.host;
        if host.entry.manifest.permissions.project == ProjectAccess::None {
            return Err(
                "The plugin may not read the project: no `project` in the permissions of \
                 flux-plugin.toml"
                    .into(),
            );
        }
        let root = host
            .root
            .clone()
            .ok_or_else(|| "The window has no project".to_string())?;
        let query = flux_search::SearchQuery {
            text: query.text,
            case_sensitive: query.case_sensitive,
            whole_word: query.whole_word,
            regex: query.regex,
        };
        let mut options = flux_search::GrepOptions::default();
        if max_matches > 0 {
            options.max_matches = max_matches as usize;
        }
        let found = Mutex::new(Vec::new());
        let summary = flux_search::search_project(&root, &query, &options, &host.cancel, |file| {
            found.lock().unwrap().push(file)
        })
        .map_err(|err| err.message)?;
        let mut files = found.into_inner().unwrap();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let files = files
            .into_iter()
            .map(|file| project::FileMatches {
                path: file.path.to_string_lossy().into_owned(),
                lines: file
                    .lines
                    .into_iter()
                    .map(|line| project::LineMatch {
                        line: line.line as u32,
                        text: line.text,
                        column_offset: line.column_offset as u32,
                        ranges: line
                            .ranges
                            .into_iter()
                            .map(|range| (range.start as u32, range.end as u32))
                            .collect(),
                    })
                    .collect(),
            })
            .collect();
        Ok(project::SearchResult {
            files,
            truncated: summary.truncated,
        })
    }
}

impl storage::Host for State {
    fn get(&mut self, key: String) -> Option<String> {
        match self.host.storage().get(&key)? {
            Value::String(value) => Some(value.clone()),
            other => Some(other.to_string()),
        }
    }

    fn set(&mut self, key: String, value: Option<String>) {
        let storage = self.host.storage();
        match value {
            Some(value) => {
                storage.insert(key, Value::String(value));
            }
            None => {
                storage.remove(&key);
            }
        }
        self.host.save_storage();
    }

    fn data_dir(&mut self) -> String {
        self.host.data_dir.to_string_lossy().into_owned()
    }
}

impl settings::Host for State {
    fn get(&mut self, key: String) -> Option<String> {
        let host = &self.host;
        host.settings
            .get(&key)
            .or_else(|| host.entry.manifest.setting(&key).map(|spec| &spec.default))
            .map(Value::to_string)
    }
}

impl status_bar::Host for State {
    fn set(&mut self, id: String, item: Option<status_bar::StatusItem>) {
        let manifest = &self.host.entry.manifest;
        if !manifest.status_items.iter().any(|known| known.id == id) {
            self.host.warn(&format!(
                "status-bar.set: the item \"{id}\" isn't in flux-plugin.toml"
            ));
            return;
        }
        let item = item.map(|mut item| {
            if let Some(command) = &item.command
                && !self.host.known_command(command, "a status bar item")
            {
                item.command = None;
            }
            item
        });
        self.host.call(HostCall::SetStatusItem { id, item });
    }
}

impl ui::Host for State {
    fn set_view(&mut self, window: String, view: ui::View) {
        if self.host.known_window(&window, "ui.set-view") {
            self.host.call(HostCall::SetView { window, view });
        }
    }

    fn show(&mut self, window: String) {
        if self.host.known_window(&window, "ui.show") {
            self.host.call(HostCall::ShowToolWindow { window });
        }
    }

    fn hide(&mut self, window: String) {
        if self.host.known_window(&window, "ui.hide") {
            self.host.call(HostCall::HideToolWindow { window });
        }
    }
}

/// Tells the window that the plugin's log has new lines: at most once per [`LOGGED_EVERY`]; a
/// burst in between is told when the call ends ([`LogSignal::flush`]).
pub(crate) struct LogSignal {
    plugin: Arc<str>,
    messages: UnboundedSender<PluginMessage>,
    last: Mutex<Option<Instant>>,
    pending: AtomicBool,
}

impl LogSignal {
    pub fn new(plugin: Arc<str>, messages: UnboundedSender<PluginMessage>) -> Self {
        LogSignal {
            plugin,
            messages,
            last: Mutex::new(None),
            pending: AtomicBool::new(false),
        }
    }

    pub fn logged(&self) {
        let mut last = self.last.lock().unwrap();
        let now = Instant::now();
        if last.is_none_or(|last| now.duration_since(last) >= LOGGED_EVERY) {
            *last = Some(now);
            self.pending.store(false, Ordering::Relaxed);
            self.send();
        } else {
            self.pending.store(true, Ordering::Relaxed);
        }
    }

    /// Tells about the lines held back, if any.
    pub fn flush(&self) {
        if self.pending.swap(false, Ordering::Relaxed) {
            *self.last.lock().unwrap() = Some(Instant::now());
            self.send();
        }
    }

    fn send(&self) {
        let _ = self.messages.unbounded_send(PluginMessage {
            plugin: self.plugin.clone(),
            kind: MessageKind::Logged,
        });
    }
}

/// The plugin's stdout or stderr: lines into its log. Stderr also keeps the last lines of the
/// current call: a panic's message is there when the call traps.
#[derive(Clone)]
pub(crate) struct LogStream(Arc<StreamInner>);

struct StreamInner {
    level: Level,
    log: PluginLog,
    signal: Arc<LogSignal>,
    /// The bytes after the last newline.
    partial: Mutex<Vec<u8>>,
    recent: Mutex<VecDeque<String>>,
}

impl LogStream {
    pub fn new(level: Level, log: PluginLog, signal: Arc<LogSignal>) -> Self {
        LogStream(Arc::new(StreamInner {
            level,
            log,
            signal,
            partial: Mutex::new(Vec::new()),
            recent: Mutex::new(VecDeque::new()),
        }))
    }

    fn push(&self, bytes: &[u8]) {
        let mut partial = self.0.partial.lock().unwrap();
        partial.extend_from_slice(bytes);
        while let Some(newline) = partial.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = partial.drain(..=newline).collect();
            self.line(&line[..line.len() - 1]);
        }
        if partial.len() > MAX_PARTIAL {
            let line = std::mem::take(&mut *partial);
            self.line(&line);
        }
    }

    fn line(&self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        let text = text.trim_end_matches('\r');
        if text.is_empty() {
            return;
        }
        self.0.log.write(self.0.level, text);
        let mut recent = self.0.recent.lock().unwrap();
        if recent.len() == RECENT_LINES {
            recent.pop_front();
        }
        recent.push_back(text.to_string());
        drop(recent);
        self.0.signal.logged();
    }

    /// Logs the bytes after the last newline: the call is over.
    pub fn flush_partial(&self) {
        let line = std::mem::take(&mut *self.0.partial.lock().unwrap());
        if !line.is_empty() {
            self.line(&line);
        }
    }

    /// A new call begins: forget the lines of the previous ones.
    pub fn clear_recent(&self) {
        self.0.recent.lock().unwrap().clear();
    }

    /// The lines of the current call.
    pub fn recent(&self) -> Vec<String> {
        self.0.recent.lock().unwrap().iter().cloned().collect()
    }
}

impl IsTerminal for LogStream {
    fn is_terminal(&self) -> bool {
        false
    }
}

impl StdoutStream for LogStream {
    fn p2_stream(&self) -> Box<dyn OutputStream> {
        Box::new(self.clone())
    }

    fn async_stream(&self) -> Box<dyn tokio::io::AsyncWrite + Send + Sync> {
        Box::new(self.clone())
    }
}

#[wasmtime_wasi::async_trait]
impl Pollable for LogStream {
    async fn ready(&mut self) {}
}

impl OutputStream for LogStream {
    fn write(&mut self, bytes: Bytes) -> StreamResult<()> {
        self.push(&bytes);
        Ok(())
    }

    fn flush(&mut self) -> StreamResult<()> {
        Ok(())
    }

    fn check_write(&mut self) -> StreamResult<usize> {
        Ok(MAX_PARTIAL)
    }
}

impl tokio::io::AsyncWrite for LogStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.push(bytes);
        Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
