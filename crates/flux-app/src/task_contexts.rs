//! Contexts (part 10.3), as Tasks & Contexts in JetBrains: the window's open tabs — documents with
//! their carets and scroll, pictures, Claude's chats in the editor — kept under a name, so a task's
//! tabs come back when the user returns to the task.
//!
//! - Flux's own commands: Save Context… (a name), Load Context… (a popup of the project's contexts,
//!   newest first, ⌘⌫ deletes one), Clear Context (⌥⇧X, as in JetBrains).
//! - Plugins switch them with `contexts.switch(save, load)` ([`switch_for_plugin`]): their keys are
//!   kept apart from other plugins' and the user's, under the titles they give.
//! - Switching saves the open tabs, closes them — documents with unsaved changes, Claude's sessions
//!   at work, terminals, diffs and merges stay — and opens the other context's: documents read in
//!   the background, chats resumed from their transcripts.
//!
//! Contexts live in their own file, `~/Library/Application Support/flux/contexts.json`
//! (`FLUX_CONTEXTS_FILE`; in a scenario without it — in memory only), by project: an older Flux
//! rewrites `projects.json` from its own structure and would drop them there.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{SystemTime, UNIX_EPOCH};

use flux_plugin::api::contexts::ContextRef;
use flux_plugin::runtime::WindowReply;
use flux_search::{FuzzyMatch, match_list};
use gpui::{
    AnyElement, App, Context, DismissEvent, Div, KeyBinding, MouseButton, SharedString, WeakEntity,
    Window, actions, div, prelude::*, px,
};
use serde_json::{Value, json};

use crate::i18n::{tr, trf, trn};
use crate::icons::{IconName, icon};
use crate::input_dialog::InputDialog;
use crate::picker::{self, Picker, PickerDelegate, highlighted_text};
use crate::project_state::SavedTab;
use crate::theme::{self, Theme};
use crate::workspace::Workspace;

actions!(contexts, [SaveContext, LoadContext, ClearContext]);

/// A project keeps this many contexts at most: the ones saved longest ago go first.
const MAX_CONTEXTS: usize = 50;
/// The popup's own key context: ⌘⌫ deletes the selected context (in the query field too).
const CONTEXT: &str = "Picker Contexts";

pub fn init(cx: &mut App) {
    cx.bind_keys([
        // Clear Context, as in JetBrains.
        KeyBinding::new("alt-shift-x", ClearContext, Some("Workspace")),
        KeyBinding::new(
            "cmd-backspace",
            picker::DeleteSelected,
            Some("Contexts > TextInput"),
        ),
        KeyBinding::new("cmd-backspace", picker::DeleteSelected, Some("Contexts")),
    ]);
}

/// The window's commands of contexts.
pub fn workspace_actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(
        cx.listener(|workspace, _: &SaveContext, window, cx| save_context(workspace, window, cx)),
    )
    .on_action(
        cx.listener(|workspace, _: &LoadContext, window, cx| load_context(workspace, window, cx)),
    )
    .on_action(cx.listener(|workspace, _: &ClearContext, window, cx| {
        workspace.close_context_tabs(window, cx)
    }))
}

/// The tabs a context keeps.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Tabs {
    /// Documents and pictures, in order.
    pub files: Vec<SavedTab>,
    /// The active one among `files`.
    pub active: Option<usize>,
    /// Claude's chats in the editor's tabs.
    pub chats: Vec<SavedChat>,
    /// The session of the chat that was the active tab.
    pub active_chat: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SavedChat {
    /// The session's id: its transcript resumes it.
    pub session: String,
    pub title: String,
}

/// A saved context of a project.
#[derive(Debug, Clone, PartialEq)]
struct Saved {
    /// `user:<title>`, `plugin:<plugin>:<the plugin's key>`.
    key: String,
    /// The plugin that keeps it; none — the user's.
    plugin: Option<String>,
    title: String,
    /// Seconds since 1970.
    saved_at: u64,
    tabs: Tabs,
}

fn user_key(title: &str) -> String {
    format!("user:{title}")
}

fn plugin_key(plugin: &str, key: &str) -> String {
    format!("plugin:{plugin}:{key}")
}

// --- Plugins ---

/// `contexts.switch` for a plugin: saves the open tabs as `save`, closes them and opens `load`'s →
/// `Done` (the documents come in the background).
pub(crate) fn switch_for_plugin(
    workspace: &mut Workspace,
    plugin: &str,
    save: Option<ContextRef>,
    load: Option<String>,
    reply: Sender<WindowReply>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(root) = workspace.root().map(Path::to_path_buf) else {
        reply
            .send(WindowReply::Done(Err("The window has no project".into())))
            .ok();
        return;
    };
    if let Some(save) = save {
        let tabs = workspace.context_tabs(cx);
        put(
            &root,
            Saved {
                key: plugin_key(plugin, &save.key),
                plugin: Some(plugin.to_string()),
                title: save.title,
                saved_at: now(),
                tabs,
            },
        );
    }
    workspace.close_context_tabs(window, cx);
    if let Some(saved) = load.and_then(|key| find(&root, &plugin_key(plugin, &key))) {
        open(workspace, &saved, window, cx);
    }
    reply.send(WindowReply::Done(Ok(()))).ok();
}

/// `contexts.remove` for a plugin: forgets its saved context `key`.
pub(crate) fn remove_for_plugin(
    workspace: &mut Workspace,
    plugin: &str,
    key: &str,
    _: &mut Context<Workspace>,
) {
    if let Some(root) = workspace.root() {
        remove(root, &plugin_key(plugin, key));
    }
}

// --- The user's commands ---

/// Save Context…: the open tabs under a name (one already saved under it is replaced).
fn save_context(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    if workspace.root().is_none() {
        return workspace.show_message(tr("Contexts are kept by project").into(), cx);
    }
    let this = cx.entity().downgrade();
    workspace.toggle_dialog(window, cx, move |window, cx| {
        InputDialog::new(tr("Save Context"), tr("Context name"), window, cx)
            .subtitle(tr("The open tabs come back with Load Context"))
            .confirm_label(tr("Save"))
            .validate(
                |name, _, _| {
                    if name.trim().is_empty() {
                        Err(tr("Enter a name").into())
                    } else {
                        Ok(())
                    }
                },
                cx,
            )
            .on_confirm(move |name, _, _, cx| {
                this.update(cx, |workspace, cx| save_named(workspace, name.trim(), cx))
                    .ok();
            })
    });
}

fn save_named(workspace: &mut Workspace, name: &str, cx: &mut Context<Workspace>) {
    let Some(root) = workspace.root().map(Path::to_path_buf) else {
        return;
    };
    let tabs = workspace.context_tabs(cx);
    put(
        &root,
        Saved {
            key: user_key(name),
            plugin: None,
            title: name.to_string(),
            saved_at: now(),
            tabs,
        },
    );
    workspace.show_message(trf("Context “{0}” saved", &[&name]).into(), cx);
}

/// Load Context…: the project's contexts in a popup; the chosen one replaces the open tabs.
fn load_context(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(root) = workspace.root().map(Path::to_path_buf) else {
        return workspace.show_message(tr("Contexts are kept by project").into(), cx);
    };
    let names: HashMap<String, String> = workspace
        .plugins
        .read(cx)
        .plugins()
        .iter()
        .map(|state| (state.id().to_string(), state.name().to_string()))
        .collect();
    let this = cx.entity().downgrade();
    workspace.toggle_modal(window, cx, move |window, cx| {
        Picker::new(ContextList::new(this, root, names), window, cx)
    });
}

/// Replaces the open tabs with a saved context's.
fn switch_to(
    workspace: &mut Workspace,
    saved: &Saved,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    workspace.close_context_tabs(window, cx);
    open(workspace, saved, window, cx);
}

/// Opens a context's tabs after the open ones: the documents read in the background, the chats
/// resumed from their transcripts.
fn open(
    workspace: &mut Workspace,
    saved: &Saved,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let tabs = &saved.tabs;
    workspace.open_saved_tabs(
        tabs.files.clone(),
        tabs.active,
        tabs.active_chat.clone(),
        window,
        cx,
    );
    if tabs.chats.is_empty() {
        return;
    }
    let store = workspace.claude.clone();
    let history = store.update(cx, |store, cx| store.history(cx));
    let chats = tabs.chats.clone();
    let active = tabs.active_chat.clone();
    cx.spawn(async move |_, cx| {
        let sessions = history.await;
        store
            .update(cx, |store, cx| {
                for chat in &chats {
                    if let Some(saved) = sessions.iter().find(|saved| saved.id == chat.session) {
                        let active = active.as_deref() == Some(chat.session.as_str());
                        store.reopen(saved, active, cx);
                    }
                }
            })
            .ok();
    })
    .detach();
}

// --- The popup ---

struct ContextList {
    workspace: WeakEntity<Workspace>,
    root: PathBuf,
    /// The plugins' names by id: the owner of a plugin's context.
    names: HashMap<String, String>,
    contexts: Vec<Saved>,
    matches: Vec<FuzzyMatch>,
    query: String,
    now: i64,
}

impl ContextList {
    fn new(
        workspace: WeakEntity<Workspace>,
        root: PathBuf,
        names: HashMap<String, String>,
    ) -> Self {
        let mut list = ContextList {
            contexts: load(&root),
            workspace,
            root,
            names,
            matches: Vec::new(),
            query: String::new(),
            now: now() as i64,
        };
        list.refilter();
        list
    }

    fn refilter(&mut self) {
        let titles: Vec<&str> = self
            .contexts
            .iter()
            .map(|saved| saved.title.as_str())
            .collect();
        self.matches = match_list(&self.query, &titles);
    }

    fn context(&self, index: usize) -> Option<&Saved> {
        self.contexts.get(self.matches.get(index)?.index)
    }

    /// What a row says under its title: how many tabs, whose, when.
    fn detail(&self, saved: &Saved) -> String {
        let tabs = saved.tabs.files.len() + saved.tabs.chats.len();
        let mut parts = vec![trn(tabs, "{n} tab", "{n} tabs")];
        if let Some(plugin) = &saved.plugin {
            parts.push(
                self.names
                    .get(plugin)
                    .cloned()
                    .unwrap_or_else(|| plugin.clone()),
            );
        }
        parts.push(crate::git_log::format_time(self.now, saved.saved_at as i64));
        parts.join(" · ")
    }
}

impl PickerDelegate for ContextList {
    fn placeholder(&self) -> SharedString {
        tr("Search contexts…").into()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn update_matches(&mut self, query: &str, _: &mut Window, _: &mut Context<Picker<Self>>) {
        self.query = query.to_string();
        self.refilter();
    }

    fn confirm(&mut self, index: usize, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(saved) = self.context(index).cloned() else {
            return;
        };
        cx.emit(DismissEvent);
        self.workspace
            .update(cx, |workspace, cx| switch_to(workspace, &saved, window, cx))
            .ok();
    }

    fn render_match(
        &mut self,
        index: usize,
        selected: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let (Some(found), Some(saved)) = (self.matches.get(index), self.context(index)) else {
            return div().into_any_element();
        };
        let name = if saved.plugin.is_some() {
            IconName::Checklist
        } else {
            IconName::Stash
        };
        // The trash shows on the row under the mouse and on the selected one.
        let delete = crate::ui::icon_button(("context-delete", index), IconName::Trash, ui)
            .flex_none()
            .tooltip(crate::ui::tooltip(tr("Delete Context"), Some("⌘⌫".into())))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(
                cx.listener(move |picker, _, window, cx| picker.delegate.delete(index, window, cx)),
            )
            .when(!selected, |button| {
                button
                    .invisible()
                    .group_hover("context-row", |style| style.visible())
            });
        div()
            .group("context-row")
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .child(icon(name, ui.dim).size(px(14.)).flex_none())
            .child(
                div()
                    .flex_shrink()
                    .min_w(px(96.))
                    .truncate()
                    .text_color(ui.foreground)
                    .child(highlighted_text(
                        saved.title.clone(),
                        &found.positions,
                        ui.match_text,
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(ui.dim)
                    .child(self.detail(saved)),
            )
            .child(delete)
            .into_any_element()
    }

    fn empty_message(&self) -> SharedString {
        if self.contexts.is_empty() {
            tr("No saved contexts in this project").into()
        } else {
            tr("No matches").into()
        }
    }

    fn confirm_label(&self) -> &'static str {
        tr("load")
    }

    fn key_context(&self) -> &'static str {
        CONTEXT
    }

    fn delete_label(&self) -> Option<&'static str> {
        Some(tr("delete"))
    }

    fn delete(&mut self, index: usize, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(saved) = self.context(index).cloned() else {
            return;
        };
        remove(&self.root, &saved.key);
        self.contexts.retain(|known| known.key != saved.key);
        self.refilter();
        cx.notify();
    }
}

// --- The file ---

thread_local! {
    /// A scenario's contexts without `FLUX_CONTEXTS_FILE`: neither read nor written.
    static MEMORY: RefCell<HashMap<PathBuf, Vec<Saved>>> = RefCell::new(HashMap::new());
}

/// The project's contexts, newest first.
fn load(root: &Path) -> Vec<Saved> {
    let mut contexts = match store_path() {
        Some(file) => parse_list(&read(&file)["projects"][key(root)]),
        None => MEMORY.with_borrow(|memory| memory.get(root).cloned().unwrap_or_default()),
    };
    contexts.sort_by_key(|saved| std::cmp::Reverse(saved.saved_at));
    contexts
}

fn find(root: &Path, key: &str) -> Option<Saved> {
    load(root).into_iter().find(|saved| saved.key == key)
}

/// Saves a context (replacing the one with its key): first in the list, the oldest beyond
/// [`MAX_CONTEXTS`] forgotten.
fn put(root: &Path, saved: Saved) {
    let mut contexts = load(root);
    contexts.retain(|known| known.key != saved.key);
    contexts.insert(0, saved);
    contexts.truncate(MAX_CONTEXTS);
    store(root, contexts);
}

fn remove(root: &Path, key: &str) {
    let mut contexts = load(root);
    let before = contexts.len();
    contexts.retain(|known| known.key != key);
    if contexts.len() != before {
        store(root, contexts);
    }
}

fn store(root: &Path, contexts: Vec<Saved>) {
    let Some(file) = store_path() else {
        MEMORY.with_borrow_mut(|memory| {
            memory.insert(root.to_path_buf(), contexts);
        });
        return;
    };
    let mut value = read(&file);
    if !value["projects"].is_object() {
        value["projects"] = json!({});
    }
    let list: Vec<Value> = contexts.iter().map(context_json).collect();
    if list.is_empty() {
        if let Some(projects) = value["projects"].as_object_mut() {
            projects.remove(&key(root));
        }
    } else {
        value["projects"][key(root)] = Value::Array(list);
    }
    if let Err(error) = write(&file, &value) {
        eprintln!("flux: {}: {error}", file.display());
    }
}

fn key(root: &Path) -> String {
    root.to_string_lossy().into_owned()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn store_path() -> Option<PathBuf> {
    store_path_for(
        std::env::var_os("FLUX_CONTEXTS_FILE"),
        std::env::var_os("FLUX_SCENARIO").is_some(),
        std::env::var_os("HOME"),
    )
}

/// Where the contexts are kept: an explicit file, otherwise (outside a scenario) Application
/// Support.
fn store_path_for(
    file: Option<OsString>,
    scenario: bool,
    home: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(file) = file.filter(|file| !file.is_empty()) {
        return Some(file.into());
    }
    if scenario {
        return None;
    }
    let home = PathBuf::from(home?);
    Some(home.join("Library/Application Support/flux/contexts.json"))
}

/// The file as JSON; missing or broken — an empty object.
fn read(file: &Path) -> Value {
    let text = match fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return json!({}),
        Err(error) => {
            eprintln!("flux: {}: {error}", file.display());
            return json!({});
        }
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(value) if value.is_object() => value,
        _ => json!({}),
    }
}

/// Written whole through a temporary file next to it.
fn write(file: &Path, value: &Value) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    let temp = file.with_extension("json.tmp");
    fs::write(&temp, text)?;
    fs::rename(&temp, file)
}

/// `{"key", "plugin", "title", "saved_at", "tabs": [{"path", "line", "column", "scroll"}],
/// "active", "chats": [{"session", "title"}], "active_chat"}`.
fn context_json(saved: &Saved) -> Value {
    let tabs: Vec<Value> = saved
        .tabs
        .files
        .iter()
        .map(|tab| {
            json!({
                "path": tab.path.to_string_lossy(),
                "line": tab.line,
                "column": tab.column,
                "scroll": tab.scroll,
            })
        })
        .collect();
    let chats: Vec<Value> = saved
        .tabs
        .chats
        .iter()
        .map(|chat| json!({ "session": chat.session, "title": chat.title }))
        .collect();
    json!({
        "key": saved.key,
        "plugin": saved.plugin,
        "title": saved.title,
        "saved_at": saved.saved_at,
        "tabs": tabs,
        "active": saved.tabs.active,
        "chats": chats,
        "active_chat": saved.tabs.active_chat,
    })
}

/// The contexts of a project; what doesn't fit is skipped.
fn parse_list(value: &Value) -> Vec<Saved> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_context)
        .collect()
}

fn parse_context(value: &Value) -> Option<Saved> {
    let key = value["key"].as_str()?.to_string();
    let title = value["title"].as_str()?.to_string();
    let files: Vec<SavedTab> = value["tabs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tab| {
            Some(SavedTab {
                path: PathBuf::from(tab["path"].as_str()?),
                line: tab["line"].as_u64().unwrap_or(0) as usize,
                column: tab["column"].as_u64().unwrap_or(0) as usize,
                scroll: tab["scroll"].as_f64().unwrap_or(0.) as f32,
            })
        })
        .collect();
    let active = value["active"]
        .as_u64()
        .map(|index| index as usize)
        .filter(|&index| index < files.len());
    let chats = value["chats"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|chat| {
            Some(SavedChat {
                session: chat["session"].as_str()?.to_string(),
                title: chat["title"].as_str().unwrap_or_default().to_string(),
            })
        })
        .collect();
    Some(Saved {
        key,
        plugin: value["plugin"].as_str().map(str::to_string),
        title,
        saved_at: value["saved_at"].as_u64().unwrap_or(0),
        tabs: Tabs {
            files,
            active,
            chats,
            active_chat: value["active_chat"].as_str().map(str::to_string),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(key: &str, at: u64) -> Saved {
        Saved {
            key: key.into(),
            plugin: key.starts_with("plugin:").then(|| "flux.yandex".into()),
            title: key.rsplit(':').next().unwrap().into(),
            saved_at: at,
            tabs: Tabs {
                files: vec![SavedTab {
                    path: "/p/src/main.rs".into(),
                    line: 12,
                    column: 4,
                    scroll: 3.5,
                }],
                active: Some(0),
                chats: vec![SavedChat {
                    session: "5f0c".into(),
                    title: "Fix the sign-in".into(),
                }],
                active_chat: None,
            },
        }
    }

    #[test]
    fn contexts_go_through_json() {
        let original = saved("plugin:flux.yandex:TRACK-142", 1_760_000_000);
        let value = context_json(&original);
        assert_eq!(parse_context(&value), Some(original));
        // What doesn't fit is skipped: a tab without a path, an active one out of range.
        let value = json!({
            "key": "user:a",
            "title": "a",
            "tabs": [{"line": 1}, {"path": "/p/b.rs"}],
            "active": 5,
        });
        let parsed = parse_context(&value).unwrap();
        assert_eq!(parsed.tabs.files.len(), 1);
        assert_eq!(parsed.tabs.active, None);
        assert_eq!(parsed.plugin, None);
        assert!(parse_context(&json!({"title": "no key"})).is_none());
    }

    #[test]
    fn a_project_keeps_its_contexts_newest_first() {
        let dir = std::env::temp_dir().join(format!("flux-contexts-{}", std::process::id()));
        let file = dir.join("contexts.json");
        let _ = fs::remove_file(&file);
        let root = Path::new("/p");
        let other = Path::new("/q");
        // The store writes where the file says, as the app does with FLUX_CONTEXTS_FILE.
        let put_in = |root: &Path, saved: Saved| {
            let mut value = read(&file);
            let mut list = parse_list(&value["projects"][key(root)]);
            list.retain(|known| known.key != saved.key);
            list.insert(0, saved);
            value["projects"][key(root)] = Value::Array(list.iter().map(context_json).collect());
            write(&file, &value).unwrap();
        };
        put_in(root, saved("user:old", 10));
        put_in(root, saved("plugin:flux.yandex:TRACK-1", 20));
        put_in(other, saved("user:elsewhere", 30));
        put_in(root, saved("user:old", 40));
        let value = read(&file);
        let mut list = parse_list(&value["projects"][key(root)]);
        list.sort_by_key(|saved| std::cmp::Reverse(saved.saved_at));
        let keys: Vec<_> = list.iter().map(|saved| saved.key.as_str()).collect();
        assert_eq!(keys, vec!["user:old", "plugin:flux.yandex:TRACK-1"]);
        assert_eq!(parse_list(&value["projects"][key(other)]).len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_file_is_explicit_or_in_application_support() {
        let home = Some(OsString::from("/Users/u"));
        assert_eq!(
            store_path_for(Some("/tmp/c.json".into()), true, home.clone()),
            Some(PathBuf::from("/tmp/c.json"))
        );
        assert_eq!(store_path_for(None, true, home.clone()), None);
        assert_eq!(
            store_path_for(None, false, home),
            Some(PathBuf::from(
                "/Users/u/Library/Application Support/flux/contexts.json"
            ))
        );
    }

    #[test]
    fn keys_keep_plugins_and_the_user_apart() {
        assert_eq!(user_key("Release"), "user:Release");
        assert_eq!(
            plugin_key("flux.yandex", "TRACK-142"),
            "plugin:flux.yandex:TRACK-142"
        );
        assert_ne!(plugin_key("a.b", "c"), plugin_key("a", "b:c"));
    }
}
