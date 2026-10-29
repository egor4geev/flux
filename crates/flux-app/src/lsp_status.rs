//! The language servers' state (part 10.5), as JetBrains IDEs' Language Services widget: a project
//! the server couldn't load (rust-analyzer's `experimental/serverStatus`) shows in the status bar
//! and as a notification; a click on the status opens the popup of the window's servers with
//! Restart, Stop or Start, Configure and the log.
//!
//! The popup opens above the status item (the palette's "Language Server: Show Servers" opens it
//! there too, or in the middle of the window when the item isn't drawn): a row per server — its
//! language's icon, the name, what it is doing ("Running · 3 files", "Indexing 42 %", "Failed to
//! load workspaces." in the warning or error color) and its buttons; ↑↓ choose a row, ↵ restarts
//! (or starts) it, ⌘F2 stops it (JetBrains' Stop), ⌘, configures it, ⌘L shows its log, Esc closes. The log is the server's stderr and messages
//! ([`flux_lsp::ServerLog`]), written to a file under the temporary directory and opened in a tab
//! read-only (an open tab of it takes the fresh text).

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use flux_lsp::{Health, ServerStatus};
use gpui::{
    Action, AnyElement, App, Bounds, ClickEvent, Context, Corner, DismissEvent, Div, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, KeyBinding, MouseButton, Pixels, Render,
    Stateful, Subscription, WeakEntity, Window, actions, div, point, prelude::*, px,
};

use crate::i18n::{tr, trf, trn};
use crate::icons::{IconName, file_icon, icon};
use crate::lsp::{LspStore, ServerKey};
use crate::lsp_settings::ConfigureServer;
use crate::motion;
use crate::notification_center::NotificationGroup;
use crate::notifications::Notification;
use crate::settings_view::Section;
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_MD, RADIUS_SM};
use crate::workspace::{Workspace, tilde};

// In the language servers' namespace: the palette shows it as "Language Server: Show Servers".
actions!(language_server, [ShowServers]);

actions!(
    lsp_servers_popup,
    [
        SelectNext,
        SelectPrevious,
        /// ↵: restart (or start) the selected server.
        Confirm,
        /// ⌘F2 (JetBrains' Stop): stop the selected server.
        StopSelected,
        /// ⌘,: "Configure…" of the selected server.
        ConfigureSelected,
        /// ⌘L: the selected server's log.
        LogSelected,
        Dismiss,
    ]
);

/// Restart (or start) a server: from its row, a notification.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = language_server, no_json)]
pub struct RestartServer(pub ServerKey);

/// Stop a server until it is started in the popup.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = language_server, no_json)]
pub struct StopServer(pub ServerKey);

/// "Show Log": the server's log in a tab.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = language_server, no_json)]
pub struct ShowServerLog(pub ServerKey);

const WIDTH: f32 = 500.;
/// The rows are inset from the popup's edges, as the context menu's items.
const INSET: f32 = 6.;
/// A server: its name over its state.
const ROW_HEIGHT: f32 = 46.;
const ACTION_HEIGHT: f32 = 30.;
const ICON_COLUMN: f32 = 24.;
const FOOTER_HEIGHT: f32 = 34.;
/// Between the status item and the popup above it.
const ANCHOR_GAP: f32 = 6.;

pub fn init(cx: &mut App) {
    let popup = Some("LspServersPopup");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, popup),
        KeyBinding::new("up", SelectPrevious, popup),
        KeyBinding::new("ctrl-n", SelectNext, popup),
        KeyBinding::new("ctrl-p", SelectPrevious, popup),
        KeyBinding::new("enter", Confirm, popup),
        KeyBinding::new("cmd-f2", StopSelected, popup),
        KeyBinding::new("cmd-,", ConfigureSelected, popup),
        KeyBinding::new("cmd-l", LogSelected, popup),
        KeyBinding::new("escape", Dismiss, popup),
    ]);
}

/// The window-level actions: the popup from the palette or the status item, and the actions of
/// the rows and the notifications ("Configure…" is [`ConfigureServer`], handled by
/// [`crate::lsp_settings`] for any window).
pub fn workspace_actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(
        cx.listener(|workspace, _: &ShowServers, window, cx| toggle(workspace, window, cx)),
    )
    .on_action(cx.listener(|workspace, action: &RestartServer, _, cx| {
        let key = action.0.clone();
        workspace
            .lsp
            .update(cx, |store, cx| store.restart_key(&key, cx))
    }))
    .on_action(cx.listener(|workspace, action: &StopServer, _, cx| {
        let key = action.0.clone();
        workspace
            .lsp
            .update(cx, |store, cx| store.stop_key(&key, cx))
    }))
    .on_action(
        cx.listener(|workspace, action: &ShowServerLog, window, cx| {
            show_log(workspace, &action.0, window, cx)
        }),
    )
}

// --- The status item ---

/// The status item's place on screen and whether the popup is open: the popup opens above the
/// item, and a press on the item while the popup is open only closes it.
#[derive(Default)]
pub(crate) struct StatusUi {
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    popup_open: Rc<Cell<bool>>,
}

/// The language server item of the status bar, made clickable: a press opens the servers' popup
/// above it (as a macOS pop-up button), a press while it is open closes it.
pub(crate) fn clickable_status(
    item: Stateful<Div>,
    status_ui: &StatusUi,
    ui: UiColors,
) -> AnyElement {
    let popup_open = status_ui.popup_open.clone();
    let was_open = Rc::new(Cell::new(false));
    let item = item
        .h(px(20.))
        .px_1()
        .rounded(px(RADIUS_SM))
        .cursor_pointer()
        .hover(move |style| style.bg(ui.hover))
        // The popup's own "press outside" closes it first: the capture phase sees it still open.
        .capture_any_mouse_down({
            let was_open = was_open.clone();
            move |_, _, _| was_open.set(popup_open.get())
        })
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            if !was_open.get() {
                window.dispatch_action(Box::new(ShowServers), cx);
            }
        });
    let bounds = status_ui.bounds.clone();
    div()
        .flex_none()
        .on_children_prepainted(move |children, _, _| bounds.set(children.first().copied()))
        .child(item)
        .into_any_element()
}

/// Opens the popup above the status item (or closes it, when open).
pub fn toggle(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let store = workspace.lsp.clone();
    let anchor = store.read(cx).status_ui.bounds.get();
    let this = cx.weak_entity();
    workspace.toggle_modal(window, cx, move |window, cx| {
        ServersPopup::new(store, this, window, cx)
    });
    match anchor {
        Some(item) => workspace.anchor_modal_at(
            point(item.left(), item.top() - px(ANCHOR_GAP)),
            Corner::BottomLeft,
        ),
        None => workspace.anchor_modal(crate::popup::window_middle(window, WIDTH)),
    }
}

// --- The notification ---

/// A server's message as plain text: Markdown links become their text, code marks go; blank
/// lines between paragraphs too.
pub(crate) fn plain_message(message: &str) -> String {
    let mut text = String::with_capacity(message.len());
    let mut rest = message;
    // `[label](url)` → `label`.
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let link = after.find("](").and_then(|close| {
            let url = &after[close + 2..];
            url.find(')').map(|end| (close, close + 2 + end + 1))
        });
        match link {
            Some((label_end, link_end)) => {
                text.push_str(&rest[..open]);
                text.push_str(&after[..label_end]);
                rest = &after[link_end..];
            }
            None => {
                text.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    text.push_str(rest);
    text.replace('`', "")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The notification about a project the server couldn't load: the server's first line in the
/// title, the rest and the last error it printed (cargo's "unclosed table…") in the body.
pub(crate) fn problem_notification(
    key: &ServerKey,
    status: &ServerStatus,
    last_error: Option<String>,
) -> Notification {
    let message = plain_message(status.message.as_deref().unwrap_or(""));
    let mut lines = message.lines();
    let headline = lines
        .next()
        .map(str::to_string)
        .unwrap_or_else(|| tr("The project isn't loaded").to_string());
    let mut body: Vec<String> = lines.map(str::to_string).collect();
    body.extend(last_error);
    let notification = match status.health {
        Health::Error => Notification::error(format!("{}: {headline}", key.name)),
        _ => Notification::warning(format!("{}: {headline}", key.name)),
    };
    let notification = if body.is_empty() {
        notification
    } else {
        notification.body(body.join("\n"))
    };
    notification
        .action(tr("Restart"), RestartServer(key.clone()))
        .action(
            tr("Configure…"),
            ConfigureServer {
                name: key.name.clone(),
            },
        )
        .action(tr("Show Log"), ShowServerLog(key.clone()))
        .group(NotificationGroup::LanguageServers)
}

/// Whether `card` is the project problem notification of the server named `server`.
pub(crate) fn is_problem_of(card: &Notification, server: &str) -> bool {
    card.group == NotificationGroup::LanguageServers
        && card.offers(&[ShowServerLog::name_for_type()])
        && card
            .title
            .strip_prefix(server)
            .is_some_and(|rest| rest.starts_with(": "))
}

// --- The log ---

/// Writes the server's log to a file and opens it in a tab, read-only, at its end.
fn show_log(
    workspace: &mut Workspace,
    key: &ServerKey,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(text) = workspace.lsp.read(cx).log_text(key) else {
        return;
    };
    let path = log_path(&std::env::temp_dir(), key);
    let written = path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::write(&path, &text));
    if let Err(err) = written {
        let notification = Notification::error(trf("Couldn't save the log of {0}", &[&key.name]))
            .body(err.to_string())
            .group(NotificationGroup::LanguageServers);
        workspace.notify(notification, cx);
        return;
    }
    // The tab knows the file by its real path (`/private/var/…` for the temporary `/var/…`).
    let path = fs::canonicalize(&path).unwrap_or(path);
    let opened = path.clone();
    workspace.open_and(path, true, window, cx, move |workspace, cx| {
        let Some(editor) = workspace.active_editor() else {
            return;
        };
        editor.update(cx, |editor, cx| {
            if editor.document.path() != Some(opened.as_path()) {
                return;
            }
            // A tab opened before shows the log of then: the file is outside the project, nothing
            // watches it.
            editor.read_only = false;
            editor.reload(&text, cx);
            editor.read_only = true;
            let end = editor.document.text().len_chars();
            editor.select_range(end..end, cx);
        });
    });
}

/// `<temp>/flux-lsp-logs/<server> (<project>).log`.
fn log_path(temp: &Path, key: &ServerKey) -> PathBuf {
    let clean = |text: &str| -> String {
        text.chars()
            .map(|c| if c == '/' || c == ':' { '-' } else { c })
            .collect()
    };
    let project = key
        .root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = if project.is_empty() {
        format!("{}.log", clean(&key.name))
    } else {
        format!("{} ({}).log", clean(&key.name), clean(&project))
    };
    temp.join("flux-lsp-logs").join(name)
}

// --- Motion ---

/// A part of a popover that crossfades ([`motion::FADE`]) when what it shows changes — a server's
/// state, the country check's verdict — but not when it first shows: it comes with its popover.
/// `key` names what it shows (a state's kind, not its every percent). A candidate for
/// `crate::motion`.
#[derive(Default)]
pub(crate) struct Crossfade {
    shown: Option<u64>,
    changes: usize,
}

impl Crossfade {
    /// `element`, showing what `key` names: it fades in when that is new.
    pub(crate) fn apply<E>(&mut self, id: &'static str, key: u64, element: E) -> AnyElement
    where
        E: Styled + IntoElement + 'static,
    {
        if self.shown.is_some_and(|shown| shown != key) {
            self.changes += 1;
        }
        self.shown = Some(key);
        if self.changes == 0 {
            return element.into_any_element();
        }
        motion::Appear::fade().apply((id, self.changes), element)
    }
}

/// `value`'s hash: the key of a [`Crossfade`], a stable element id.
pub(crate) fn key_of(value: impl Hash) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// The popup's rows in motion: a server's state crossfades as it changes; a server that comes
/// while the popup is open fades in. The list's height glides ([`motion::glide_height`]) and the
/// popup hangs above the status bar by its bottom edge: its top edge moves smoothly as rows grow
/// (a project problem's cause line), come and go.
#[derive(Default)]
struct RowMotion {
    states: HashMap<ServerKey, Crossfade>,
    /// The servers of the popup's first frame (`None` before it): they come with the popup.
    first: Option<HashSet<ServerKey>>,
}

impl RowMotion {
    /// The rows of this frame: what is gone is forgotten.
    fn begin(&mut self, rows: &[ServerRow]) {
        let keys: HashSet<ServerKey> = rows.iter().map(|row| row.key.clone()).collect();
        self.states.retain(|key, _| keys.contains(key));
        self.first.get_or_insert(keys);
    }

    /// Whether the server `key` came after the popup opened.
    fn came(&self, key: &ServerKey) -> bool {
        self.first
            .as_ref()
            .is_some_and(|first| !first.contains(key))
    }

    /// The state line of the server `key`, `kind` naming what it says.
    fn state(&mut self, key: &ServerKey, kind: u64, line: Div) -> AnyElement {
        self.states
            .entry(key.clone())
            .or_default()
            .apply("lsp-row-state", kind, line)
    }
}

// --- The popup ---

/// A server as the popup shows it ([`LspStore::overview`]).
pub(crate) struct ServerRow {
    pub(crate) key: ServerKey,
    /// The first file extension it serves: its icon.
    pub(crate) extension: Option<String>,
    /// Open documents it serves.
    pub(crate) documents: usize,
    pub(crate) state: RowState,
    /// What it printed last as an error (cargo's), for the tooltip of a project problem.
    pub(crate) last_error: Option<String>,
    pub(crate) has_log: bool,
}

/// What a server is doing.
pub(crate) enum RowState {
    Starting,
    Installing(Option<f32>),
    Running,
    Busy {
        title: Option<String>,
        message: Option<String>,
        percentage: Option<u32>,
    },
    /// A project it couldn't load.
    Problem(ServerStatus),
    Stopped,
    Failed(String),
    NotInstalled,
    CannotInstall(String),
    InstallFailed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Quiet,
    Good,
    Busy,
    Warning,
    Error,
}

impl RowState {
    /// The state's line and its tone.
    fn describe(&self, documents: usize) -> (String, Tone) {
        match self {
            RowState::Starting => (tr("Starting…").to_string(), Tone::Busy),
            RowState::Installing(Some(fraction)) => (
                format!(
                    "{} {}%",
                    tr("Installing…"),
                    (fraction.clamp(0., 1.) * 100.).round() as u32
                ),
                Tone::Busy,
            ),
            RowState::Installing(None) => (tr("Installing…").to_string(), Tone::Busy),
            RowState::Running if documents == 0 => (tr("Running").to_string(), Tone::Good),
            RowState::Running => (
                format!(
                    "{} · {}",
                    tr("Running"),
                    trn(documents, "{n} file", "{n} files")
                ),
                Tone::Good,
            ),
            RowState::Busy {
                title,
                message,
                percentage,
            } => {
                let detail = match (percentage, message) {
                    (Some(percentage), _) => format!("{percentage}%"),
                    (None, Some(message)) => message.clone(),
                    (None, None) => String::new(),
                };
                let text = [title.as_deref().unwrap_or_default(), &detail]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                let text = if text.is_empty() {
                    tr("Working…").to_string()
                } else {
                    text
                };
                (text, Tone::Busy)
            }
            RowState::Problem(status) => {
                let message = plain_message(status.message.as_deref().unwrap_or(""));
                let headline = message
                    .lines()
                    .next()
                    .map(str::to_string)
                    .unwrap_or_else(|| tr("The project isn't loaded").to_string());
                let tone = if status.health == Health::Error {
                    Tone::Error
                } else {
                    Tone::Warning
                };
                (headline, tone)
            }
            RowState::Stopped => (tr("Stopped").to_string(), Tone::Quiet),
            RowState::Failed(reason) => (trf("Stopped: {0}", &[reason]), Tone::Error),
            RowState::NotInstalled => (tr("Not installed").to_string(), Tone::Quiet),
            RowState::CannotInstall(reason) => (reason.clone(), Tone::Quiet),
            RowState::InstallFailed(reason) => {
                (trf("Couldn't install: {0}", &[reason]), Tone::Error)
            }
        }
    }

    /// The process is there to stop.
    fn stoppable(&self) -> bool {
        matches!(
            self,
            RowState::Starting
                | RowState::Running
                | RowState::Busy { .. }
                | RowState::Problem(_)
                | RowState::Installing(_)
        )
    }

    /// Restart starts it again; for a stopped or failed one it is "Start".
    fn startable(&self) -> bool {
        !matches!(self, RowState::NotInstalled | RowState::CannotInstall(_))
    }

    fn running(&self) -> bool {
        matches!(
            self,
            RowState::Running | RowState::Busy { .. } | RowState::Problem(_)
        )
    }
}

/// The popup of the window's language servers.
pub struct ServersPopup {
    store: Entity<LspStore>,
    workspace: WeakEntity<Workspace>,
    /// The highlighted row: a server, or "Language Server Settings…" after them. The pointer moves
    /// it too, as in JetBrains' popups.
    selected: usize,
    focus_handle: FocusHandle,
    motion: RowMotion,
    _subscriptions: Vec<Subscription>,
}

impl ServersPopup {
    fn new(
        store: Entity<LspStore>,
        workspace: WeakEntity<Workspace>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let open = store.read(cx).status_ui.popup_open.clone();
        open.set(true);
        let subscriptions = vec![
            // The servers change while it is open: starting, indexing, stopped.
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.on_release(move |_, _| open.set(false)),
        ];
        Self {
            store,
            workspace,
            selected: 0,
            focus_handle: cx.focus_handle(),
            motion: RowMotion::default(),
            _subscriptions: subscriptions,
        }
    }

    fn rows(&self, cx: &App) -> Vec<ServerRow> {
        self.store.read(cx).overview()
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.rows(cx).len() + 1;
        self.selected = if forward {
            (self.selected + 1) % count
        } else {
            (self.selected + count - 1) % count
        };
        cx.notify();
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.selected != index {
            self.selected = index;
            cx.notify();
        }
    }

    /// ↵: the selected server starts anew; on "Language Server Settings…", Settings open.
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.rows(cx);
        match rows.get(self.selected) {
            Some(row) if row.state.startable() => {
                let key = row.key.clone();
                self.store
                    .update(cx, |store, cx| store.restart_key(&key, cx));
            }
            Some(_) => {}
            None => self.open_settings(window, cx),
        }
    }

    /// ⌘F2: the selected server stops.
    fn stop_selected(&mut self, cx: &mut Context<Self>) {
        let rows = self.rows(cx);
        if let Some(row) = rows.get(self.selected).filter(|row| row.state.stoppable()) {
            let key = row.key.clone();
            self.store.update(cx, |store, cx| store.stop_key(&key, cx));
        }
    }

    /// ⌘, and ⌘L: Configure… and Show Log of the selected server.
    fn act_on_selected(&mut self, log: bool, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.rows(cx);
        let Some(row) = rows.get(self.selected) else {
            return;
        };
        let action: Box<dyn Action> = if log {
            if !row.has_log {
                return;
            }
            Box::new(ShowServerLog(row.key.clone()))
        } else {
            Box::new(ConfigureServer {
                name: row.key.name.clone(),
            })
        };
        self.close_and(action, window, cx);
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    crate::settings_view::open(workspace, Section::LanguageServers, window, cx)
                })
                .ok();
        });
    }

    /// An action of the window for a row (Configure, Show Log): after the popup is gone and focus
    /// is back where it was.
    fn close_and(&mut self, action: Box<dyn Action>, window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
        window.defer(cx, move |window, cx| window.dispatch_action(action, cx));
    }

    fn render_row(
        &mut self,
        index: usize,
        row: &ServerRow,
        roots: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let ui = Theme::ui(cx);
        let selected = self.selected == index;
        let (state, tone) = row.state.describe(row.documents);
        let color = match tone {
            Tone::Quiet => ui.dim,
            Tone::Good => ui.text_muted,
            Tone::Busy => ui.accent_text,
            Tone::Warning => ui.warning,
            Tone::Error => ui.error,
        };
        let glyph = match tone {
            Tone::Warning => Some(icon(IconName::Warning, color).size(px(12.))),
            Tone::Error => Some(icon(IconName::Error, color).size(px(12.))),
            _ => None,
        };
        // The whole story in the tooltip: the server's message, the error it printed.
        let details = match &row.state {
            RowState::Problem(status) => {
                let mut lines = vec![plain_message(status.message.as_deref().unwrap_or(""))];
                lines.extend(row.last_error.clone());
                Some(lines.join("\n"))
            }
            RowState::Failed(reason) | RowState::InstallFailed(reason) => Some(reason.clone()),
            _ => None,
        };
        let file = file_icon(
            &format!("file.{}", row.extension.as_deref().unwrap_or("txt")),
            &ui,
        );
        let mut buttons: Vec<AnyElement> = Vec::new();
        if row.state.startable() {
            let key = row.key.clone();
            let label = if row.state.running() || row.state.stoppable() {
                tr("Restart")
            } else {
                tr("Start Server")
            };
            buttons.push(
                ui::icon_button(("lsp-row-restart", index), IconName::Refresh, ui)
                    .tooltip(ui::tooltip(label, Some("↵".into())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let key = key.clone();
                        this.store
                            .update(cx, |store, cx| store.restart_key(&key, cx));
                    }))
                    .into_any_element(),
            );
        }
        if row.state.stoppable() {
            let key = row.key.clone();
            buttons.push(
                ui::icon_button(("lsp-row-stop", index), IconName::Stop, ui)
                    .tooltip(ui::tooltip(tr("Stop"), Some("⌘F2".into())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let key = key.clone();
                        this.store.update(cx, |store, cx| store.stop_key(&key, cx));
                    }))
                    .into_any_element(),
            );
        }
        {
            let name = row.key.name.clone();
            buttons.push(
                ui::icon_button(("lsp-row-configure", index), IconName::Settings, ui)
                    .tooltip(ui::tooltip(tr("Configure…"), Some("⌘,".into())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        let action = ConfigureServer { name: name.clone() };
                        this.close_and(Box::new(action), window, cx)
                    }))
                    .into_any_element(),
            );
        }
        if row.has_log {
            let key = row.key.clone();
            buttons.push(
                ui::icon_button(("lsp-row-log", index), IconName::Terminal, ui)
                    .tooltip(ui::tooltip(tr("Show Log"), Some("⌘L".into())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.close_and(Box::new(ShowServerLog(key.clone())), window, cx)
                    }))
                    .into_any_element(),
            );
        }
        let state_line = div()
            .flex()
            .items_center()
            .gap_1()
            .min_w_0()
            .text_size(px(theme::TEXT_SM))
            .text_color(color)
            .children(glyph)
            .child(div().min_w_0().truncate().child(state));
        // Another state (not another percent of the same): it fades in.
        let state_line = self.motion.state(
            &row.key,
            key_of(std::mem::discriminant(&row.state)),
            state_line,
        );
        // What it couldn't load the project over, when it printed that (cargo's error).
        let cause = match &row.state {
            RowState::Problem(_) => row
                .last_error
                .as_deref()
                .and_then(|error| error.lines().next())
                .map(|error| {
                    div()
                        .flex()
                        .min_w_0()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.text_muted)
                        .child(div().flex_1().min_w_0().truncate().child(error.to_string()))
                }),
            _ => None,
        };
        div()
            .id(("lsp-row", index))
            .min_h(px(ROW_HEIGHT))
            .py_1p5()
            .px_2()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .rounded(px(RADIUS_MD))
            .when(selected, |row| row.bg(ui.list_selected))
            .when_some(details, |row, details| {
                row.tooltip(ui::tooltip(details, None))
            })
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.select(index, cx)
                }
            }))
            .child(
                div()
                    .flex_none()
                    .w(px(ICON_COLUMN))
                    .flex()
                    .justify_center()
                    .child(file.render().size(px(16.))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .flex_none()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(ui.foreground)
                                    .child(row.key.name.clone()),
                            )
                            .when(roots, |line| {
                                line.child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(theme::TEXT_XS))
                                        .text_color(ui.dim)
                                        .child(tilde(&row.key.root)),
                                )
                            }),
                    )
                    .child(state_line)
                    .children(cause),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .children(buttons),
            )
    }

    /// "Language Server Settings…": Settings → Language Servers.
    fn render_settings(&self, index: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let ui = Theme::ui(cx);
        let selected = self.selected == index;
        div()
            .id("lsp-popup-settings")
            .h(px(ACTION_HEIGHT))
            .px_2()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .rounded(px(RADIUS_SM))
            .cursor_pointer()
            .when(selected, |row| row.bg(ui.list_selected))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.select(index, cx)
                }
            }))
            .on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| this.open_settings(window, cx)),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(ICON_COLUMN))
                    .flex()
                    .justify_center()
                    .child(icon(IconName::Settings, ui.text_muted).size(px(15.))),
            )
            .child(
                div()
                    .flex_1()
                    .text_color(ui.foreground)
                    .child(tr("Language Server Settings…")),
            )
    }
}

impl EventEmitter<DismissEvent> for ServersPopup {}

impl Focusable for ServersPopup {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ServersPopup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let rows = self.rows(cx);
        self.selected = self.selected.min(rows.len());
        // Servers of several roots (files from several repositories without a project): each
        // shows its root.
        let roots = rows.iter().any(|row| {
            rows.first()
                .is_some_and(|first| first.key.root != row.key.root)
        });
        // A server that comes while the popup is open fades in (`crate::motion`).
        self.motion.begin(&rows);
        let items: Vec<AnyElement> = rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let element = self.render_row(index, row, roots, cx);
                if self.motion.came(&row.key) {
                    motion::Appear::fade().apply(("lsp-row-in", key_of(&row.key)), element)
                } else {
                    element.into_any_element()
                }
            })
            .collect();
        let empty = items.is_empty().then(|| {
            div()
                .h(px(ROW_HEIGHT))
                .px_2()
                .flex()
                .items_center()
                .gap_3()
                .text_color(ui.dim)
                .child(
                    div()
                        .flex_none()
                        .w(px(ICON_COLUMN))
                        .flex()
                        .justify_center()
                        .child(icon(IconName::Command, ui.dim).size(px(15.))),
                )
                .child(tr("Language servers start with the files they serve"))
        });
        let settings = self.render_settings(rows.len(), cx);
        crate::popup::panel(ui)
            .id("lsp-servers-popup")
            .key_context("LspServersPopup")
            .track_focus(&self.focus_handle)
            .occlude()
            .w(px(WIDTH))
            .flex()
            .flex_col()
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm(window, cx)))
            .on_action(cx.listener(|this, _: &StopSelected, _, cx| this.stop_selected(cx)))
            .on_action(cx.listener(|this, _: &ConfigureSelected, window, cx| {
                this.act_on_selected(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &LogSelected, window, cx| {
                this.act_on_selected(true, window, cx)
            }))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .child(
                div()
                    .px(px(INSET + 8.))
                    .pt_3()
                    .pb_1p5()
                    .child(ui::section_label(tr("Language Servers"), ui)),
            )
            // Rows that grow, come and go: the list's height glides, the popup's top edge with it.
            .child(motion::glide_height(
                "lsp-servers-list",
                div()
                    .px(px(INSET))
                    .pb(px(INSET))
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .children(items)
                    .children(empty),
            ))
            .child(ui::divider(ui))
            .child(div().p(px(INSET)).child(settings))
            .child(ui::divider(ui))
            .child(
                div()
                    .flex_none()
                    .h(px(FOOTER_HEIGHT))
                    .px(px(INSET + 8.))
                    .flex()
                    .items_center()
                    .child(ui::hint_bar(
                        &[
                            ("↵", tr("restart")),
                            ("⌘F2", tr("stop")),
                            ("⌘L", tr("log")),
                            ("esc", tr("close")),
                        ],
                        ui,
                    )),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str, root: &str) -> ServerKey {
        ServerKey {
            name: name.into(),
            root: PathBuf::from(root),
        }
    }

    #[test]
    fn markdown_of_a_message_becomes_plain_text() {
        let message = "Failed to discover workspace.\nConsider adding the `Cargo.toml` of the \
                       workspace to the [`linkedProjects`](https://rust-analyzer.github.io/book/\
                       configuration.html#linkedProjects) setting.\n\nFailed to load workspaces.";
        assert_eq!(
            plain_message(message),
            "Failed to discover workspace.\nConsider adding the Cargo.toml of the workspace to \
             the linkedProjects setting.\nFailed to load workspaces."
        );
        assert_eq!(plain_message("a [b] c [d](e"), "a [b] c [d](e");
        assert_eq!(plain_message(""), "");
    }

    #[test]
    fn the_problem_notification_and_its_expiry() {
        let status = ServerStatus {
            health: Health::Error,
            quiescent: true,
            message: Some("Failed to load workspaces.".into()),
        };
        let server = key("rust-analyzer", "/work/proj");
        let card = problem_notification(
            &server,
            &status,
            Some("unclosed table, expected `]` (Cargo.toml:5:14)".into()),
        );
        assert_eq!(
            card.title.as_ref(),
            "rust-analyzer: Failed to load workspaces."
        );
        assert_eq!(
            card.body.as_ref().map(|body| body.as_ref()),
            Some("unclosed table, expected `]` (Cargo.toml:5:14)")
        );
        assert_eq!(card.kind, crate::notifications::NotificationKind::Error);
        assert!(card.offers(&["language_server::RestartServer"]));
        assert!(card.offers(&["language_server::ConfigureServer"]));
        assert!(is_problem_of(&card, "rust-analyzer"));
        assert!(!is_problem_of(&card, "rust"));
        // A server's own message (window/showMessage) is not it.
        let message = Notification::error("rust-analyzer")
            .body("boom")
            .group(NotificationGroup::LanguageServers);
        assert!(!is_problem_of(&message, "rust-analyzer"));

        let warning = ServerStatus {
            health: Health::Warning,
            quiescent: true,
            message: Some("Failed to discover workspace.\nConsider it.".into()),
        };
        let card = problem_notification(&server, &warning, None);
        assert_eq!(card.kind, crate::notifications::NotificationKind::Warning);
        assert_eq!(
            card.body.as_ref().map(|body| body.as_ref()),
            Some("Consider it.")
        );
    }

    #[test]
    fn the_log_file_is_named_after_the_server_and_the_project() {
        let temp = Path::new("/tmp/x");
        assert_eq!(
            log_path(temp, &key("rust-analyzer", "/work/flux")),
            PathBuf::from("/tmp/x/flux-lsp-logs/rust-analyzer (flux).log")
        );
        assert_eq!(
            log_path(temp, &key("a/b", "/")),
            PathBuf::from("/tmp/x/flux-lsp-logs/a-b.log")
        );
    }

    #[test]
    fn states_read_short_and_say_what_can_be_done() {
        let (text, tone) = RowState::Running.describe(3);
        assert_eq!((text.as_str(), tone), ("Running · 3 files", Tone::Good));
        let (text, _) = RowState::Busy {
            title: Some("Indexing".into()),
            message: Some("12/40".into()),
            percentage: Some(30),
        }
        .describe(1);
        assert_eq!(text, "Indexing 30%");
        let problem = RowState::Problem(ServerStatus {
            health: Health::Warning,
            quiescent: true,
            message: Some("Failed to discover workspace.\nMore.".into()),
        });
        assert_eq!(
            problem.describe(0),
            ("Failed to discover workspace.".to_string(), Tone::Warning)
        );
        assert!(problem.stoppable() && problem.startable() && problem.running());
        assert!(!RowState::Stopped.stoppable() && RowState::Stopped.startable());
        assert!(!RowState::NotInstalled.startable());
        assert!(RowState::Failed("exit 1".into()).startable());
    }
}
