//! The Claude console (part «Пульт Claude», wiki Roadmap): the left island's tool window in place
//! of the project tree, as the commit window. It lists the project's sessions — "Now" (live ones
//! and unloaded ones not archived: a status dot, the title, one phrase of what happens), then the
//! history in three groups that fold: "Recent" (the last 12 hours), "This Week" (older, within 7
//! days), "All" (every session of this folder; archived ones behind "Show archived") — and the
//! subscription limits at the
//! bottom. A session's chat lives only in the editor's tabs; closing its tab doesn't end the session
//! ([`crate::claude::ClaudeStore::console`]).
//!
//! A waiting session's question stays open under it (digits answer it too); a plan's approval,
//! a done session's Review and Rollback show on the row under the mouse or selected. ⌘Esc focuses
//! the console; a click shows the session's tab and keeps the keyboard here, ↵ and a double click
//! go into its chat; F2 renames, ⌘⌫ closes (out of "Now", into the history), the menu archives
//! (out of the history too), ⇧F10 opens the menu, ⌘F filters "All". While `claude` isn't ready, the console says what to do about it: install (the command
//! typed into a terminal), sign in, try again.
//!
//! The country check (part «Флаг страны», [`crate::region_guard`]): the flag next to the title, and
//! while Claude is blocked a banner over the list says why, with Check Again; asked again after a
//! block, it says Flux is checking.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime};

use gpui::{
    Action, Animation, AnimationExt, AnyElement, App, ClickEvent, Context, DismissEvent, Entity,
    EntityId, EventEmitter, FocusHandle, Focusable, FontWeight, KeyBinding, MouseButton,
    MouseDownEvent, NoAction, Pixels, Point, Render, ScrollHandle, SharedString, Subscription,
    Window, actions, div, point, prelude::*, px,
};

use flux_claude::session::Question;
use flux_claude::{Answer, PendingKind, SavedSession};

use crate::claude::{self, ClaudeStore, CliState, ConsoleItem};
use crate::claude_history;
use crate::claude_session::ClaudeSession;
use crate::claude_status::{self, GlanceState, Tone};
use crate::context_menu::ContextMenu;
use crate::git_log::local_offset;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::input::{InputEvent, TextInput};
use crate::region_guard::{self, Verdict};
use crate::region_status;
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_MD, RADIUS_SM};

const CONTEXT: &str = "ClaudeConsole";

actions!(
    claude_console,
    [
        /// ↑: the previous row.
        SelectPrevious,
        /// ↓: the next row.
        SelectNext,
        /// ↵: the selected session's chat in its editor tab.
        OpenSelected,
        /// F2: renames the selected session.
        RenameSelected,
        /// ⌘⌫: the selected session leaves "Now" (its process stops) for the history.
        CloseSelected,
        /// The selected session to the archive, out of the history (from the archive: back).
        ArchiveSelected,
        /// The selected session goes on in the terminal CLI.
        OpenSelectedInTerminal,
        /// Stops the selected session's running turn.
        StopSelected,
        /// ⇧F10: the selected row's menu.
        ShowMenu,
        /// ⌘F: the filter of "All" (the group opens).
        FocusFilter,
        /// ⌘↵: Next / Send on the selected session's question card (several answers).
        NextQuestion,
        /// Esc: from the filter back to the list, from the list to the editor.
        Leave,
    ]
);

/// 1–9: the selected session's question gets that option.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = claude_console, no_json)]
pub struct AnswerOption(pub usize);

/// The header, as the commit window's.
const HEADER_HEIGHT: f32 = 46.;
/// Rows are inset from the island's edges; the highlight is a rounded box inside.
const ROW_INSET: f32 = 8.;
const ROW_PADDING: f32 = 12.;
/// The gap between the sessions of "Now" (the author asked for air between them).
const SESSION_GAP: f32 = 6.;
/// The status column of a session: its dot, centred on the title's line.
const STATUS_WIDTH: f32 = 16.;
const STATUS_GAP: f32 = 12.;
/// A history row: one line, the title and when.
const HISTORY_ROW_HEIGHT: f32 = 28.;
/// The height a waiting question slides open to at most (`motion::SMOOTH`).
const OPEN_HEIGHT: f32 = 600.;
/// How many options a question offers in the console (digits 1–9).
const MAX_OPTIONS: usize = 9;
/// How long "Recent" and "This Week" reach back (the author: "Recent" — 12 hours).
const RECENT: Duration = Duration::from_secs(12 * 60 * 60);
const WEEK: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// The onboarding column's width at most.
const ONBOARDING_WIDTH: f32 = 300.;

pub fn init(cx: &mut App) {
    let context = Some(CONTEXT);
    let mut bindings = vec![
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("enter", OpenSelected, context),
        KeyBinding::new("f2", RenameSelected, context),
        KeyBinding::new("cmd-backspace", CloseSelected, context),
        KeyBinding::new("shift-f10", ShowMenu, context),
        KeyBinding::new("cmd-f", FocusFilter, context),
        KeyBinding::new("cmd-enter", NextQuestion, context),
        KeyBinding::new("escape", Leave, context),
    ];
    for digit in 1..=MAX_OPTIONS {
        let key = digit.to_string();
        bindings.push(KeyBinding::new(&key, AnswerOption(digit - 1), context));
        // Typed into the filter, a digit is a digit.
        bindings.push(KeyBinding::new(
            &key,
            NoAction,
            Some("ClaudeConsole > TextInput"),
        ));
    }
    cx.bind_keys(bindings);
}

/// A session the console acts on: a live one (its process may be running or not yet started) or
/// a saved one (unloaded, or from the history).
#[derive(Clone, Debug, PartialEq)]
pub enum SessionRef {
    Live(Entity<ClaudeSession>),
    Saved(SavedSession),
}

/// What the console asks the window to do: everything that needs the editor's tabs, a dialog or a
/// terminal. What only touches the session or the store (answering a question, approving a plan,
/// archiving an idle saved session) the console does itself.
#[derive(Clone, Debug, PartialEq)]
pub enum ClaudeConsoleEvent {
    /// "+" and the empty state: a new session in a new editor tab.
    NewSession,
    /// Show the session's chat in its editor tab (opening one; a saved session is resumed) and go
    /// into it: ↵, a double click.
    Open(SessionRef),
    /// The same tab, but the keyboard stays in the console: a click.
    Show(SessionRef),
    /// Rename (F2).
    Rename(SessionRef),
    /// Go on with the session in the terminal CLI.
    OpenInTerminal(SessionRef),
    /// Stop the running turn (the process stays).
    Stop(Entity<ClaudeSession>),
    /// Close: out of "Now" into the history; a live session ends (asked when Claude works), its
    /// tab closes.
    Close(SessionRef),
    /// Move to the archive: a live session is unloaded first (asked when Claude works).
    Archive(SessionRef),
    /// A done session's Review: its changes as the commit window's "Claude · <session>" group.
    Review(Entity<ClaudeSession>),
    /// A done session's Rollback: its changes undone (asked first).
    Rollback(Entity<ClaudeSession>),
    /// Focus goes back to the editor (Esc).
    Dismiss,
}

/// The groups of the list, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Section {
    Now,
    Recent,
    Week,
    All,
}

/// A row: a session in its group (a saved session shows in "Recent" and in "All").
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RowKey {
    section: Section,
    id: RowId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum RowId {
    Live(EntityId),
    Saved(String),
}

#[derive(Clone)]
struct Row {
    key: RowKey,
    item: SessionRef,
}

struct Menu {
    menu: Entity<ContextMenu>,
    position: Point<Pixels>,
    _subscriptions: [Subscription; 2],
}

pub struct ClaudeConsole {
    store: Entity<ClaudeStore>,
    focus_handle: FocusHandle,
    /// Filters "All" by title and first prompt.
    filter: Entity<TextInput>,
    selected: Option<RowKey>,
    hovered: Option<RowKey>,
    /// The history's groups that are open: "Recent" and "This Week" by default, not "All" (it can
    /// be long).
    recent_open: bool,
    week_open: bool,
    all_open: bool,
    /// "All" shows the archived sessions too.
    show_archived: bool,
    /// The answers picked so far for a question card, by its request id: several questions go
    /// step by step, several answers toggle.
    picks: HashMap<String, Picks>,
    menu: Option<Menu>,
    scroll: ScrollHandle,
    /// The country check's last answer blocked Claude: while it asks again, the banner says so.
    region_blocked: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ClaudeConsoleEvent> for ClaudeConsole {}

impl ClaudeConsole {
    pub fn new(store: Entity<ClaudeStore>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| TextInput::new(tr("Filter sessions"), cx).icon(IconName::Search));
        let mut subscriptions = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe(&filter, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        // The console may be drawn from its cache: it follows the country check itself.
        if let Some(guard) = region_guard::global(cx) {
            subscriptions.push(cx.observe(&guard, |this, guard, cx| {
                let verdict = guard.read(cx).verdict();
                if *verdict != Verdict::Checking {
                    this.region_blocked = verdict.blocks();
                }
                cx.notify()
            }));
        }
        store.update(cx, |store, cx| store.refresh_saved(cx));
        Self {
            store,
            focus_handle: cx.focus_handle(),
            filter,
            selected: None,
            hovered: None,
            recent_open: true,
            week_open: true,
            all_open: false,
            show_archived: false,
            picks: HashMap::new(),
            menu: None,
            scroll: ScrollHandle::new(),
            region_blocked: region_guard::verdict(cx).blocks(),
            _subscriptions: subscriptions,
        }
    }

    /// Whether the keyboard is in the console (⌘Esc from here goes back to the last chat).
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.contains_focused(window, cx)
            || self
                .menu
                .as_ref()
                .is_some_and(|menu| menu.menu.focus_handle(cx).contains_focused(window, cx))
    }

    /// ⌘Esc: the keyboard to the console, the selection on the active or first session.
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        let rows = self.rows(cx);
        if self
            .selected
            .as_ref()
            .is_none_or(|selected| !rows.iter().any(|row| &row.key == selected))
        {
            self.selected = rows.first().map(|row| row.key.clone());
        }
        cx.notify();
    }

    // --- The rows ---

    /// Every row in its order: "Now", "Recent", "This Week", then "All" when open (filtered).
    fn rows(&self, cx: &App) -> Vec<Row> {
        let store = self.store.read(cx);
        let now_items = store.console(cx);
        let mut in_now: HashSet<String> = HashSet::new();
        let mut rows: Vec<Row> = Vec::new();
        for item in now_items {
            match item {
                ConsoleItem::Live(session) => {
                    if let Some(id) = session.read(cx).session_id() {
                        in_now.insert(id.to_string());
                    }
                    rows.push(Row {
                        key: RowKey {
                            section: Section::Now,
                            id: RowId::Live(session.entity_id()),
                        },
                        item: SessionRef::Live(session),
                    });
                }
                ConsoleItem::Unloaded(saved) => {
                    in_now.insert(saved.id.clone());
                    rows.push(Row {
                        key: RowKey {
                            section: Section::Now,
                            id: RowId::Saved(saved.id.clone()),
                        },
                        item: SessionRef::Saved(saved),
                    });
                }
            }
        }
        let saved = store.saved_sessions().unwrap_or_default();
        let (recent, week) = group_history(
            saved,
            &in_now,
            |id| store.is_archived(id),
            SystemTime::now(),
        );
        let saved_row = |section, saved: &SavedSession| Row {
            key: RowKey {
                section,
                id: RowId::Saved(saved.id.clone()),
            },
            item: SessionRef::Saved(saved.clone()),
        };
        if self.recent_open {
            rows.extend(
                recent
                    .into_iter()
                    .map(|saved| saved_row(Section::Recent, saved)),
            );
        }
        if self.week_open {
            rows.extend(
                week.into_iter()
                    .map(|saved| saved_row(Section::Week, saved)),
            );
        }
        if self.all_open {
            let query = self.filter.read(cx).text();
            rows.extend(
                saved
                    .iter()
                    .filter(|saved| self.show_archived || !store.is_archived(&saved.id))
                    .filter(|saved| matches_filter(saved, &query))
                    .map(|saved| saved_row(Section::All, saved)),
            );
        }
        rows
    }

    fn selected_row(&self, cx: &App) -> Option<Row> {
        let selected = self.selected.as_ref()?;
        self.rows(cx).into_iter().find(|row| &row.key == selected)
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let rows = self.rows(cx);
        if rows.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|selected| rows.iter().position(|row| &row.key == selected));
        let next = match current {
            None if delta > 0 => 0,
            None => rows.len() - 1,
            Some(index) => (index as isize + delta).clamp(0, rows.len() as isize - 1) as usize,
        };
        self.selected = Some(rows[next].key.clone());
        cx.notify();
    }

    // --- Actions ---

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, cx);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, cx);
    }

    fn open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.selected_row(cx) {
            cx.emit(ClaudeConsoleEvent::Open(row.item));
        }
    }

    fn rename_selected(&mut self, _: &RenameSelected, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.selected_row(cx) {
            cx.emit(ClaudeConsoleEvent::Rename(row.item));
        }
    }

    fn open_selected_in_terminal(
        &mut self,
        _: &OpenSelectedInTerminal,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(row) = self.selected_row(cx) {
            cx.emit(ClaudeConsoleEvent::OpenInTerminal(row.item));
        }
    }

    fn stop_selected(&mut self, _: &StopSelected, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(SessionRef::Live(session)) = self.selected_row(cx).map(|row| row.item) {
            cx.emit(ClaudeConsoleEvent::Stop(session));
        }
    }

    fn close_selected(&mut self, _: &CloseSelected, _: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row(cx) else {
            return;
        };
        // Only "Now" has something to close: the history is closed already.
        if row.key.section != Section::Now {
            return;
        }
        self.select_past(&row, cx);
        cx.emit(ClaudeConsoleEvent::Close(row.item));
    }

    fn archive_selected(&mut self, _: &ArchiveSelected, _: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row(cx) else {
            return;
        };
        self.select_past(&row, cx);
        self.archive(row.item, cx);
    }

    /// The selection goes to the row after one that leaves (or before it, at the end).
    fn select_past(&mut self, row: &Row, cx: &App) {
        let rows = self.rows(cx);
        if let Some(index) = rows.iter().position(|other| other.key == row.key) {
            self.selected = rows
                .get(index + 1)
                .or_else(|| index.checked_sub(1).and_then(|index| rows.get(index)))
                .map(|row| row.key.clone());
        }
    }

    /// The archive: a live session goes through the window (it unloads it, asking when Claude
    /// works); a saved one the store moves itself — and one already there comes back.
    fn archive(&mut self, item: SessionRef, cx: &mut Context<Self>) {
        match item {
            SessionRef::Live(_) => cx.emit(ClaudeConsoleEvent::Archive(item)),
            SessionRef::Saved(saved) => self.store.update(cx, |store, cx| {
                if store.is_archived(&saved.id) {
                    store.unarchive(&saved.id, cx);
                } else {
                    store.archive(&saved.id, cx);
                }
            }),
        }
        cx.notify();
    }

    fn show_menu_action(&mut self, _: &ShowMenu, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_row(cx).is_none() {
            return;
        }
        // Without a mouse position: at the top of the list, inset.
        let origin = self.scroll.bounds().origin;
        let position = point(origin.x + px(24.), origin.y + px(24.));
        self.open_row_menu(position, window, cx);
    }

    fn focus_filter(&mut self, _: &FocusFilter, window: &mut Window, cx: &mut Context<Self>) {
        self.all_open = true;
        let focus = self.filter.focus_handle(cx);
        window.focus(&focus);
        self.filter.update(cx, |filter, cx| filter.select_all(cx));
        cx.notify();
    }

    fn leave(&mut self, _: &Leave, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter.focus_handle(cx).is_focused(window) {
            window.focus(&self.focus_handle);
            cx.notify();
            return;
        }
        cx.emit(ClaudeConsoleEvent::Dismiss);
    }

    fn next_question_action(&mut self, _: &NextQuestion, _: &mut Window, cx: &mut Context<Self>) {
        let Some(SessionRef::Live(session)) = self.selected_row(cx).map(|row| row.item) else {
            return;
        };
        self.next_question(&session, cx);
    }

    fn answer_option(&mut self, action: &AnswerOption, _: &mut Window, cx: &mut Context<Self>) {
        let Some(SessionRef::Live(session)) = self.selected_row(cx).map(|row| row.item) else {
            return;
        };
        self.answer(&session, action.0, cx);
    }

    /// A question's option `index` for the session, on the card's current step: one answer —
    /// taken and the next question (the last one sends); several answers — toggled. The one past
    /// the options opens the chat ("Other", a long answer).
    fn answer(&mut self, session: &Entity<ClaudeSession>, index: usize, cx: &mut Context<Self>) {
        let Some(quick) = self.quick_question(session, cx) else {
            return;
        };
        let Some(question) = quick.questions.get(quick.step) else {
            return;
        };
        let shown = question.options.len().min(MAX_OPTIONS - 1);
        if index == shown {
            cx.emit(ClaudeConsoleEvent::Open(SessionRef::Live(session.clone())));
            return;
        }
        if index > shown {
            return;
        }
        let picks = self
            .picks
            .entry(quick.request.clone())
            .or_insert_with(|| Picks::new(quick.questions.len()));
        let chosen = &mut picks.chosen[quick.step];
        if question.multi_select {
            match chosen.iter().position(|option| *option == index) {
                Some(position) => {
                    chosen.remove(position);
                }
                None => chosen.push(index),
            }
            cx.notify();
            return;
        }
        *chosen = vec![index];
        self.next_question(session, cx);
    }

    /// Next (Send on the last question): once the current step has an answer.
    fn next_question(&mut self, session: &Entity<ClaudeSession>, cx: &mut Context<Self>) {
        let Some(quick) = self.quick_question(session, cx) else {
            return;
        };
        let Some(picks) = self.picks.get_mut(&quick.request) else {
            return;
        };
        if picks.chosen.get(quick.step).is_none_or(Vec::is_empty) {
            return;
        }
        if quick.step + 1 < quick.questions.len() {
            picks.step = quick.step + 1;
            cx.notify();
            return;
        }
        let answers = crate::claude_cards::question_answers(&quick.questions, &picks.chosen, &[]);
        self.picks.remove(&quick.request);
        let request = quick.request;
        session.update(cx, |session, cx| {
            session.answer(&request, Answer::Answers(answers), cx)
        });
    }

    /// The session's question card as the console offers it: its questions, the current step and
    /// what is picked on it.
    fn quick_question(&self, session: &Entity<ClaudeSession>, cx: &App) -> Option<QuickQuestion> {
        let pending = session.read(cx).model().pending.first()?;
        let PendingKind::Questions(questions) = &pending.kind else {
            return None;
        };
        if questions.is_empty() {
            return None;
        }
        let picks = self.picks.get(&pending.id);
        let step = picks.map_or(0, |picks| picks.step).min(questions.len() - 1);
        Some(QuickQuestion {
            request: pending.id.clone(),
            questions: questions.clone(),
            step,
            chosen: picks
                .and_then(|picks| picks.chosen.get(step).cloned())
                .unwrap_or_default(),
        })
    }

    /// A plan's approval from the console (the plain one: edits are still asked about).
    fn approve_plan(&mut self, session: &Entity<ClaudeSession>, cx: &mut Context<Self>) {
        let request = session
            .read(cx)
            .model()
            .pending
            .first()
            .and_then(|pending| {
                matches!(pending.kind, PendingKind::Plan { .. }).then(|| pending.id.clone())
            });
        if let Some(request) = request {
            let answer = Answer::ApprovePlan {
                accept_edits: false,
            };
            session.update(cx, |session, cx| session.answer(&request, answer, cx));
        }
    }

    // --- Menus ---

    /// The row's menu: it acts on the selected row.
    fn open_row_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.selected_row(cx) else {
            return;
        };
        let store = self.store.read(cx);
        let ready = store.is_ready();
        let in_now = row.key.section == Section::Now;
        let (working, archived) = match &row.item {
            SessionRef::Live(session) => (session.read(cx).model().is_working(), false),
            SessionRef::Saved(saved) => (false, store.is_archived(&saved.id)),
        };
        window.focus(&self.focus_handle);
        let menu = cx.new(|cx| {
            ContextMenu::new(window, cx)
                .entry_if(ready, tr("Open"), OpenSelected)
                .entry(tr("Rename…"), RenameSelected)
                .entry_if(ready, tr("Open in Terminal"), OpenSelectedInTerminal)
                .separator()
                .entry_if(working, tr("Stop"), StopSelected)
                .entry_if(in_now, tr("Close"), CloseSelected)
                .entry(
                    if archived {
                        tr("Restore from the Archive")
                    } else {
                        tr("Archive")
                    },
                    ArchiveSelected,
                )
        });
        self.show_menu(menu, position, window, cx);
    }

    /// ⋯ in the header.
    fn open_more_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = self.store.read(cx);
        let ready = store.is_ready();
        let signed_out = matches!(store.cli(), CliState::SignedOut { .. });
        window.focus(&self.focus_handle);
        let menu = cx.new(|cx| {
            let mut menu = ContextMenu::new(window, cx)
                .entry_if(ready, tr("New Session"), claude::NewSession)
                .entry_if(signed_out, tr("Sign In"), claude::SignIn);
            if !ready {
                menu = menu.entry(tr("Check Again"), claude::CheckAgain);
            }
            menu.separator()
                .entry(tr("Claude Code Settings…"), claude::OpenSettings)
        });
        self.show_menu(menu, position, window, cx);
    }

    fn show_menu(
        &mut self,
        menu: Entity<ContextMenu>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = menu.focus_handle(cx);
        let subscriptions = [
            cx.subscribe_in(&menu, window, |this, menu, _: &DismissEvent, window, cx| {
                this.close_menu(menu, window, cx)
            }),
            cx.on_focus_out(&focus, window, {
                let menu = menu.clone();
                move |this, _, window, cx| this.close_menu(&menu, window, cx)
            }),
        ];
        window.focus(&focus);
        self.menu = Some(Menu {
            menu,
            position,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    fn close_menu(
        &mut self,
        menu: &Entity<ContextMenu>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu.as_ref().is_none_or(|open| open.menu != *menu) {
            return;
        }
        let had_focus = menu.focus_handle(cx).contains_focused(window, cx);
        self.menu = None;
        if had_focus {
            window.focus(&self.focus_handle);
        }
        cx.notify();
    }

    // --- Drawing ---

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let ready = self.store.read(cx).is_ready();
        let new_keys = ui::shortcut_for(&claude::NewSession, cx);
        // Where the network sees this Mac: the flag, the rest in its tooltip.
        let region = region_status::mark(
            &region_guard::verdict(cx),
            13.,
            "claude-console-region-pulse",
            ui,
        )
        .map(|mark| {
            div()
                .id("claude-console-region")
                .flex_none()
                .flex()
                .items_center()
                .child(mark)
                .tooltip(|window, cx| {
                    let text = region_status::tooltip_text(&region_guard::verdict(cx));
                    ui::tooltip(text, None)(window, cx)
                })
        });
        div()
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .pl(px(ROW_INSET + ROW_PADDING - 2.))
            .pr(px(ROW_INSET + 2.))
            .flex()
            .items_center()
            .gap_0p5()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon(IconName::Claude, ui.claude).size(px(15.)))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(theme::TEXT_MD))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(ui.foreground)
                            .child(tr("Claude")),
                    )
                    .children(region),
            )
            .when(ready, |header| {
                header.child(
                    ui::icon_button("claude-console-new", IconName::Plus, ui)
                        .tooltip(ui::tooltip(tr("New Session"), new_keys))
                        .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                            cx.emit(ClaudeConsoleEvent::NewSession)
                        })),
                )
            })
            .child(
                ui::icon_button("claude-console-more", IconName::More, ui)
                    .tooltip(ui::tooltip(tr("More"), None))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_more_menu(under(event.position), window, cx)
                        }),
                    ),
            )
    }

    /// Over the list while Claude is blocked: why, and Check Again; asked again after a block —
    /// that Flux is checking.
    fn render_region_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ui = Theme::ui(cx);
        let verdict = region_guard::verdict(cx);
        let banner = div()
            .flex_none()
            .mx(px(ROW_INSET))
            .mt_0p5()
            .mb_1()
            .py(px(10.))
            .px(px(ROW_PADDING))
            .flex()
            .gap(px(10.))
            .rounded(px(RADIUS_MD))
            .border_1()
            .text_size(px(theme::TEXT_SM));
        if verdict == Verdict::Checking && self.region_blocked {
            return Some(
                banner
                    .items_center()
                    .border_color(ui.island_border)
                    .bg(ui.input_background)
                    .text_color(ui.text_muted)
                    .child(spinner_dot(ui))
                    .child(tr("Checking the country…"))
                    .into_any_element(),
            );
        }
        let refusal = verdict.refusal()?;
        let mark = region_status::mark(&verdict, 15., "claude-console-banner-pulse", ui);
        Some(
            banner
                .border_color(UiColors::tint(ui.error, 0.32))
                .bg(UiColors::tint(ui.error, 0.09))
                .child(
                    div()
                        .flex_none()
                        .h(px(18.))
                        .flex()
                        .items_center()
                        .children(mark),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap(px(10.))
                        .child(
                            div()
                                .line_height(px(18.))
                                .text_color(ui.foreground)
                                .child(refusal.message()),
                        )
                        .child(
                            ui::text_button(
                                "claude-console-check-country",
                                tr("Check Again"),
                                false,
                                ui,
                            )
                            .on_click(|_, _, cx| region_guard::check_again(cx)),
                        ),
                )
                .into_any_element(),
        )
    }

    /// A group's caption: "Now", "Recent", "This Week".
    fn caption(&self, label: SharedString, ui: UiColors) -> gpui::Div {
        div()
            .flex_none()
            .pt(px(18.))
            .pb(px(8.))
            .px(px(ROW_INSET + ROW_PADDING - 2.))
            .text_size(px(theme::TEXT_SM))
            .text_color(ui.dim)
            .child(label)
    }

    /// The highlight of a row: selected (focused or not), else hovered.
    fn row_fill(&self, key: &RowKey, focused: bool, ui: UiColors) -> Option<gpui::Hsla> {
        if self.selected.as_ref() == Some(key) {
            Some(if focused {
                ui.list_selected
            } else {
                ui.list_selected_inactive
            })
        } else if self.hovered.as_ref() == Some(key) {
            Some(ui.hover)
        } else {
            None
        }
    }

    /// The common part of every row: selection, hover, click (open), right click (menu).
    fn row_shell(
        &self,
        index: usize,
        row: &Row,
        focused: bool,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let key = row.key.clone();
        let fill = self.row_fill(&key, focused, ui);
        let item = row.item.clone();
        div()
            .id(("claude-console-row", index))
            .mx(px(ROW_INSET))
            .px(px(ROW_PADDING))
            .rounded(px(RADIUS_MD))
            .cursor_pointer()
            .when_some(fill, |row, fill| row.bg(fill))
            .on_hover(cx.listener({
                let key = key.clone();
                move |this, hovered: &bool, _, cx| {
                    if *hovered {
                        this.hovered = Some(key.clone());
                    } else if this.hovered.as_ref() == Some(&key) {
                        this.hovered = None;
                    }
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener({
                    let key = key.clone();
                    move |this, _: &MouseDownEvent, window, cx| {
                        this.selected = Some(key.clone());
                        window.focus(&this.focus_handle);
                        cx.notify();
                    }
                }),
            )
            // A click shows the tab and keeps the keyboard here (as the project tree); a double
            // click goes into the chat.
            .on_click(cx.listener(move |_, event: &ClickEvent, _, cx| {
                cx.emit(if event.click_count() >= 2 {
                    ClaudeConsoleEvent::Open(item.clone())
                } else {
                    ClaudeConsoleEvent::Show(item.clone())
                })
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.selected = Some(key.clone());
                    this.open_row_menu(event.position, window, cx);
                }),
            )
    }

    /// A session of "Now": its dot, its title, what it does; quick actions when hovered or selected.
    fn render_session(
        &self,
        index: usize,
        row: &Row,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let now = SystemTime::now();
        let active =
            self.selected.as_ref() == Some(&row.key) || self.hovered.as_ref() == Some(&row.key);
        let shell = self
            .row_shell(index, row, focused, ui, cx)
            .py(px(10.))
            .flex()
            .gap(px(STATUS_GAP));
        let status = div()
            .flex_none()
            .w(px(STATUS_WIDTH))
            .h(px(20.))
            .flex()
            .items_center()
            .justify_center();
        let title = |text: SharedString, color| {
            div()
                .truncate()
                .text_size(px(theme::TEXT_MD + 0.5))
                .text_color(color)
                .child(text)
        };
        match &row.item {
            SessionRef::Saved(saved) => shell
                .child(status.child(hollow_dot(ui)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .child(title(saved.title.clone().into(), ui.text_muted))
                        .child(
                            div()
                                .truncate()
                                .text_size(px(theme::TEXT_SM))
                                .text_color(ui.text_muted)
                                .child(worked_ago(saved.modified, now)),
                        ),
                )
                .into_any_element(),
            SessionRef::Live(session) => {
                let store = self.store.read(cx);
                let finished = store.finished_at(session.entity_id());
                let unseen = store.is_unseen(session.entity_id());
                let last_worked = store.last_worked(session, cx);
                let model = session.read(cx).model();
                let glance = claude_status::glance(model);
                let mut phrase = claude_status::phrase(model, finished.filter(|_| unseen));
                // Idle and seen: when it last worked, as an unloaded one reads.
                if phrase.text.is_empty() {
                    phrase.text = last_worked
                        .map(|at| worked_ago(at, now))
                        .unwrap_or_default();
                }
                let done = glance.state == GlanceState::Idle && unseen;
                let has_changes = !model.changed_files.is_empty();
                // A question or a plan waits: it stays open under the session, not only under the
                // mouse (the author missed it while working in another session).
                let waiting = !model.pending.is_empty();
                let quick = self.quick_question(session, cx);
                let plan = model
                    .pending
                    .first()
                    .is_some_and(|pending| matches!(pending.kind, PendingKind::Plan { .. }));
                let request = model.pending.first().map(|pending| pending.id.clone());
                let tone = match phrase.tone {
                    Tone::Accent => ui.accent_text,
                    Tone::Warning => ui.warning,
                    Tone::Success => ui.success,
                    Tone::Error => ui.error,
                    Tone::Muted => ui.text_muted,
                };
                let title_text = session.read(cx).title();
                let id = session.entity_id();
                let mut main = div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(title(title_text, ui.foreground));
                // A question reads whole (up to three lines); other phrases are one line.
                main = main.child(if let Some(quick) = &quick {
                    // The question on the card's current step, "2/3" before it when there are
                    // several.
                    let text = trf("Asks: {0}", &[&quick.current().question]);
                    let text = if quick.questions.len() > 1 {
                        format!("{}/{} · {text}", quick.step + 1, quick.questions.len())
                    } else {
                        text
                    };
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(tone)
                        .line_clamp(3)
                        .child(text)
                } else {
                    div()
                        .truncate()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(tone)
                        .child(phrase.text)
                });
                let mut extra: Option<AnyElement> = None;
                if let Some(quick) = quick {
                    extra = Some(
                        self.render_options(session, &quick, ui, cx)
                            .into_any_element(),
                    );
                } else if plan {
                    let approve = session.clone();
                    let open = session.clone();
                    extra = Some(
                        div()
                            .mt_2()
                            .flex()
                            .gap_2()
                            .child(
                                ui::primary_button(
                                    ("claude-console-approve", index),
                                    tr("Approve"),
                                    true,
                                    ui,
                                )
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.approve_plan(&approve, cx)
                                    },
                                )),
                            )
                            .child(
                                ui::text_button(
                                    ("claude-console-plan", index),
                                    tr("Open the Plan"),
                                    false,
                                    ui,
                                )
                                .on_click(cx.listener(
                                    move |_, _: &ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        cx.emit(ClaudeConsoleEvent::Open(SessionRef::Live(
                                            open.clone(),
                                        )))
                                    },
                                )),
                            )
                            .into_any_element(),
                    );
                }
                // It slides open when the session starts to wait: once per question (the
                // animation's id), then it stays still — no frames asked for after.
                if let (Some(extra), Some(request)) = (extra, request) {
                    let content = div().overflow_hidden().child(extra);
                    main = main.child(if crate::motion::enabled() {
                        let curve = crate::motion::SMOOTH;
                        content
                            .with_animation(
                                SharedString::from(format!("claude-console-open-{request}")),
                                Animation::new(curve.duration())
                                    .with_easing(move |t| curve.ease(t)),
                                |content, delta| {
                                    content
                                        .max_h(px(OPEN_HEIGHT * delta))
                                        .opacity(delta.min(1.))
                                },
                            )
                            .into_any_element()
                    } else {
                        content.into_any_element()
                    });
                }
                let actions = (active && done && has_changes).then(|| {
                    let review = session.clone();
                    let rollback = session.clone();
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_0p5()
                        .child(
                            ui::icon_button(("claude-console-review", index), IconName::Diff, ui)
                                .tooltip(ui::tooltip(tr("Review"), None))
                                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(ClaudeConsoleEvent::Review(review.clone()))
                                })),
                        )
                        .child(
                            ui::icon_button(
                                ("claude-console-rollback", index),
                                IconName::Rollback,
                                ui,
                            )
                            .tooltip(ui::tooltip(tr("Rollback…"), None))
                            .on_click(cx.listener(
                                move |_, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(ClaudeConsoleEvent::Rollback(rollback.clone()))
                                },
                            )),
                        )
                });
                // No tooltip on a waiting session: its question is in the row, and a tooltip would
                // cover the options.
                shell
                    .when(!waiting, |shell| {
                        shell.tooltip(claude_status::tooltip(session.clone(), finished, false))
                    })
                    .child(status.child(claude_status::dot(
                        &glance,
                        done,
                        ("claude-console-dot", id.as_u64()),
                        ui,
                    )))
                    .child(main)
                    .children(actions)
                    .into_any_element()
            }
        }
    }

    /// A waiting question's options under its session (the card's current step); the last one
    /// opens the chat. Several answers toggle with a check and go on with Next / Send.
    fn render_options(
        &self,
        session: &Entity<ClaudeSession>,
        quick: &QuickQuestion,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let question = quick.current();
        let multi = question.multi_select;
        let option = |index: usize, label: SharedString, picked: Option<bool>| {
            let session = session.clone();
            let muted = picked.is_none();
            let mark = match picked {
                Some(true) => icon(IconName::Check, ui.accent_text)
                    .size(px(12.))
                    .into_any_element(),
                _ => div()
                    .font_family(theme::code_font())
                    .text_size(px(theme::TEXT_XS))
                    .text_color(ui.dim)
                    .child((index + 1).to_string())
                    .into_any_element(),
            };
            div()
                .id(("claude-console-option", index))
                .px(px(10.))
                .py(px(6.))
                .rounded(px(RADIUS_SM))
                .bg(if picked == Some(true) {
                    ui.accent_soft
                } else {
                    UiColors::tint(ui.foreground, 0.04)
                })
                .hover(move |style| style.bg(ui.hover))
                .flex()
                .gap(px(10.))
                .text_size(px(theme::TEXT_SM + 0.5))
                .text_color(if muted { ui.text_muted } else { ui.foreground })
                .child(
                    div()
                        .flex_none()
                        .w(px(12.))
                        .h(px(18.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(mark),
                )
                .child(div().flex_1().min_w_0().child(label))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.answer(&session, index, cx)
                }))
        };
        let shown: Vec<SharedString> = question
            .options
            .iter()
            .take(MAX_OPTIONS - 1)
            .map(|option| option.label.clone().into())
            .collect();
        let count = shown.len();
        let last = quick.step + 1 == quick.questions.len();
        let next = multi.then(|| {
            let session = session.clone();
            div().flex().justify_end().child(
                ui::primary_button(
                    ("claude-console-next", quick.step),
                    if last { tr("Send") } else { tr("Next") },
                    !quick.chosen.is_empty(),
                    ui,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.next_question(&session, cx)
                })),
            )
        });
        div()
            .mt_2()
            .flex()
            .flex_col()
            .gap(px(5.))
            .children(
                shown.into_iter().enumerate().map(|(index, label)| {
                    option(index, label, Some(quick.chosen.contains(&index)))
                }),
            )
            .child(option(count, tr("Answer in the Chat…").into(), None))
            .children(next)
    }

    /// A row of the history: the title, when it was last active; archived ones marked.
    fn render_saved(
        &self,
        index: usize,
        row: &Row,
        focused: bool,
        now: i64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let SessionRef::Saved(saved) = &row.item else {
            return div().into_any_element();
        };
        let archived = self.store.read(cx).is_archived(&saved.id);
        let active =
            self.selected.as_ref() == Some(&row.key) || self.hovered.as_ref() == Some(&row.key);
        let restore = (archived && active).then(|| {
            let saved = saved.clone();
            ui::text_button(("claude-console-restore", index), tr("Restore"), false, ui).on_click(
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.archive(SessionRef::Saved(saved.clone()), cx)
                }),
            )
        });
        self.row_shell(index, row, focused, ui, cx)
            .h(px(HISTORY_ROW_HEIGHT))
            .pl(px(ROW_PADDING + STATUS_WIDTH + STATUS_GAP))
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::TEXT_SM + 0.5))
                    .text_color(if archived { ui.dim } else { ui.text_muted })
                    .child(saved.title.clone()),
            )
            .when(archived && restore.is_none(), |row| {
                row.child(icon(IconName::Archive, ui.dim).size(px(13.)))
            })
            .children(restore)
            .when(!(archived && active), |row| {
                row.child(
                    div()
                        .flex_none()
                        .text_size(px(theme::TEXT_XS + 0.5))
                        .text_color(ui.dim)
                        .child(short_when(now, claude_history::seconds(saved.modified))),
                )
            })
            .into_any_element()
    }

    /// Whether a history group is open.
    fn group_open(&self, section: Section) -> bool {
        match section {
            Section::Recent => self.recent_open,
            Section::Week => self.week_open,
            Section::All => self.all_open,
            Section::Now => true,
        }
    }

    /// A history group's caption: a chevron, the name, the count; a click opens or closes it.
    fn render_group_caption(
        &self,
        section: Section,
        label: SharedString,
        count: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let open = self.group_open(section);
        let id: &'static str = match section {
            Section::Recent => "claude-console-recent",
            Section::Week => "claude-console-week",
            _ => "claude-console-all",
        };
        div()
            .id(id)
            .flex_none()
            .mt(px(10.))
            .mx(px(ROW_INSET))
            .px(px(ROW_PADDING - 2.))
            .h(px(28.))
            .rounded(px(RADIUS_SM))
            .flex()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .hover(move |style| style.bg(ui.hover))
            .text_size(px(theme::TEXT_SM))
            .text_color(ui.dim)
            .child(
                icon(
                    if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    },
                    ui.dim,
                )
                .size(px(12.)),
            )
            .child(label)
            .children(
                count.map(|count| div().text_color(ui.text_disabled).child(count.to_string())),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                match section {
                    Section::Recent => this.recent_open = !this.recent_open,
                    Section::Week => this.week_open = !this.week_open,
                    _ => {
                        this.all_open = !this.all_open;
                        if !this.all_open {
                            this.filter.update(cx, |filter, cx| filter.set_text("", cx));
                            window.focus(&this.focus_handle);
                        }
                    }
                }
                cx.notify();
            }))
    }

    /// "Show archived" under the filter of "All".
    fn render_show_archived(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let state = ui::CheckState::from_bool(self.show_archived);
        div()
            .id("claude-console-show-archived")
            .flex_none()
            .mx(px(ROW_INSET))
            .px(px(ROW_PADDING - 2.))
            .h(px(28.))
            .rounded(px(RADIUS_SM))
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .hover(move |style| style.bg(ui.hover))
            .text_size(px(theme::TEXT_SM))
            .text_color(ui.text_muted)
            .child(ui::checkbox("claude-console-show-archived-box", state, ui))
            .child(tr("Show archived"))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.show_archived = !this.show_archived;
                cx.notify();
            }))
    }

    fn render_limits(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ui = Theme::ui(cx);
        if !crate::settings::claude(cx).show_limits {
            return None;
        }
        let limits = self.store.read(cx).limits()?.clone();
        let now = SystemTime::now();
        let rows: Vec<AnyElement> = [
            (tr("5 h"), limits.five_hour),
            (tr("7 days"), limits.seven_day),
        ]
        .into_iter()
        .filter_map(|(label, window)| {
            let window = window?;
            let color = match claude::limit_level(window.utilization) {
                claude::LimitLevel::Normal => ui.accent,
                claude::LimitLevel::Warning => ui.warning,
                claude::LimitLevel::Critical => ui.error,
            };
            let share = window.utilization.clamp(0., 1.);
            Some(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_none().w(px(48.)).child(label))
                    .child(
                        div()
                            .flex_1()
                            .h(px(3.))
                            .rounded(px(2.))
                            .bg(UiColors::tint(ui.foreground, 0.07))
                            .child(
                                div()
                                    .h_full()
                                    .rounded(px(2.))
                                    .bg(color)
                                    .w(gpui::relative(share)),
                            ),
                    )
                    .child(div().flex_none().text_color(ui.dim).child(trf(
                        "{0}% · {1}",
                        &[
                            &claude::percent(window.utilization),
                            &resets_at(seconds(now), seconds(window.resets_at)),
                        ],
                    )))
                    .into_any_element(),
            )
        })
        .collect();
        if rows.is_empty() {
            return None;
        }
        Some(
            div()
                .flex_none()
                .px(px(ROW_INSET + ROW_PADDING))
                .pt(px(14.))
                .pb(px(16.))
                .flex()
                .flex_col()
                .gap(px(10.))
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.text_muted)
                .children(rows)
                .into_any_element(),
        )
    }

    fn render_list(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let focused = self.focus_handle.contains_focused(window, cx);
        let rows = self.rows(cx);
        let saved_count = self
            .store
            .read(cx)
            .saved_sessions()
            .map(|saved| saved.len());
        let now = claude_history::now_seconds();
        let mut list = div()
            .id("claude-console-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .pb_3()
            .flex()
            .flex_col();
        // "Now": its sessions, apart from each other.
        list = list.child(self.caption(tr("Now").into(), ui).pt(px(6.)));
        let now_rows: Vec<(usize, &Row)> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.key.section == Section::Now)
            .collect();
        if now_rows.is_empty() {
            list = list.child(
                div()
                    .mx(px(ROW_INSET))
                    .px(px(ROW_PADDING))
                    .py(px(6.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(div().flex_1().child(tr("No sessions")))
                    .child(
                        ui::text_button("claude-console-start", tr("New Session"), false, ui)
                            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                                cx.emit(ClaudeConsoleEvent::NewSession)
                            })),
                    ),
            );
        }
        let mut sessions = div().flex().flex_col().gap(px(SESSION_GAP));
        for (index, row) in now_rows {
            sessions = sessions.child(self.render_session(index, row, focused, cx));
        }
        list = list.child(sessions);
        // The groups' sizes, also while closed (their rows aren't listed then).
        let (recent_count, week_count, all_count) = {
            let store = self.store.read(cx);
            let in_now: HashSet<String> = store
                .console(cx)
                .iter()
                .filter_map(|item| match item {
                    ConsoleItem::Live(session) => session.read(cx).session_id().map(str::to_string),
                    ConsoleItem::Unloaded(saved) => Some(saved.id.clone()),
                })
                .collect();
            let saved = store.saved_sessions().unwrap_or_default();
            let (recent, week) = group_history(
                saved,
                &in_now,
                |id| store.is_archived(id),
                SystemTime::now(),
            );
            let all = saved
                .iter()
                .filter(|saved| self.show_archived || !store.is_archived(&saved.id))
                .count();
            (recent.len(), week.len(), saved_count.map(|_| all))
        };
        for (section, label, count) in [
            (Section::Recent, tr("Recent"), recent_count),
            (Section::Week, tr("This Week"), week_count),
        ] {
            if count == 0 {
                continue;
            }
            list = list.child(self.render_group_caption(section, label.into(), Some(count), cx));
            for (index, row) in rows
                .iter()
                .enumerate()
                .filter(|(_, row)| row.key.section == section)
            {
                list = list.child(self.render_saved(index, row, focused, now, cx));
            }
        }
        list = list.child(self.render_group_caption(Section::All, tr("All").into(), all_count, cx));
        if self.all_open {
            list = list.child(
                div()
                    .mx(px(ROW_INSET + 4.))
                    .mt_1()
                    .mb_1p5()
                    .child(self.filter.clone()),
            );
            list = list.child(self.render_show_archived(cx));
            let all: Vec<(usize, &Row)> = rows
                .iter()
                .enumerate()
                .filter(|(_, row)| row.key.section == Section::All)
                .collect();
            if all.is_empty() {
                list = list.child(
                    div()
                        .px(px(ROW_INSET + ROW_PADDING + STATUS_WIDTH + STATUS_GAP))
                        .py_1()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .child(match saved_count {
                            None => tr("Reading the sessions…"),
                            Some(_) => tr("Nothing found"),
                        }),
                );
            }
            for (index, row) in all {
                list = list.child(self.render_saved(index, row, focused, now, cx));
            }
        }
        list.into_any_element()
    }

    /// While `claude` isn't ready: what to do about it.
    fn render_onboarding(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let store = self.store.read(cx);
        let column = div()
            .w_full()
            .max_w(px(ONBOARDING_WIDTH))
            .flex()
            .flex_col()
            .items_center()
            .gap_3()
            .text_center();
        let tile = |name: IconName, color: gpui::Hsla| {
            div()
                .size(px(44.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(ui::RADIUS_LG))
                .bg(UiColors::tint(color, 0.14))
                .border_1()
                .border_color(UiColors::tint(color, 0.3))
                .child(icon(name, color).size(px(22.)))
        };
        let heading = |text: SharedString| {
            div()
                .text_size(px(theme::TEXT_LG))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(ui.foreground)
                .child(text)
        };
        let note = |text: SharedString| {
            div()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.text_muted)
                .child(text)
        };
        let action_button =
            |id: &'static str, label: &'static str, primary: bool, action: Box<dyn Action>| {
                let button = if primary {
                    ui::primary_button(id, label, true, ui)
                } else {
                    ui::text_button(id, label, false, ui)
                };
                button
                    .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
            };
        let content = match store.cli() {
            CliState::Checking => column
                .child(spinner_dot(ui))
                .child(note(tr("Looking for Claude Code…").into())),
            CliState::Missing => column
                .child(tile(IconName::Claude, ui.claude))
                .child(heading(tr("Claude Code isn't installed").into()))
                .child(note(
                    tr("Flux runs the Claude Code CLI with your Claude subscription. Install it, then sign in.").into(),
                ))
                .child(
                    div()
                        .w_full()
                        .mt_1()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(claude::INSTALL_COMMANDS.iter().enumerate().map(
                            |(index, (title, command))| install_option(index, tr(title), command, ui),
                        )),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(action_button(
                            "claude-check-again",
                            tr("Check Again"),
                            true,
                            Box::new(claude::CheckAgain),
                        ))
                        .child(action_button(
                            "claude-open-settings",
                            tr("Set Path…"),
                            false,
                            Box::new(claude::OpenSettings),
                        )),
                ),
            CliState::SignedOut { .. } => column
                .child(tile(IconName::Claude, ui.claude))
                .child(heading(tr("Sign in to Claude").into()))
                .child(note(
                    tr("Claude Code works with your Claude account: a Pro, Max, Team or Enterprise subscription. Signing in opens the browser.").into(),
                ))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(action_button(
                            "claude-sign-in",
                            tr("Sign In"),
                            true,
                            Box::new(claude::SignIn),
                        ))
                        .child(action_button(
                            "claude-check-again",
                            tr("Check Again"),
                            false,
                            Box::new(claude::CheckAgain),
                        )),
                ),
            CliState::Failed(error) => column
                .child(tile(IconName::Warning, ui.warning))
                .child(heading(tr("Claude Code doesn't respond").into()))
                .child(
                    div()
                        .w_full()
                        .p_2()
                        .rounded(px(RADIUS_SM))
                        .bg(ui.input_background)
                        .border_1()
                        .border_color(ui.input_border)
                        .text_left()
                        .font_family(theme::code_font())
                        .text_size(px(theme::TEXT_XS))
                        .text_color(ui.text_muted)
                        .child(error.clone()),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(action_button(
                            "claude-check-again",
                            tr("Check Again"),
                            true,
                            Box::new(claude::CheckAgain),
                        ))
                        .child(action_button(
                            "claude-open-settings",
                            tr("Settings…"),
                            false,
                            Box::new(claude::OpenSettings),
                        )),
                ),
            // Ready: the list draws instead.
            CliState::Ready { .. } => column,
        };
        div()
            .id("claude-onboarding")
            .flex_1()
            .min_h_0()
            .px_5()
            .py_6()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .child(content)
            .into_any_element()
    }
}

/// A question the console can answer: one question with a single choice.
#[derive(Debug, Clone, PartialEq)]
struct QuickQuestion {
    request: String,
    questions: Vec<Question>,
    /// The question shown: the first one not answered yet.
    step: usize,
    /// The options picked on it.
    chosen: Vec<usize>,
}

impl QuickQuestion {
    fn current(&self) -> &Question {
        &self.questions[self.step]
    }
}

/// A question card's answers in the console so far.
struct Picks {
    step: usize,
    chosen: Vec<Vec<usize>>,
}

impl Picks {
    fn new(questions: usize) -> Self {
        Self {
            step: 0,
            chosen: vec![Vec::new(); questions],
        }
    }
}

/// The history in the console's groups: "Recent" — active in the last 12 hours, "This Week" —
/// before that within 7 days; without the sessions of "Now" and the archived ones. `saved` is the
/// latest first, and so are the groups.
fn group_history<'a>(
    saved: &'a [SavedSession],
    in_now: &HashSet<String>,
    archived: impl Fn(&str) -> bool,
    now: SystemTime,
) -> (Vec<&'a SavedSession>, Vec<&'a SavedSession>) {
    let mut recent = Vec::new();
    let mut week = Vec::new();
    for session in saved {
        if in_now.contains(&session.id) || archived(&session.id) {
            continue;
        }
        let age = now.duration_since(session.modified).unwrap_or_default();
        if age < RECENT {
            recent.push(session);
        } else if age < WEEK {
            week.push(session);
        }
    }
    (recent, week)
}

/// Whether a saved session matches the filter of "All": every word in its title or first prompt.
fn matches_filter(saved: &SavedSession, query: &str) -> bool {
    let haystack = format!(
        "{} {}",
        saved.title.to_lowercase(),
        saved.prompt.as_deref().unwrap_or_default().to_lowercase()
    );
    query
        .split_whitespace()
        .all(|word| haystack.contains(&word.to_lowercase()))
}

/// When a saved session was last active, short for the console's narrow rows: "12 min", "14:05"
/// today, "Yesterday", a weekday within the week, else the date as the history gives it.
fn short_when(now: i64, time: i64) -> String {
    let ago = (now - time).max(0);
    if ago < 60 {
        return tr("just now").to_string();
    }
    if ago < 3600 {
        return trf("{0} min", &[&(ago / 60)]);
    }
    let local = time + local_offset(time);
    let today = (now + local_offset(now)).div_euclid(86_400);
    let day = local.div_euclid(86_400);
    let minutes = local.rem_euclid(86_400) / 60;
    match today - day {
        0 => format!("{:02}:{:02}", minutes / 60, minutes % 60),
        1 => tr("Yesterday").to_string(),
        2..7 => {
            // 1970-01-01 was a Thursday.
            const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
            tr(WEEKDAYS[(day + 3).rem_euclid(7) as usize]).to_string()
        }
        _ => claude_history::when(now, time),
    }
}

/// When a limit resets, short enough to leave its bar room in the narrow island: "until 17:00"
/// today, "until Mon" later in the week (the mockup approved by the author).
fn resets_at(now: i64, at: i64) -> String {
    let local = at + local_offset(at);
    let today = (now + local_offset(now)).div_euclid(86_400);
    let day = local.div_euclid(86_400);
    if day <= today {
        let minutes = local.rem_euclid(86_400) / 60;
        return trf(
            "until {0}",
            &[&format!("{:02}:{:02}", minutes / 60, minutes % 60)],
        );
    }
    // 1970-01-01 was a Thursday.
    const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    trf(
        "until {0}",
        &[&tr(WEEKDAYS[(day + 3).rem_euclid(7) as usize])],
    )
}

fn seconds(time: SystemTime) -> i64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// When a session last worked, for the second line of an idle one: "12 min ago", "3 h ago",
/// "Yesterday", a weekday, the date.
fn worked_ago(at: SystemTime, now: SystemTime) -> String {
    let (now, at) = (seconds(now), seconds(at));
    let ago = (now - at).max(0);
    if ago < 60 {
        return tr("just now").to_string();
    }
    if ago < 3600 {
        return trf("{0} min ago", &[&(ago / 60)]);
    }
    if ago < 12 * 3600 {
        return trf("{0} h ago", &[&(ago / 3600)]);
    }
    short_when(now, at)
}

/// Where a popup opened from a header button goes: under the button, from the click.
fn under(position: Point<Pixels>) -> Point<Pixels> {
    point(position.x - px(16.), position.y + px(16.))
}

/// An unloaded session: a hollow, dim ring.
fn hollow_dot(ui: UiColors) -> impl IntoElement {
    div()
        .flex_none()
        .size(px(7.))
        .rounded(px(4.))
        .border_1()
        .border_color(ui.dim)
}

/// An install option: its name, the command (code font), and "Type in Terminal".
fn install_option(
    index: usize,
    title: &'static str,
    command: &'static str,
    ui: UiColors,
) -> impl IntoElement {
    div()
        .w_full()
        .p_2p5()
        .flex()
        .flex_col()
        .gap_1p5()
        .rounded(px(RADIUS_MD))
        .border_1()
        .border_color(ui.island_border)
        .text_left()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                // A long name wraps; the button keeps its size.
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(theme::TEXT_SM))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(ui.foreground)
                        .child(title),
                )
                .child(
                    ui::text_button(("claude-install", index), tr("Type in Terminal"), false, ui)
                        .on_click(move |_, window, cx| {
                            window.dispatch_action(
                                Box::new(claude::TypeInTerminal(command.to_string())),
                                cx,
                            )
                        }),
                ),
        )
        .child(
            div()
                .px_2()
                .py_1()
                .rounded(px(RADIUS_SM))
                .bg(ui.input_background)
                .font_family(theme::code_font())
                .text_size(px(theme::TEXT_XS))
                .text_color(ui.text_muted)
                .child(command),
        )
}

/// A pulsing dot while `claude` is looked for.
fn spinner_dot(ui: UiColors) -> impl IntoElement {
    div()
        .size(px(9.))
        .rounded(px(5.))
        .bg(ui.accent)
        .with_animation(
            "claude-checking",
            Animation::new(Duration::from_millis(900)).repeat(),
            |dot, delta| dot.opacity(0.3 + 0.7 * (1. - (2. * delta - 1.).abs())),
        )
}

impl Focusable for ClaudeConsole {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ClaudeConsole {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ready = self.store.read(cx).is_ready();
        let body = if ready {
            self.render_list(window, cx)
        } else {
            self.render_onboarding(cx)
        };
        let limits = ready.then(|| self.render_limits(cx)).flatten();
        let menu = Some(ContextMenu::slot(
            "claude-console-menu",
            self.menu.as_ref().map(|menu| (&menu.menu, menu.position)),
        ));
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::open_selected))
            .on_action(cx.listener(Self::rename_selected))
            .on_action(cx.listener(Self::open_selected_in_terminal))
            .on_action(cx.listener(Self::stop_selected))
            .on_action(cx.listener(Self::close_selected))
            .on_action(cx.listener(Self::archive_selected))
            .on_action(cx.listener(Self::show_menu_action))
            .on_action(cx.listener(Self::focus_filter))
            .on_action(cx.listener(Self::leave))
            .on_action(cx.listener(Self::answer_option))
            .on_action(cx.listener(Self::next_question_action))
            .size_full()
            .flex()
            .flex_col()
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_MD))
            .child(self.render_header(cx))
            .children(self.render_region_banner(cx))
            .child(body)
            .children(limits)
            .children(menu)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn saved(id: &str, hours_ago: u64, now: SystemTime) -> SavedSession {
        SavedSession {
            id: id.to_string(),
            title: format!("Session {id}"),
            prompt: Some(format!("Prompt of {id}")),
            branch: None,
            modified: now - Duration::from_secs(hours_ago * 3600),
            size: 0,
            path: PathBuf::from(format!("/tmp/{id}.jsonl")),
        }
    }

    #[test]
    fn the_history_splits_into_the_last_twelve_hours_and_the_week() {
        let now = SystemTime::now();
        let sessions = vec![
            saved("a", 1, now),
            saved("live", 2, now),
            saved("b", 11, now),
            saved("half", 13, now),
            saved("archived", 30, now),
            saved("c", 25, now),
            saved("d", 24 * 6, now),
            saved("old", 24 * 8, now),
        ];
        let in_now: HashSet<String> = ["live".to_string()].into();
        let (recent, week) = group_history(&sessions, &in_now, |id| id == "archived", now);
        let ids =
            |group: Vec<&SavedSession>| group.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(recent), ["a", "b"]);
        assert_eq!(ids(week), ["half", "c", "d"]);
    }

    #[test]
    fn the_filter_takes_every_word_from_the_title_or_the_prompt() {
        let session = saved("redis", 1, SystemTime::now());
        assert!(matches_filter(&session, ""));
        assert!(matches_filter(&session, "session REDIS"));
        assert!(matches_filter(&session, "prompt"));
        assert!(!matches_filter(&session, "session postgres"));
    }
}
