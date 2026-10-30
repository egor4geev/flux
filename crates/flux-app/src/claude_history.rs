//! The project's Claude sessions (part 9.2): "Resume Session…" — `/resume` in the message field,
//! the palette — lists the saved sessions of the project
//! (`~/.claude/projects`, the terminal's and Flux's alike) in a popup; the chosen one comes back in
//! a tab, its conversation read from the transcript, and goes on with `claude --resume`. The
//! sessions open when the project closed come back when it opens (Settings → Claude Code).
//!
//! - The popup ([`open`], [`open_at`]): a [`Picker`] — a search over the titles and the first
//!   prompts, rows with the time of the last activity and the branch; the sessions open in this
//!   window are marked, choosing one of them brings its chat into sight where it is. A session is
//!   deleted with the trash on its row or ⌘⌫ (part 10.2): after a question, its transcript and its
//!   folder (subagents, tool results) go to the Trash; an open one is closed first.
//! - The open sessions of a project ([`OpenSessions`]: the chats in the editor's tabs) are kept in `~/Library/Application Support/flux/claude-sessions.json`
//!   (`FLUX_CLAUDE_SESSIONS_FILE`; in scenarios without it — neither read nor written), written
//!   whenever they change ([`crate::claude::ClaudeStore::persist`]).

use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use flux_claude::SavedSession;
use flux_search::{FuzzyMatch, match_list};
use gpui::{
    Action, AnyElement, App, Context, DismissEvent, Div, Entity, KeyBinding, MouseButton, Pixels,
    Point, SharedString, Window, div, prelude::*, px,
};
use serde_json::{Value, json};

use crate::claude::{self, ClaudeStore};
use crate::dialog::Dialog;
use crate::git_log::local_offset;
use crate::i18n::{tr, trf, trn};
use crate::icons::{IconName, icon};
use crate::picker::{self, Picker, PickerDelegate, highlighted_text};
use crate::theme::{self, Theme, UiColors};
use crate::workspace::Workspace;

/// How many sessions of one project come back after a restart at most.
const MAX_OPEN: usize = 20;

/// The popup's own key context: ⌘⌫ deletes the selected session (in the query field too — the
/// binding comes after the field's own ⌘⌫).
const CONTEXT: &str = "Picker ClaudeHistory";

pub fn init(cx: &mut App) {
    crate::keymap::bind_keys(
        cx,
        [
            KeyBinding::new(
                "cmd-backspace",
                picker::DeleteSelected,
                Some("ClaudeHistory > TextInput"),
            ),
            KeyBinding::new(
                "cmd-backspace",
                picker::DeleteSelected,
                Some("ClaudeHistory"),
            ),
        ],
    );
}

thread_local! {
    /// The row the history comes back on after a deletion (it closes for the question).
    static RESELECT: Cell<Option<usize>> = const { Cell::new(None) };
}

// --- The popup ---

/// "Resume Session…": the history popup, centered under the title bar.
pub fn open(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    open_at(workspace, None, window, cx)
}

/// The history popup at `anchor`, or centered under the title bar.
pub fn open_at(
    workspace: &mut Workspace,
    anchor: Option<Point<Pixels>>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let store = workspace.claude.clone();
    let open = open_ids(&store, cx);
    let history = store.update(cx, |store, cx| store.history(cx));
    let reselect = RESELECT.with(Cell::take);
    workspace.toggle_modal(window, cx, move |window, cx| {
        let picker = Picker::new(HistoryDelegate::new(store, open), window, cx);
        cx.spawn(async move |picker, cx| {
            let sessions = history.await;
            picker
                .update(cx, |picker, cx| {
                    picker.delegate.set_sessions(sessions);
                    if let Some(index) = reselect {
                        picker.set_selected(index, cx);
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
        picker
    });
    if let Some(anchor) = anchor {
        workspace.anchor_modal(anchor);
    }
}

/// The ids of the sessions open in the window.
fn open_ids(store: &Entity<ClaudeStore>, cx: &gpui::App) -> HashSet<String> {
    store
        .read(cx)
        .sessions()
        .iter()
        .filter_map(|session| session.read(cx).session_id().map(str::to_string))
        .collect()
}

/// A saved session chosen in the history: back in a tab (an open one is brought into sight where it
/// is); while `claude` isn't ready, the console says why.
pub fn resume(
    store: &Entity<ClaudeStore>,
    saved: &SavedSession,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    let was_open = open_ids(store, cx).contains(&saved.id);
    let session = store.update(cx, |store, cx| store.resume(saved, cx));
    let action: Box<dyn Action> = match session {
        // A new one comes into sight by itself (`ClaudeStoreEvent::SessionAdded`).
        Some(_) if !was_open => return,
        Some(session) => Box::new(claude::ShowSession(session.entity_id())),
        // Not ready: "New Session" shows the window, which says what to do.
        None => Box::new(claude::NewSession),
    };
    window.dispatch_action(action, cx);
}

struct HistoryDelegate {
    store: Entity<ClaudeStore>,
    /// The sessions open in the window: marked.
    open: HashSet<String>,
    /// `None` while the transcripts are read.
    sessions: Option<Vec<SavedSession>>,
    matches: Vec<FuzzyMatch>,
    query: String,
    now: i64,
}

impl HistoryDelegate {
    fn new(store: Entity<ClaudeStore>, open: HashSet<String>) -> Self {
        Self {
            store,
            open,
            sessions: None,
            matches: Vec::new(),
            query: String::new(),
            now: now_seconds(),
        }
    }

    fn set_sessions(&mut self, sessions: Vec<SavedSession>) {
        self.sessions = Some(sessions);
        self.refilter();
    }

    fn refilter(&mut self) {
        let haystacks: Vec<String> = self.sessions.iter().flatten().map(haystack).collect();
        self.matches = match_list(&self.query, &haystacks);
    }

    fn session(&self, index: usize) -> Option<&SavedSession> {
        let found = self.matches.get(index)?;
        self.sessions.as_ref()?.get(found.index)
    }
}

impl PickerDelegate for HistoryDelegate {
    fn placeholder(&self) -> SharedString {
        tr("Search sessions…").into()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn update_matches(&mut self, query: &str, _: &mut Window, _: &mut Context<Picker<Self>>) {
        self.query = query.to_string();
        self.refilter();
    }

    fn confirm(&mut self, index: usize, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(saved) = self.session(index).cloned() else {
            return;
        };
        resume(&self.store, &saved, window, cx);
        cx.emit(DismissEvent);
    }

    fn render_match(
        &mut self,
        index: usize,
        selected: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let (Some(found), Some(session)) = (self.matches.get(index), self.session(index)) else {
            return div().into_any_element();
        };
        let (title_positions, prompt_positions) = split_positions(session, &found.positions);
        let open = self.open.contains(&session.id);
        let prompt = shown_prompt(session).map(|prompt| {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.dim)
                .child(highlighted_text(
                    prompt.to_string(),
                    &prompt_positions,
                    ui.match_text,
                ))
        });
        // Without a prompt of its own, the title takes the row (a long first prompt as a title).
        let alone = prompt.is_none();
        // The trash shows on the row under the mouse and on the selected one; under the mouse it
        // fades in (the button's id keeps the fade), moved to with the arrows it is there at once.
        let delete = crate::ui::icon_button(("history-delete", index), IconName::Trash, ui)
            .flex_none()
            .tooltip(crate::ui::tooltip(
                tr("Delete Session"),
                Some(crate::keymap::shortcut("picker::DeleteSelected", "cmd-backspace").into()),
            ))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(
                cx.listener(move |picker, _, window, cx| picker.delegate.delete(index, window, cx)),
            )
            .when(!selected, |button| {
                button
                    .opacity(0.)
                    .group_hover("history-row", |style| style.opacity(1.))
            });
        div()
            .group("history-row")
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .child(
                icon(IconName::Claude, if open { ui.accent_text } else { ui.dim })
                    .size(px(14.))
                    .flex_none(),
            )
            .child(
                div()
                    .min_w(px(96.))
                    .when(alone, |title| title.flex_1())
                    .when(!alone, |title| title.flex_shrink().max_w(px(360.)))
                    .truncate()
                    .text_color(ui.foreground)
                    .child(highlighted_text(
                        session.title.clone(),
                        &title_positions,
                        ui.match_text,
                    )),
            )
            .children(prompt)
            .children(open.then(|| crate::ui::badge(tr("already open"), ui.accent_text)))
            .children(
                session
                    .branch
                    .as_deref()
                    .map(|branch| branch_chip(branch, ui)),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(ui.dim)
                    .child(when(self.now, seconds(session.modified))),
            )
            .child(delete)
            .into_any_element()
    }

    fn render_footer(&self, _: &mut Window, cx: &mut Context<Picker<Self>>) -> Option<AnyElement> {
        let ui = Theme::ui(cx);
        let text = match &self.sessions {
            None => tr("Reading the sessions…").to_string(),
            Some(sessions) => trn(sessions.len(), "{n} session", "{n} sessions"),
        };
        Some(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(ui.dim)
                .child(text)
                .into_any_element(),
        )
    }

    fn empty_message(&self) -> SharedString {
        match &self.sessions {
            None => tr("Loading…").into(),
            Some(sessions) if sessions.is_empty() => tr("No saved sessions in this project").into(),
            Some(_) => tr("No matches").into(),
        }
    }

    fn confirm_label(&self) -> &'static str {
        tr("resume")
    }

    fn key_context(&self) -> &'static str {
        CONTEXT
    }

    fn delete_label(&self) -> Option<&'static str> {
        Some(tr("delete"))
    }

    /// Deletes a session after a question: its transcript and folder go to the Trash, an open one
    /// is closed first. The popup closes for the question and comes back on the same row.
    fn delete(&mut self, index: usize, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(saved) = self.session(index).cloned() else {
            return;
        };
        let session = self
            .store
            .read(cx)
            .sessions()
            .iter()
            .find(|session| session.read(cx).session_id() == Some(saved.id.as_str()));
        let session = session.cloned();
        let dialog = match &session {
            Some(_) => Dialog::warning(trf("The session “{0}” is open", &[&saved.title]))
                .message(tr("It closes, and its transcript goes to the Trash."))
                .danger(tr("Close and Delete"))
                .cancel(tr("Cancel")),
            None => Dialog::warning(trf("Delete the session “{0}”?", &[&saved.title]))
                .message(tr("Its transcript goes to the Trash."))
                .danger(tr("Delete"))
                .cancel(tr("Cancel")),
        };
        let answer = dialog.show(window, cx);
        let store = self.store.clone();
        cx.spawn_in(window, async move |picker, cx| {
            let confirmed = answer.await == Some(0);
            if confirmed {
                if let Some(session) = &session {
                    store
                        .update(cx, |store, cx| store.close_session(session, cx))
                        .ok();
                }
                let paths = transcript_files(&saved.path);
                let trashed = cx
                    .background_spawn(async move { flux_fs::trash(&paths) })
                    .await;
                if let Err(error) = trashed {
                    let shown = cx.update(|window, cx| {
                        Dialog::warning(tr("Couldn't delete the session"))
                            .details(error.to_string())
                            .primary(tr("OK"))
                            .show(window, cx)
                    });
                    if let Ok(shown) = shown {
                        shown.await;
                    }
                }
            }
            // Back to the history, on the row that took the deleted one's place: the popup read
            // again if the question left it open, otherwise opened anew.
            let refreshed = picker.update(cx, |picker, cx| {
                picker.delegate.open.remove(&saved.id);
                let history = store.update(cx, |store, cx| store.history(cx));
                cx.spawn(async move |picker, cx| {
                    let sessions = history.await;
                    picker
                        .update(cx, |picker, cx| {
                            picker.delegate.set_sessions(sessions);
                            picker.set_selected(index, cx);
                        })
                        .ok();
                })
                .detach();
            });
            if refreshed.is_err() {
                RESELECT.with(|reselect| reselect.set(Some(index)));
                cx.update(|window, cx| window.dispatch_action(Box::new(claude::ResumeSession), cx))
                    .ok();
            }
        })
        .detach();
    }
}

/// What deleting a session moves to the Trash: its transcript and, if it has one, its folder of
/// subagents and tool results (`<id>/` next to `<id>.jsonl`).
fn transcript_files(transcript: &Path) -> Vec<PathBuf> {
    let mut paths = vec![transcript.to_path_buf()];
    let folder = transcript.with_extension("");
    if folder.is_dir() {
        paths.push(folder);
    }
    paths.retain(|path| path.symlink_metadata().is_ok());
    paths
}

/// What the search matches: the title, then the first prompt when it says more.
fn haystack(session: &SavedSession) -> String {
    match shown_prompt(session) {
        Some(prompt) => format!("{}{PROMPT_SEPARATOR}{prompt}", session.title),
        None => session.title.clone(),
    }
}

/// Between the title and the prompt in the searched text: the matched positions after it belong
/// to the prompt.
const PROMPT_SEPARATOR: &str = "  ";

/// The matched positions of [`haystack`] → those of the title and those of the prompt.
fn split_positions(session: &SavedSession, positions: &[usize]) -> (Vec<usize>, Vec<usize>) {
    let title = session.title.chars().count();
    let prompt_start = title + PROMPT_SEPARATOR.chars().count();
    let in_title = positions.iter().copied().filter(|&at| at < title).collect();
    let in_prompt = positions
        .iter()
        .filter_map(|&at| at.checked_sub(prompt_start))
        .collect();
    (in_title, in_prompt)
}

/// The first prompt, on one line, unless the title is already made of it (a session without a
/// title of its own).
pub(crate) fn shown_prompt(session: &SavedSession) -> Option<String> {
    let prompt = session.prompt.as_deref()?;
    let line = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return None;
    }
    let title = session.title.trim_end_matches('…').trim_end();
    if line == session.title || (session.title.ends_with('…') && line.starts_with(title)) {
        return None;
    }
    Some(line)
}

/// A git branch in a row: a small violet chip, as in the title bar.
fn branch_chip(branch: &str, ui: UiColors) -> Div {
    div()
        .flex_none()
        .max_w(px(140.))
        .h(px(18.))
        .px_1p5()
        .flex()
        .items_center()
        .gap_1()
        .rounded(px(9.))
        .bg(UiColors::tint(ui.violet, 0.12))
        .text_size(px(theme::TEXT_XS))
        .text_color(ui.violet)
        .child(icon(IconName::Branch, ui.violet).size(px(11.)).flex_none())
        .child(div().min_w_0().truncate().child(branch.to_string()))
}

// --- Times ---

pub(crate) fn now_seconds() -> i64 {
    seconds(SystemTime::now())
}

pub(crate) fn seconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// When a session was last active: "just now", "12 min ago", "Today 14:05", "Yesterday 09:30",
/// "2025-03-14" — the Git log dates its commits the same way.
pub(crate) fn when(now: i64, time: i64) -> String {
    when_in(now, time, local_offset(now), local_offset(time))
}

/// [`when`] with the local zone's offsets (seconds east of UTC) at both moments.
fn when_in(now: i64, time: i64, now_offset: i64, time_offset: i64) -> String {
    let ago = (now - time).max(0);
    if ago < 60 {
        return tr("just now").to_string();
    }
    if ago < 3600 {
        return trf("{0} min ago", &[&(ago / 60)]);
    }
    let local = time + time_offset;
    let today = (now + now_offset).div_euclid(86_400);
    let day = local.div_euclid(86_400);
    let minutes = local.rem_euclid(86_400) / 60;
    let clock = format!("{:02}:{:02}", minutes / 60, minutes % 60);
    match today - day {
        0 => trf("Today {0}", &[&clock]),
        1 => trf("Yesterday {0}", &[&clock]),
        _ => {
            let (y, m, d) = civil(day);
            format!("{y:04}-{m:02}-{d:02}")
        }
    }
}

/// Days since 1970-01-01 → (year, month, day) (Howard Hinnant's civil_from_days).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// --- The open sessions of a project, between launches ---

/// Where a session's chat was: an editor tab — the only place since the console took the Claude
/// window's (part «Пульт Claude»); a pill of that window (`"panel"`) comes back as a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPlace {
    Editor,
}

impl SessionPlace {
    fn key(self) -> &'static str {
        match self {
            SessionPlace::Editor => "editor",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        match key {
            "panel" | "editor" => Some(SessionPlace::Editor),
            _ => None,
        }
    }
}

/// The open sessions of a project, as they come back.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OpenSessions {
    /// The chats in the editor's tabs, in their order.
    pub sessions: Vec<(String, SessionPlace)>,
    /// The active one (a context coming back, part 10.3).
    pub active: Option<String>,
}

/// The sessions open when the project last closed.
pub fn load_open(root: &Path) -> OpenSessions {
    store_path()
        .map(|file| read(&file))
        .and_then(|mut projects| projects.remove(&key(root)))
        .unwrap_or_default()
}

/// Remembers the project's open sessions (none — the project is forgotten). Errors go to stderr:
/// the window works on without them.
pub fn save_open(root: &Path, open: &OpenSessions) {
    let Some(file) = store_path() else {
        return;
    };
    let mut projects = read(&file);
    if open.sessions.is_empty() {
        projects.remove(&key(root));
    } else {
        projects.insert(key(root), open.clone());
    }
    if let Err(error) = write(&file, &serialize(&projects)) {
        eprintln!("flux: {}: {error}", file.display());
    }
}

fn key(root: &Path) -> String {
    root.to_string_lossy().into_owned()
}

fn store_path() -> Option<PathBuf> {
    store_path_for(
        std::env::var_os("FLUX_CLAUDE_SESSIONS_FILE"),
        std::env::var_os("FLUX_SCENARIO").is_some(),
        flux_platform::paths::home().map(std::ffi::OsString::from),
    )
}

fn console_path() -> Option<PathBuf> {
    file_path_for(
        "claude-console.json",
        std::env::var_os("FLUX_CLAUDE_CONSOLE_FILE"),
        std::env::var_os("FLUX_SCENARIO").is_some(),
        flux_platform::paths::home().map(std::ffi::OsString::from),
    )
}

/// Where the open sessions are kept: an explicit file, otherwise (outside a scenario) Application
/// Support.
fn store_path_for(
    file: Option<OsString>,
    scenario: bool,
    home: Option<OsString>,
) -> Option<PathBuf> {
    file_path_for("claude-sessions.json", file, scenario, home)
}

/// A file of Flux's own in Application Support (`name`), unless an explicit one is given; in
/// scenarios without one — none.
fn file_path_for(
    name: &str,
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
    Some(flux_platform::paths::support_dir_in(&home).join(name))
}

fn read(file: &Path) -> BTreeMap<String, OpenSessions> {
    match fs::read_to_string(file) {
        Ok(text) => parse(&text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
        Err(error) => {
            eprintln!("flux: {}: {error}", file.display());
            BTreeMap::new()
        }
    }
}

/// `{"projects": {"/root": {"sessions": [{"id": "…", "place": "panel"}], "active": "…"}}}`;
/// anything else in the file is skipped.
fn parse(text: &str) -> BTreeMap<String, OpenSessions> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return BTreeMap::new();
    };
    let Some(projects) = value["projects"].as_object() else {
        return BTreeMap::new();
    };
    projects
        .iter()
        .map(|(root, project)| {
            let mut sessions: Vec<(String, SessionPlace)> = Vec::new();
            for session in project["sessions"].as_array().into_iter().flatten() {
                let (Some(id), Some(place)) = (
                    session["id"].as_str().filter(|id| !id.is_empty()),
                    session["place"].as_str().and_then(SessionPlace::from_key),
                ) else {
                    continue;
                };
                if !sessions.iter().any(|(known, _)| known == id) {
                    sessions.push((id.to_string(), place));
                }
            }
            sessions.truncate(MAX_OPEN);
            let active = project["active"]
                .as_str()
                .filter(|active| sessions.iter().any(|(id, _)| id == active))
                .map(str::to_string);
            (root.clone(), OpenSessions { sessions, active })
        })
        .filter(|(_, open)| !open.sessions.is_empty())
        .collect()
}

fn serialize(projects: &BTreeMap<String, OpenSessions>) -> String {
    let projects: serde_json::Map<String, Value> = projects
        .iter()
        .map(|(root, open)| {
            let sessions: Vec<Value> = open
                .sessions
                .iter()
                .take(MAX_OPEN)
                .map(|(id, place)| json!({ "id": id, "place": place.key() }))
                .collect();
            (
                root.clone(),
                json!({ "sessions": sessions, "active": open.active }),
            )
        })
        .collect();
    serde_json::to_string_pretty(&json!({ "projects": projects })).unwrap_or_default() + "\n"
}

// --- The console's sessions of a project (part «Пульт Claude») ---

/// The console's sessions of a project between launches: "Now" (by CLI id, the latest first — the
/// ones open in tabs come back through [`OpenSessions`], the rest unloaded) and the archive. Kept in
/// `~/Library/Application Support/flux/claude-console.json` (`FLUX_CLAUDE_CONSOLE_FILE`; in scenarios
/// without it — neither read nor written). A new file: an older Flux running alongside rewrites
/// `claude-sessions.json` from its own struct.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConsoleState {
    pub now: Vec<String>,
    pub archived: Vec<String>,
}

/// How many sessions "Now" and the archive keep at most.
const MAX_CONSOLE: usize = 200;

pub fn load_console(root: &Path) -> ConsoleState {
    console_path()
        .map(|file| read_console(&file))
        .and_then(|mut projects| projects.remove(&key(root)))
        .unwrap_or_default()
}

/// Remembers the project's console (an empty one — the project is forgotten).
pub fn save_console(root: &Path, state: &ConsoleState) {
    let Some(file) = console_path() else {
        return;
    };
    let mut projects = read_console(&file);
    if state.now.is_empty() && state.archived.is_empty() {
        projects.remove(&key(root));
    } else {
        projects.insert(key(root), state.clone());
    }
    if let Err(error) = write(&file, &serialize_console(&projects)) {
        eprintln!("flux: {}: {error}", file.display());
    }
}

fn read_console(file: &Path) -> BTreeMap<String, ConsoleState> {
    match fs::read_to_string(file) {
        Ok(text) => parse_console(&text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
        Err(error) => {
            eprintln!("flux: {}: {error}", file.display());
            BTreeMap::new()
        }
    }
}

/// `{"projects": {"/root": {"now": ["…"], "archived": ["…"]}}}`; anything else is skipped.
fn parse_console(text: &str) -> BTreeMap<String, ConsoleState> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return BTreeMap::new();
    };
    let Some(projects) = value["projects"].as_object() else {
        return BTreeMap::new();
    };
    let ids = |value: &Value| {
        let mut ids: Vec<String> = Vec::new();
        for id in value.as_array().into_iter().flatten() {
            if let Some(id) = id.as_str().filter(|id| !id.is_empty())
                && !ids.iter().any(|known| known == id)
            {
                ids.push(id.to_string());
            }
        }
        ids.truncate(MAX_CONSOLE);
        ids
    };
    projects
        .iter()
        .map(|(root, project)| {
            let archived = ids(&project["archived"]);
            let now = ids(&project["now"])
                .into_iter()
                .filter(|id| !archived.contains(id))
                .collect();
            (root.clone(), ConsoleState { now, archived })
        })
        .filter(|(_, state)| !state.now.is_empty() || !state.archived.is_empty())
        .collect()
}

fn serialize_console(projects: &BTreeMap<String, ConsoleState>) -> String {
    let projects: serde_json::Map<String, Value> = projects
        .iter()
        .map(|(root, state)| {
            let now: Vec<&String> = state.now.iter().take(MAX_CONSOLE).collect();
            let archived: Vec<&String> = state.archived.iter().take(MAX_CONSOLE).collect();
            (root.clone(), json!({ "now": now, "archived": archived }))
        })
        .collect();
    serde_json::to_string_pretty(&json!({ "projects": projects })).unwrap_or_default() + "\n"
}

/// Through a temporary file next to the target and a `rename`: never half-written; the process id
/// in its name keeps two Flux windows apart.
fn write(file: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut temp = file.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    fs::write(&temp, text)?;
    fs::rename(&temp, file).inspect_err(|_| {
        fs::remove_file(&temp).ok();
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(title: &str, prompt: Option<&str>) -> SavedSession {
        SavedSession {
            id: "id".into(),
            title: title.into(),
            prompt: prompt.map(str::to_string),
            branch: None,
            modified: UNIX_EPOCH,
            size: 0,
            path: PathBuf::from("/p/id.jsonl"),
        }
    }

    #[test]
    fn the_open_sessions_round_trip_through_the_file() {
        let dir = tempfile_dir();
        let file = dir.join("claude-sessions.json");
        let mut projects = BTreeMap::new();
        let open = OpenSessions {
            sessions: vec![
                ("a".into(), SessionPlace::Editor),
                ("b".into(), SessionPlace::Editor),
                ("c".into(), SessionPlace::Editor),
            ],
            active: Some("c".into()),
        };
        projects.insert("/Users/me/app".to_string(), open.clone());
        write(&file, &serialize(&projects)).unwrap();
        assert_eq!(read(&file), projects);
        // An empty project isn't kept; a missing file reads as nothing.
        assert!(
            parse(&serialize(&BTreeMap::from([(
                "/x".to_string(),
                OpenSessions::default()
            )])))
            .is_empty()
        );
        assert!(read(&dir.join("missing.json")).is_empty());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn broken_entries_are_skipped() {
        let text = r#"{"projects": {
            "/a": {"sessions": [{"id": "x", "place": "panel"}, {"id": "", "place": "panel"},
                                {"id": "y", "place": "nowhere"}, {"id": "x", "place": "editor"}],
                   "active": "gone"},
            "/b": "not an object"
        }}"#;
        let projects = parse(text);
        assert_eq!(
            projects.get("/a"),
            Some(&OpenSessions {
                // The Claude window's pill comes back as a tab.
                sessions: vec![("x".into(), SessionPlace::Editor)],
                active: None,
            })
        );
        assert_eq!(projects.len(), 1);
        assert!(parse("not json").is_empty());
    }

    #[test]
    fn the_console_round_trips_and_archived_ids_leave_now() {
        // Its own folder: the other file test removes its own when done.
        let dir = std::env::temp_dir().join(format!("flux-claude-console-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("claude-console.json");
        let state = ConsoleState {
            now: vec!["a".into(), "b".into()],
            archived: vec!["c".into()],
        };
        let projects = BTreeMap::from([("/Users/me/app".to_string(), state)]);
        write(&file, &serialize_console(&projects)).unwrap();
        assert_eq!(read_console(&file), projects);
        let text = r#"{"projects": {"/a": {"now": ["x", "", "y", "x", 5], "archived": ["y"]},
            "/b": {"now": []}}}"#;
        assert_eq!(
            parse_console(text),
            BTreeMap::from([(
                "/a".to_string(),
                ConsoleState {
                    now: vec!["x".into()],
                    archived: vec!["y".into()],
                }
            )])
        );
        assert_eq!(
            file_path_for("claude-console.json", None, false, Some("/Users/me".into())),
            Some(PathBuf::from(
                "/Users/me/Library/Application Support/flux/claude-console.json"
            ))
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn the_file_is_explicit_or_in_application_support_outside_scenarios() {
        let home = Some(OsString::from("/Users/me"));
        assert_eq!(
            store_path_for(Some("/tmp/s.json".into()), true, home.clone()),
            Some(PathBuf::from("/tmp/s.json"))
        );
        assert_eq!(store_path_for(None, true, home.clone()), None);
        assert_eq!(
            store_path_for(None, false, home),
            Some(PathBuf::from(
                "/Users/me/Library/Application Support/flux/claude-sessions.json"
            ))
        );
    }

    #[test]
    fn times_read_as_the_git_log_dates() {
        // 2026-10-09 12:00 UTC.
        let now = 1_791_547_200;
        assert_eq!(when_in(now, now - 20, 0, 0), "just now");
        assert_eq!(when_in(now, now - 12 * 60, 0, 0), "12 min ago");
        assert_eq!(when_in(now, now - 3 * 3600, 0, 0), "Today 09:00");
        assert_eq!(when_in(now, now - 20 * 3600, 0, 0), "Yesterday 16:00");
        assert_eq!(when_in(now, now - 3 * 86_400, 0, 0), "2026-10-06");
        // The day is the local one: 01:00 in UTC+3 is still "today" there at 12:00 UTC.
        assert_eq!(
            when_in(now, now - 14 * 3600, 3 * 3600, 3 * 3600),
            "Today 01:00"
        );
    }

    #[test]
    fn the_prompt_shows_when_it_says_more_than_the_title() {
        assert_eq!(
            shown_prompt(&saved(
                "Fix the login flow",
                Some("the login  fails\nwhen…")
            )),
            Some("the login fails when…".to_string())
        );
        // A title made of the prompt.
        assert_eq!(
            shown_prompt(&saved("explain main.rs", Some("explain main.rs"))),
            None
        );
        assert_eq!(
            shown_prompt(&saved(
                "a very long prompt cut…",
                Some("a very long prompt cut at eighty characters")
            )),
            None
        );
        assert_eq!(shown_prompt(&saved("Title", None)), None);
    }

    #[test]
    fn matched_positions_split_between_the_title_and_the_prompt() {
        let session = saved("Fix", Some("auth fails"));
        assert_eq!(haystack(&session), "Fix  auth fails");
        // "F", then "a" of "auth" (index 5 of the haystack = 0 of the prompt).
        assert_eq!(split_positions(&session, &[0, 5, 6]), (vec![0], vec![0, 1]));
        let found = match_list("auth", &[haystack(&session)]);
        assert_eq!(found.len(), 1);
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "flux-claude-history-{}-{}",
            std::process::id(),
            now_seconds()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
