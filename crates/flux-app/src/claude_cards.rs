//! What a Claude chat asks the user, above the message field — as the prompts of the CLI, in
//! Flux's design: permission cards (Allow / "Allow and don't ask again for …" / Deny with what to do
//! instead), edits (Accept / Open Diff / Reject), Claude's questions with options (AskUserQuestion),
//! the plan of plan mode (Approve / Approve and accept edits / Keep planning); below them, Claude's
//! task list and the background tasks.
//!
//! The cards are stacked oldest first; the newest has the keyboard: ↑/↓ and the number keys pick
//! a row, ↵ takes it, Esc denies (keeps planning), ⌘↵ accepts an edit. A text field (what to do
//! instead, "Other") is outside the rows, so typing in it never picks one.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use flux_claude::session::{Pending, PendingKind, Question, TaskStatus};
use flux_claude::{Answer, Session};
use gpui::{
    Action, AnyElement, App, ClickEvent, Context, Entity, FocusHandle, Focusable, FontWeight,
    HighlightStyle,
    KeyBinding, SharedString, StyledText, Window, actions, div, prelude::*, px,
};
use serde_json::Value;

use crate::claude_chat::{ClaudeChat, ClaudeChatEvent};
use crate::claude_session::ClaudeSession;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::input::TextInput;
use crate::markdown::{self, Block};
use crate::theme::{self, Theme, UiColors};
use crate::ui;

actions!(
    claude_cards,
    [
        /// ↑: the row above.
        SelectPrevious,
        /// ↓: the row below.
        SelectNext,
        /// ↵ (⌘↵ on an edit): the highlighted row.
        Confirm,
        /// Esc: deny the call, decline the question, keep planning.
        Dismiss,
        /// Space: a checkbox of a question with several answers.
        Toggle,
        /// ↵ in a card's text field: deny with it, keep planning with it, answer "Other".
        SubmitField,
        /// Esc in a card's text field: back to the rows.
        CloseField,
    ]
);

/// The number keys: the n-th row (zero-based), as in the CLI.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = claude_cards, no_json)]
pub struct PickRow(pub usize);

/// The rows of the card with the keyboard.
const ROWS_CONTEXT: &str = "ClaudeCardRows";
/// A text field of a card.
const FIELD_CONTEXT: &str = "ClaudeCardField";
/// How many lines of an edit's first block the card shows.
const PREVIEW_LINES: usize = 6;
const PLAN_MAX_HEIGHT: f32 = 280.;
const PREVIEW_MAX_HEIGHT: f32 = 180.;

/// What Claude is told when the user denies without saying what to do instead: the turn stops.
const DENY_AND_STOP: &str =
    "The user rejected this action. Stop and wait for the user to tell you how to proceed.";
const KEEP_PLANNING: &str = "The user doesn't approve the plan yet: keep planning.";
const DECLINED: &str = "The user declined to answer the questions.";

pub fn init(cx: &mut App) {
    let rows = Some(ROWS_CONTEXT);
    let mut bindings = vec![
        KeyBinding::new("up", SelectPrevious, rows),
        KeyBinding::new("down", SelectNext, rows),
        KeyBinding::new("enter", Confirm, rows),
        KeyBinding::new("cmd-enter", Confirm, rows),
        KeyBinding::new("escape", Dismiss, rows),
        KeyBinding::new("space", Toggle, rows),
        KeyBinding::new("enter", SubmitField, Some(FIELD_CONTEXT)),
        KeyBinding::new("escape", CloseField, Some(FIELD_CONTEXT)),
    ];
    for n in 1..=9 {
        bindings.push(KeyBinding::new(&n.to_string(), PickRow(n - 1), rows));
    }
    cx.bind_keys(bindings);
}

/// The cards' state in their chat: which card has the keyboard, the highlighted rows, the open
/// text fields, the chosen options of Claude's questions, the plan's Markdown.
#[derive(Default)]
pub struct CardsState {
    cards: HashMap<String, CardState>,
    /// The pending requests of the last frame: a new one takes the keyboard.
    seen: HashSet<String>,
    /// The card the keys go to (a request id).
    keyboard: Option<String>,
    focus: Option<FocusHandle>,
    tasks_collapsed: bool,
    /// The plans' Markdown by request, highlighted in the background.
    plans: HashMap<String, Arc<Vec<Block>>>,
}

#[derive(Default)]
struct CardState {
    highlighted: usize,
    /// The field for what Claude should do instead (deny, reject, keep planning).
    field: Option<Entity<TextInput>>,
    /// AskUserQuestion: the question on screen, the chosen options of each, their "Other" text.
    step: usize,
    chosen: Vec<Vec<usize>>,
    other: Vec<Option<Entity<TextInput>>>,
}

/// Focus to the newest card, if there is one: the chat calls it when a question arrives and the
/// message field is empty (a field the user is typing in keeps the keyboard).
pub fn take_focus(state: &mut CardsState, model: &Session, window: &mut Window) {
    let Some(newest) = model.pending.last() else {
        return;
    };
    state.keyboard = Some(newest.id.clone());
    if let Some(focus) = &state.focus {
        window.focus(focus);
    }
}

/// Whether a card has the keyboard (the chat's Esc and ↵ shouldn't act then).
pub fn has_focus(state: &CardsState, window: &Window) -> bool {
    state
        .focus
        .as_ref()
        .is_some_and(|focus| focus.is_focused(window))
}

/// The pending questions and the task list; `None` when there is nothing.
pub fn render(
    state: &mut CardsState,
    session: &Entity<ClaudeSession>,
    model: &Session,
    window: &mut Window,
    cx: &mut Context<ClaudeChat>,
) -> Option<AnyElement> {
    let ui = Theme::ui(cx);
    let focus = state.focus.get_or_insert_with(|| cx.focus_handle()).clone();
    sync(state, model, window, cx);

    let cards: Vec<AnyElement> = model
        .pending
        .iter()
        .map(|pending| {
            let keyboard = state.keyboard.as_deref() == Some(pending.id.as_str());
            render_card(state, session, pending, model, keyboard, &focus, ui, window, cx)
        })
        .collect();
    let tasks = render_tasks(state, model, ui, cx);
    let background = render_background(session, model, ui);
    if cards.is_empty() && tasks.is_none() && background.is_none() {
        return None;
    }
    Some(
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(ui::GAP))
            .px(px(ui::GAP))
            .pb(px(ui::GAP))
            .children(cards)
            .children(tasks)
            .children(background)
            .into_any_element(),
    )
}

/// Keeps the per-card state in step with the session: new questions get state (and the keyboard,
/// when the cards already had it or nothing has focus), answered ones lose it.
fn sync(
    state: &mut CardsState,
    model: &Session,
    window: &mut Window,
    cx: &mut Context<ClaudeChat>,
) {
    let ids: HashSet<String> = model.pending.iter().map(|pending| pending.id.clone()).collect();
    state.cards.retain(|id, _| ids.contains(id));
    state.plans.retain(|id, _| ids.contains(id));
    let mut arrived = None;
    for pending in &model.pending {
        if !state.seen.contains(&pending.id) {
            arrived = Some(pending.id.clone());
        }
        if !state.cards.contains_key(&pending.id) {
            let card = new_card(pending);
            state.cards.insert(pending.id.clone(), card);
        }
        if let PendingKind::Plan { plan, .. } = &pending.kind
            && !state.plans.contains_key(&pending.id)
        {
            state
                .plans
                .insert(pending.id.clone(), Arc::new(markdown::parse(plan)));
            highlight_plan(pending.id.clone(), plan.clone(), cx);
        }
    }
    let keyboard_gone = state
        .keyboard
        .as_ref()
        .is_none_or(|id| !ids.contains(id));
    if let Some(id) = arrived {
        let cards_had_keys = has_focus(state, window)
            || state
                .keyboard
                .as_ref()
                .and_then(|id| state.cards.get(id))
                .and_then(|card| card.field.as_ref())
                .is_some_and(|field| field.focus_handle(cx).is_focused(window));
        state.keyboard = Some(id);
        if cards_had_keys || window.focused(cx).is_none() {
            take_focus(state, model, window);
        }
    } else if keyboard_gone {
        // The card with the keys was answered: the newest one left takes them (the chat gives the
        // message field the focus back when nothing is left: `has_focus`).
        state.keyboard = model.pending.last().map(|pending| pending.id.clone());
    }
    state.seen = ids;
}

fn new_card(pending: &Pending) -> CardState {
    let rows = rows(pending, Path::new(""));
    let highlighted = if pending.default_to_no {
        rows.iter()
            .position(|row| row.choice == Choice::Deny)
            .unwrap_or(0)
    } else {
        0
    };
    let questions = match &pending.kind {
        PendingKind::Questions(questions) => questions.len(),
        _ => 0,
    };
    CardState {
        highlighted,
        field: None,
        step: 0,
        chosen: vec![Vec::new(); questions],
        other: vec![None; questions],
    }
}

/// Parses the plan's code blocks' colors off the UI thread.
fn highlight_plan(id: String, plan: String, cx: &mut Context<ClaudeChat>) {
    let scopes = markdown::scopes(cx);
    cx.spawn(async move |chat, cx| {
        let blocks = cx
            .background_executor()
            .spawn(async move {
                let mut blocks = markdown::parse(&plan);
                markdown::highlight(&mut blocks, None, &scopes);
                blocks
            })
            .await;
        chat.update(cx, |chat, cx| {
            if let Some(plan) = chat.cards.plans.get_mut(&id) {
                *plan = Arc::new(blocks);
                cx.notify();
            }
        })
        .ok();
    })
    .detach();
}

// --- Rows ---

/// What a row of a card does.
#[derive(Debug, Clone, PartialEq)]
enum Choice {
    /// Allow (accept an edit), remembering the chosen "don't ask again" suggestions.
    Allow { remember: Vec<Value> },
    /// Deny: the field for what to do instead opens.
    Deny,
    Approve { accept_edits: bool },
    /// Keep planning: the field for the feedback opens.
    KeepPlanning,
}

#[derive(Debug, Clone, PartialEq)]
struct Row {
    label: String,
    choice: Choice,
    /// The key that takes it besides its number: "↵", "Esc".
    hint: Option<&'static str>,
}

/// The rows of a permission, an edit or a plan card (Claude's questions have their options).
fn rows(pending: &Pending, cwd: &Path) -> Vec<Row> {
    let edit = matches!(pending.kind, PendingKind::Edit(_));
    match &pending.kind {
        PendingKind::Plan { .. } => vec![
            Row {
                label: tr("Approve").into(),
                choice: Choice::Approve {
                    accept_edits: false,
                },
                hint: Some("↵"),
            },
            Row {
                label: tr("Approve, and accept edits").into(),
                choice: Choice::Approve { accept_edits: true },
                hint: None,
            },
            Row {
                label: tr("Keep Planning…").into(),
                choice: Choice::KeepPlanning,
                hint: Some("Esc"),
            },
        ],
        PendingKind::Questions(_) => Vec::new(),
        PendingKind::Tool | PendingKind::Edit(_) => {
            let mut rows = vec![Row {
                label: if edit { tr("Accept") } else { tr("Allow") }.into(),
                choice: Choice::Allow {
                    remember: Vec::new(),
                },
                hint: Some(if pending.default_to_no { "" } else { "↵" }).filter(|h| !h.is_empty()),
            }];
            if !pending.suppress_always_allow {
                for suggestion in &pending.suggestions {
                    if let Some(label) = suggestion_label(suggestion, cwd, edit) {
                        rows.push(Row {
                            label,
                            choice: Choice::Allow {
                                remember: vec![suggestion.clone()],
                            },
                            hint: None,
                        });
                    }
                }
            }
            rows.push(Row {
                label: if edit { tr("Reject…") } else { tr("Deny…") }.into(),
                choice: Choice::Deny,
                hint: Some("Esc"),
            });
            rows
        }
    }
}

/// A "don't ask again" suggestion of the CLI (a `PermissionUpdate`) as a row: "Allow, and don't
/// ask again for `touch:*` in this project". `None` — a kind the card doesn't offer.
fn suggestion_label(suggestion: &Value, cwd: &Path, edit: bool) -> Option<String> {
    let place = |destination: &str| -> String {
        match destination {
            "session" => tr("in this session").into(),
            "localSettings" => tr("in this project").into(),
            "projectSettings" => tr("in this project, for everyone").into(),
            "userSettings" => tr("in all projects").into(),
            _ => String::new(),
        }
    };
    let destination = suggestion["destination"].as_str().unwrap_or("session");
    let label = match suggestion["type"].as_str()? {
        "setMode" => match suggestion["mode"].as_str()? {
            "acceptEdits" if edit => tr("Accept all edits in this session").to_string(),
            "acceptEdits" => tr("Allow, and accept edits in this session").to_string(),
            "plan" => tr("Allow, and switch to plan mode").to_string(),
            "bypassPermissions" => tr("Allow, and stop asking in this session").to_string(),
            _ => return None,
        },
        "addRules" if suggestion["behavior"].as_str() == Some("allow") => {
            let rules: Vec<String> = suggestion["rules"]
                .as_array()?
                .iter()
                .filter_map(|rule| rule_label(rule, cwd))
                .collect();
            if rules.is_empty() {
                return None;
            }
            let template = if edit {
                "Accept, and don't ask again for {0} {1}"
            } else {
                "Allow, and don't ask again for {0} {1}"
            };
            trf(template, &[&rules.join(", "), &place(destination)])
                .trim_end()
                .to_string()
        }
        "addDirectories" => {
            let dirs: Vec<String> = suggestion["directories"]
                .as_array()?
                .iter()
                .filter_map(|dir| {
                    Some(match short_path(dir.as_str()?, cwd).as_str() {
                        "." => tr("the project folder").to_string(),
                        path => format!("`{path}`"),
                    })
                })
                .collect();
            if dirs.is_empty() {
                return None;
            }
            trf("Allow, and allow access to {0} {1}", &[&dirs.join(", "), &place(destination)])
                .trim_end()
                .to_string()
        }
        _ => return None,
    };
    Some(label)
}

/// A permission rule as words: "`npm test`", "fetches from example.com", "reading `/tmp/**`".
fn rule_label(rule: &Value, cwd: &Path) -> Option<String> {
    let tool = rule["toolName"].as_str()?;
    let content = rule["ruleContent"].as_str().filter(|content| !content.is_empty());
    Some(match (tool, content) {
        ("Bash", Some(command)) => format!("`{command}`"),
        ("WebFetch", Some(content)) => {
            let domain = content.strip_prefix("domain:").unwrap_or(content);
            trf("fetches from {0}", &[&domain])
        }
        ("WebFetch", None) => tr("web pages").to_string(),
        ("WebSearch", _) => tr("web searches").to_string(),
        ("Read" | "Glob" | "Grep", Some(path)) => {
            trf("reading {0}", &[&format!("`{}`", rule_path(path, cwd))])
        }
        ("Edit" | "Write" | "MultiEdit", Some(path)) => {
            trf("edits of {0}", &[&format!("`{}`", rule_path(path, cwd))])
        }
        (tool, None) => match tool.strip_prefix("mcp__").and_then(|rest| rest.split_once("__")) {
            Some((server, name)) => format!("`{name}` ({server})"),
            None => format!("`{tool}`"),
        },
        (tool, Some(content)) => format!("`{tool}({content})`"),
    })
}

/// A rule's path: `//abs/dir/**` is an absolute path in the CLI's rules.
fn rule_path(path: &str, cwd: &Path) -> String {
    let path = path.strip_prefix('/').filter(|rest| rest.starts_with('/')).unwrap_or(path);
    short_path(path, cwd)
}

/// A path for a label: relative to the project, or with `~` for the home folder.
fn short_path(path: &str, cwd: &Path) -> String {
    if let Ok(relative) = Path::new(path).strip_prefix(cwd)
        && !cwd.as_os_str().is_empty()
    {
        let relative = relative.to_string_lossy();
        return if relative.is_empty() {
            ".".to_string()
        } else {
            relative.into_owned()
        };
    }
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => {
            format!("~{}", &path[home.len()..])
        }
        _ => path.to_string(),
    }
}

// --- Answers ---

/// Claude's questions as answered: the question's text → the chosen labels (", " between several)
/// and the "Other" text.
fn question_answers(
    questions: &[Question],
    chosen: &[Vec<usize>],
    other: &[String],
) -> Vec<(String, String)> {
    questions
        .iter()
        .enumerate()
        .map(|(index, question)| {
            let mut parts: Vec<String> = chosen
                .get(index)
                .into_iter()
                .flatten()
                .filter_map(|option| question.options.get(*option))
                .map(|option| option.label.clone())
                .collect();
            if let Some(text) = other.get(index).map(|text| text.trim()).filter(|t| !t.is_empty())
            {
                parts.push(text.to_string());
            }
            (question.question.clone(), parts.join(", "))
        })
        .collect()
}

/// The deny answer for what the user wrote: their words go to Claude, which goes on; nothing
/// written — the turn stops, as Esc in the CLI.
fn deny(feedback: &str) -> Answer {
    let feedback = feedback.trim();
    if feedback.is_empty() {
        Answer::Deny {
            message: DENY_AND_STOP.to_string(),
            interrupt: true,
        }
    } else {
        Answer::Deny {
            message: format!("The user rejected this action and said: {feedback}"),
            interrupt: false,
        }
    }
}

fn keep_planning(feedback: &str) -> Answer {
    let feedback = feedback.trim();
    if feedback.is_empty() {
        Answer::KeepPlanning(KEEP_PLANNING.to_string())
    } else {
        Answer::KeepPlanning(format!("Keep planning. The user said: {feedback}"))
    }
}

/// Sends the answer of a card.
fn answer(chat: &mut ClaudeChat, id: &str, answer: Answer, cx: &mut Context<ClaudeChat>) {
    let session = chat.session().clone();
    let id = id.to_string();
    session.update(cx, |session, cx| session.answer(&id, answer, cx));
}

/// The pending question of a card, read fresh from the session.
fn pending_of(chat: &ClaudeChat, id: &str, cx: &App) -> Option<Pending> {
    chat.session().read(cx).model().pending(id).cloned()
}

/// What a row does when taken.
fn take_row(
    chat: &mut ClaudeChat,
    id: &str,
    index: usize,
    window: &mut Window,
    cx: &mut Context<ClaudeChat>,
) {
    let Some(pending) = pending_of(chat, id, cx) else {
        return;
    };
    if let PendingKind::Questions(questions) = &pending.kind {
        return pick_option(chat, id, &pending, questions, index, window, cx);
    }
    let cwd = chat.session().read(cx).model().info.cwd.clone();
    let Some(row) = rows(&pending, &cwd).get(index).cloned() else {
        return;
    };
    if let Some(card) = chat.cards.cards.get_mut(id) {
        card.highlighted = index;
    }
    match row.choice {
        Choice::Allow { remember } => answer(chat, id, Answer::Allow { remember }, cx),
        Choice::Approve { accept_edits } => {
            answer(chat, id, Answer::ApprovePlan { accept_edits }, cx)
        }
        Choice::Deny | Choice::KeepPlanning => open_field(chat, id, window, cx),
    }
}

/// Opens (and focuses) the field for what Claude should do instead.
fn open_field(chat: &mut ClaudeChat, id: &str, window: &mut Window, cx: &mut Context<ClaudeChat>) {
    let plan = matches!(
        pending_of(chat, id, cx).map(|pending| pending.kind),
        Some(PendingKind::Plan { .. })
    );
    let Some(card) = chat.cards.cards.get_mut(id) else {
        return;
    };
    let field = card
        .field
        .get_or_insert_with(|| {
            let placeholder = if plan {
                tr("What should change in the plan?")
            } else {
                tr("Tell Claude what to do instead (optional)")
            };
            cx.new(|cx| TextInput::new(placeholder, cx))
        })
        .clone();
    window.focus(&field.focus_handle(cx));
    cx.notify();
}

/// ↵ in a card's text field.
fn submit_field(chat: &mut ClaudeChat, id: &str, window: &mut Window, cx: &mut Context<ClaudeChat>) {
    let Some(pending) = pending_of(chat, id, cx) else {
        return;
    };
    if let PendingKind::Questions(questions) = &pending.kind {
        return next_question(chat, id, &pending, questions, window, cx);
    }
    let text = chat
        .cards
        .cards
        .get(id)
        .and_then(|card| card.field.as_ref())
        .map(|field| field.read(cx).text())
        .unwrap_or_default();
    let reply = match pending.kind {
        PendingKind::Plan { .. } => keep_planning(&text),
        _ => deny(&text),
    };
    answer(chat, id, reply, cx);
}

/// Esc in a card's text field: the field closes, the rows get the keys.
fn close_field(chat: &mut ClaudeChat, id: &str, window: &mut Window, cx: &mut Context<ClaudeChat>) {
    let is_question = matches!(
        pending_of(chat, id, cx).map(|pending| pending.kind),
        Some(PendingKind::Questions(_))
    );
    if let Some(card) = chat.cards.cards.get_mut(id)
        && !is_question
    {
        card.field = None;
    }
    if let Some(focus) = &chat.cards.focus {
        window.focus(focus);
    }
    cx.notify();
}

/// Esc on the rows: deny and stop, decline the questions, keep planning.
fn dismiss(chat: &mut ClaudeChat, id: &str, cx: &mut Context<ClaudeChat>) {
    let Some(pending) = pending_of(chat, id, cx) else {
        return;
    };
    let reply = match pending.kind {
        PendingKind::Plan { .. } => keep_planning(""),
        PendingKind::Questions(_) => Answer::Deny {
            message: DECLINED.to_string(),
            interrupt: true,
        },
        _ => deny(""),
    };
    answer(chat, id, reply, cx);
}

/// A number key or a click on an option of Claude's questions: one answer — taken and the next
/// question; several answers — toggled; "Other" — its field.
fn pick_option(
    chat: &mut ClaudeChat,
    id: &str,
    pending: &Pending,
    questions: &[Question],
    index: usize,
    window: &mut Window,
    cx: &mut Context<ClaudeChat>,
) {
    let Some(card) = chat.cards.cards.get_mut(id) else {
        return;
    };
    let step = card.step.min(questions.len().saturating_sub(1));
    let Some(question) = questions.get(step) else {
        return;
    };
    card.highlighted = index;
    if index == question.options.len() {
        // "Other": the field takes the keys.
        let field = card.other[step]
            .get_or_insert_with(|| cx.new(|cx| TextInput::new(tr("Your answer"), cx)))
            .clone();
        if !question.multi_select {
            card.chosen[step].clear();
        }
        window.focus(&field.focus_handle(cx));
        cx.notify();
        return;
    }
    if index > question.options.len() {
        return;
    }
    if question.multi_select {
        let chosen = &mut card.chosen[step];
        match chosen.iter().position(|option| *option == index) {
            Some(position) => {
                chosen.remove(position);
            }
            None => chosen.push(index),
        }
        cx.notify();
        return;
    }
    card.chosen[step] = vec![index];
    card.other[step] = None;
    next_question(chat, id, pending, questions, window, cx);
}

/// Next (or Submit on the last question): answered when every question has an answer.
fn next_question(
    chat: &mut ClaudeChat,
    id: &str,
    _pending: &Pending,
    questions: &[Question],
    window: &mut Window,
    cx: &mut Context<ClaudeChat>,
) {
    let Some(card) = chat.cards.cards.get_mut(id) else {
        return;
    };
    let other: Vec<String> = card
        .other
        .iter()
        .map(|field| field.as_ref().map(|field| field.read(cx).text()).unwrap_or_default())
        .collect();
    let step = card.step;
    let answered = |step: usize| {
        card.chosen.get(step).is_some_and(|chosen| !chosen.is_empty())
            || other.get(step).is_some_and(|text| !text.trim().is_empty())
    };
    if !answered(step) {
        return;
    }
    if step + 1 < questions.len() {
        card.step = step + 1;
        card.highlighted = 0;
        if let Some(focus) = &chat.cards.focus {
            window.focus(focus);
        }
        cx.notify();
        return;
    }
    let answers = question_answers(questions, &card.chosen, &other);
    answer(chat, id, Answer::Answers(answers), cx);
}

/// The number of rows the keys move through on a card.
fn row_count(chat: &ClaudeChat, pending: &Pending, cx: &App) -> usize {
    match &pending.kind {
        PendingKind::Questions(questions) => {
            let step = chat.cards.cards.get(&pending.id).map_or(0, |card| card.step);
            questions
                .get(step.min(questions.len().saturating_sub(1)))
                .map_or(0, |question| question.options.len() + 1)
        }
        _ => rows(pending, &chat.session().read(cx).model().info.cwd).len(),
    }
}

fn move_highlight(chat: &mut ClaudeChat, step: isize, cx: &mut Context<ClaudeChat>) {
    let Some(id) = chat.cards.keyboard.clone() else {
        return;
    };
    let Some(pending) = pending_of(chat, &id, cx) else {
        return;
    };
    let count = row_count(chat, &pending, cx);
    if count == 0 {
        return;
    }
    if let Some(card) = chat.cards.cards.get_mut(&id) {
        card.highlighted = (card.highlighted as isize + step).rem_euclid(count as isize) as usize;
        cx.notify();
    }
}

/// ↵ on the rows: the highlighted row; on a question with several answers — Next / Submit.
fn confirm(chat: &mut ClaudeChat, window: &mut Window, cx: &mut Context<ClaudeChat>) {
    let Some(id) = chat.cards.keyboard.clone() else {
        return;
    };
    let Some(pending) = pending_of(chat, &id, cx) else {
        return;
    };
    let highlighted = chat.cards.cards.get(&id).map_or(0, |card| card.highlighted);
    if let PendingKind::Questions(questions) = &pending.kind {
        let step = chat.cards.cards.get(&id).map_or(0, |card| card.step);
        let multi = questions.get(step).is_some_and(|question| question.multi_select);
        let has_choice = chat
            .cards
            .cards
            .get(&id)
            .and_then(|card| card.chosen.get(step))
            .is_some_and(|chosen| !chosen.is_empty());
        if multi && has_choice {
            return next_question(chat, &id, &pending, questions, window, cx);
        }
    }
    take_row(chat, &id, highlighted, window, cx);
}

// --- Drawing ---

#[allow(clippy::too_many_arguments)]
fn render_card(
    state: &mut CardsState,
    session: &Entity<ClaudeSession>,
    pending: &Pending,
    model: &Session,
    keyboard: bool,
    focus: &FocusHandle,
    ui: UiColors,
    window: &mut Window,
    cx: &mut Context<ClaudeChat>,
) -> AnyElement {
    let cwd = model.info.cwd.clone();
    let id = pending.id.clone();
    let header = render_header(pending, &cwd, ui, cx);
    let body: Option<AnyElement> = match &pending.kind {
        PendingKind::Tool => render_tool_details(pending, &cwd, ui),
        PendingKind::Edit(proposal) => Some(render_edit_preview(proposal, ui)),
        PendingKind::Plan { .. } => state
            .plans
            .get(&pending.id)
            .map(|blocks| render_plan(&pending.id, blocks, ui, cx)),
        PendingKind::Questions(questions) => {
            let card = state.cards.entry(pending.id.clone()).or_default();
            Some(render_question_text(questions, card.step, ui))
        }
    };
    let card = state.cards.entry(pending.id.clone()).or_default();
    let rows_element = match &pending.kind {
        PendingKind::Questions(questions) => render_question_options(&id, questions, card, ui, cx),
        _ => render_rows(&id, &rows(pending, &cwd), card.highlighted, keyboard, ui, cx),
    };
    // The rows of the card with the keyboard take the keys (a field never sits inside them).
    let rows_element = div()
        .id(SharedString::from(format!("claude-card-rows-{id}")))
        .when(keyboard, |rows| {
            rows.key_context(ROWS_CONTEXT)
                .track_focus(focus)
                .on_action(cx.listener(|chat, _: &SelectPrevious, _, cx| move_highlight(chat, -1, cx)))
                .on_action(cx.listener(|chat, _: &SelectNext, _, cx| move_highlight(chat, 1, cx)))
                .on_action(cx.listener(|chat, _: &Confirm, window, cx| confirm(chat, window, cx)))
                .on_action(cx.listener(|chat, _: &Toggle, window, cx| {
                    if let Some(id) = chat.cards.keyboard.clone() {
                        let highlighted =
                            chat.cards.cards.get(&id).map_or(0, |card| card.highlighted);
                        take_row(chat, &id, highlighted, window, cx);
                    }
                }))
                .on_action(cx.listener(|chat, action: &PickRow, window, cx| {
                    if let Some(id) = chat.cards.keyboard.clone() {
                        take_row(chat, &id, action.0, window, cx);
                    }
                }))
                .on_action(cx.listener(|chat, _: &Dismiss, _, cx| {
                    if let Some(id) = chat.cards.keyboard.clone() {
                        dismiss(chat, &id, cx);
                    }
                }))
        })
        .child(rows_element);
    let card = state.cards.entry(pending.id.clone()).or_default();
    let field = render_field(&id, pending, card, ui, cx);
    let footer = match &pending.kind {
        PendingKind::Questions(questions) => Some(render_question_footer(&id, questions, card, ui, cx)),
        _ => None,
    };
    let field_focused = card
        .field
        .iter()
        .chain(card.other.iter().flatten())
        .any(|field| field.focus_handle(cx).is_focused(window));
    let focused_card = keyboard && (focus.is_focused(window) || field_focused);
    let _ = session;
    div()
        .id(SharedString::from(format!("claude-card-{id}")))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(8.))
        .p(px(10.))
        .rounded(px(ui::RADIUS_LG))
        .bg(ui.elevated)
        .border_1()
        .border_color(if focused_card {
            UiColors::tint(ui.accent, 0.55)
        } else {
            ui.elevated_border
        })
        .shadow(ui::popover_shadow(ui))
        .on_mouse_down(gpui::MouseButton::Left, {
            let id = id.clone();
            cx.listener(move |chat, _, window, cx| {
                chat.cards.keyboard = Some(id.clone());
                // A click on a field keeps the focus there (the field takes it itself).
                let field_focused = chat
                    .cards
                    .cards
                    .get(&id)
                    .and_then(|card| card.field.as_ref())
                    .is_some_and(|field| field.focus_handle(cx).is_focused(window));
                if !field_focused && let Some(focus) = &chat.cards.focus {
                    window.focus(focus);
                }
                cx.notify();
            })
        })
        .child(header)
        .children(body)
        .child(rows_element)
        .children(field)
        .children(footer)
        .into_any_element()
}

/// The card's title row: the tool's icon, what it asks, a "Subagent" chip; Open Diff on edits.
fn render_header(
    pending: &Pending,
    cwd: &Path,
    ui: UiColors,
    cx: &mut Context<ClaudeChat>,
) -> AnyElement {
    let (icon_name, title, detail): (IconName, String, Option<String>) = match &pending.kind {
        PendingKind::Edit(proposal) => {
            let name = file_name(&proposal.path);
            let title = match (pending.tool.as_str(), &proposal.original) {
                ("Write", None) => trf("Create {0}", &[&name]),
                ("Write", Some(_)) => trf("Overwrite {0}", &[&name]),
                _ => trf("Edit {0}", &[&name]),
            };
            let folder = proposal
                .path
                .parent()
                .map(|dir| short_path(&dir.to_string_lossy(), cwd))
                .filter(|dir| dir != ".");
            let icon_name = if proposal.original.is_none() {
                IconName::FilePlus
            } else {
                IconName::Pencil
            };
            (icon_name, title, folder)
        }
        PendingKind::Plan { .. } => (IconName::Plan, tr("Claude's plan").into(), None),
        PendingKind::Questions(questions) => (
            IconName::Question,
            tr("Claude asks").into(),
            (questions.len() > 1).then(|| trf("{0} questions", &[&questions.len()])),
        ),
        PendingKind::Tool => {
            let (icon_name, title) = tool_title(pending);
            (icon_name, title, None)
        }
    };
    let counts = match &pending.kind {
        PendingKind::Edit(proposal) => proposal
            .proposed
            .as_ref()
            .ok()
            .map(|text| line_counts(proposal.original.as_deref().unwrap_or(""), text)),
        _ => None,
    };
    let open_diff = match &pending.kind {
        PendingKind::Edit(proposal) if proposal.proposed.is_ok() => {
            let id = pending.id.clone();
            Some(
                ui::icon_button(
                    SharedString::from(format!("claude-open-diff-{id}")),
                    IconName::Diff,
                    ui,
                )
                .tooltip(ui::tooltip(tr("Open Diff"), None))
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    cx.emit(ClaudeChatEvent::OpenProposal(id.clone()))
                })),
            )
        }
        _ => None,
    };
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .min_w_0()
        .child(icon(icon_name, ui.accent_text).size(px(15.)))
        .child(
            div()
                .min_w_0()
                .truncate()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .children(counts.map(|(added, removed)| {
            div()
                .flex_none()
                .flex()
                .gap(px(4.))
                .text_size(px(theme::TEXT_SM))
                .font_family(theme::code_font())
                .when(added > 0, |row| {
                    row.child(div().text_color(ui.diff_added).child(format!("+{added}")))
                })
                .when(removed > 0, |row| {
                    row.child(div().text_color(ui.diff_deleted).child(format!("−{removed}")))
                })
        }))
        .children(detail.map(|detail| {
            div()
                .min_w_0()
                .truncate()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.dim)
                .child(detail)
        }))
        .when(pending.from_agent, |row| {
            row.child(ui::badge(tr("Subagent"), ui.violet))
        })
        .child(div().flex_1())
        .children(open_diff)
        .into_any_element()
}

/// What a tool card asks: its icon and the question.
fn tool_title(pending: &Pending) -> (IconName, String) {
    let tool = pending.tool.as_str();
    match tool {
        "Bash" => (IconName::Terminal, tr("Run a command?").into()),
        "WebFetch" => (IconName::Globe, tr("Fetch a web page?").into()),
        "WebSearch" => (IconName::Globe, tr("Search the web?").into()),
        "Read" | "Glob" | "Grep" => (IconName::Search, tr("Read outside the project?").into()),
        "NotebookEdit" => (IconName::Pencil, tr("Edit a notebook?").into()),
        "Agent" | "Task" => (IconName::Agent, tr("Start a subagent?").into()),
        _ if tool.starts_with("mcp__") => (
            IconName::Plug,
            trf("Use {0}?", &[&pending.display_name]),
        ),
        _ => (IconName::Command, trf("Use {0}?", &[&pending.display_name])),
    }
}

/// The details of a tool card: the command, the URL, the path; why it asks.
fn render_tool_details(pending: &Pending, cwd: &Path, ui: UiColors) -> Option<AnyElement> {
    let input = &pending.input;
    let text = |key: &str| input[key].as_str().map(str::to_string);
    let (code, note): (Option<String>, Option<String>) = match pending.tool.as_str() {
        "Bash" => (text("command"), text("description")),
        "WebFetch" => (text("url"), text("prompt")),
        "WebSearch" => (text("query"), None),
        "Read" => (text("file_path").map(|path| short_path(&path, cwd)), None),
        "Glob" | "Grep" => (
            text("pattern"),
            text("path").map(|path| short_path(&path, cwd)),
        ),
        "NotebookEdit" => (text("notebook_path").map(|path| short_path(&path, cwd)), None),
        "Agent" | "Task" => (text("description"), text("prompt")),
        _ => {
            let json = serde_json::to_string_pretty(input).unwrap_or_default();
            ((!json.is_empty() && json != "null" && json != "{}").then_some(json), pending.description.clone())
        }
    };
    // Why it asks (a path outside the project…), and the path it is about.
    let reason = pending.reason.clone().filter(|reason| !reason.is_empty());
    let path = pending
        .blocked_path
        .as_ref()
        .map(|path| short_path(path, cwd))
        .filter(|path| code.as_deref() != Some(path.as_str()));
    if code.is_none() && note.is_none() && reason.is_none() && path.is_none() {
        return None;
    }
    Some(
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .children(note.filter(|note| !note.is_empty()).map(|note| {
                div()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.text_muted)
                    .child(note)
            }))
            .children(code.map(|code| code_box(code, ui)))
            .children(reason.map(|reason| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.warning)
                    .child(icon(IconName::Warning, ui.warning).size(px(12.)))
                    .child(reason)
            }))
            .children(path.map(|path| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .min_w_0()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.text_muted)
                    .child(icon(IconName::File, ui.dim).size(px(12.)))
                    .child(div().min_w_0().truncate().child(trf("Path: {0}", &[&path])))
            }))
            .into_any_element(),
    )
}

/// A command or a URL in the code font, in a quiet box; long text scrolls.
fn code_box(code: String, ui: UiColors) -> impl IntoElement {
    div()
        .id("claude-card-code")
        .max_h(px(PREVIEW_MAX_HEIGHT))
        .overflow_y_scroll()
        .px(px(8.))
        .py(px(6.))
        .rounded(px(ui::RADIUS_SM))
        .bg(ui.input_background)
        .border_1()
        .border_color(ui.input_border)
        .font_family(theme::code_font())
        .text_size(px(theme::TEXT_SM))
        .text_color(ui.foreground)
        .child(code)
}

/// An edit's first changed block, as in a unified diff; or why the edit doesn't apply.
fn render_edit_preview(proposal: &flux_claude::edits::EditProposal, ui: UiColors) -> AnyElement {
    let proposed = match &proposal.proposed {
        Ok(text) => text,
        Err(reason) => {
            return div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.warning)
                .child(icon(IconName::Warning, ui.warning).size(px(12.)))
                .child(trf("The edit doesn't apply: {0}", &[&reason_text(reason)]))
                .into_any_element();
        }
    };
    let original = proposal.original.as_deref().unwrap_or("");
    let lines = preview_lines(original, proposed, PREVIEW_LINES);
    let rows = lines.into_iter().map(|(kind, number, text)| {
        let (sign, color, background) = match kind {
            PreviewKind::Removed => ("−", ui.diff_deleted, ui.diff_deleted_bg),
            PreviewKind::Added => ("+", ui.diff_added, ui.diff_added_bg),
            PreviewKind::More => (" ", ui.dim, gpui::transparent_black()),
        };
        div()
            .flex()
            .gap(px(6.))
            .px(px(6.))
            .bg(background)
            .whitespace_nowrap()
            .overflow_hidden()
            .child(
                div()
                    .flex_none()
                    .w(px(28.))
                    .text_color(ui.dim)
                    .child(number.map(|n| n.to_string()).unwrap_or_default()),
            )
            .child(div().flex_none().text_color(color).child(sign))
            .child(div().text_color(if kind == PreviewKind::More { ui.dim } else { ui.foreground }).child(text))
    });
    div()
        .py(px(4.))
        .rounded(px(ui::RADIUS_SM))
        .bg(ui.input_background)
        .border_1()
        .border_color(ui.input_border)
        .overflow_hidden()
        .font_family(theme::code_font())
        .text_size(px(theme::TEXT_SM))
        .children(rows)
        .into_any_element()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewKind {
    Removed,
    Added,
    /// "… N more lines".
    More,
}

/// The first changed block of an edit as removed and added lines (with their 1-based numbers),
/// cut at `max` lines with a "more" line.
fn preview_lines(
    original: &str,
    proposed: &str,
    max: usize,
) -> Vec<(PreviewKind, Option<u32>, String)> {
    let normalize = |text: &str| text.replace("\r\n", "\n");
    let (original, proposed) = (normalize(original), normalize(proposed));
    let hunks = flux_git::diff_lines(&original, &proposed);
    let Some(hunk) = hunks.first() else {
        return Vec::new();
    };
    let old: Vec<&str> = original.lines().collect();
    let new: Vec<&str> = proposed.lines().collect();
    let mut lines = Vec::new();
    for line in hunk.old.clone() {
        lines.push((
            PreviewKind::Removed,
            Some(line + 1),
            old.get(line as usize).copied().unwrap_or("").to_string(),
        ));
    }
    for line in hunk.new.clone() {
        lines.push((
            PreviewKind::Added,
            Some(line + 1),
            new.get(line as usize).copied().unwrap_or("").to_string(),
        ));
    }
    let rest: usize = hunks
        .iter()
        .skip(1)
        .map(|hunk| hunk.old.len() + hunk.new.len())
        .sum();
    let hidden = lines.len().saturating_sub(max) + rest;
    lines.truncate(max);
    if hidden > 0 {
        lines.push((
            PreviewKind::More,
            None,
            trf("… {0} more changed lines", &[&hidden]),
        ));
    }
    lines
}

/// Lines added and removed by an edit.
fn line_counts(original: &str, proposed: &str) -> (usize, usize) {
    let normalize = |text: &str| text.replace("\r\n", "\n");
    flux_git::diff_lines(&normalize(original), &normalize(proposed))
        .iter()
        .fold((0, 0), |(added, removed), hunk| {
            (added + hunk.new.len(), removed + hunk.old.len())
        })
}

fn render_plan(
    id: &str,
    blocks: &Arc<Vec<Block>>,
    ui: UiColors,
    cx: &mut Context<ClaudeChat>,
) -> AnyElement {
    div()
        .id(SharedString::from(format!("claude-plan-{id}")))
        .max_h(px(PLAN_MAX_HEIGHT))
        .overflow_y_scroll()
        .px(px(10.))
        .py(px(8.))
        .rounded(px(ui::RADIUS_SM))
        .bg(ui.input_background)
        .border_1()
        .border_color(ui.input_border)
        .text_size(px(theme::TEXT_MD))
        .child(markdown::render(blocks, ui.foreground, Theme::get(cx)))
        .into_any_element()
}

/// The rows of a permission, edit or plan card: a number, the label (`code` in the code style),
/// its key on the right.
fn render_rows(
    id: &str,
    rows: &[Row],
    highlighted: usize,
    keyboard: bool,
    ui: UiColors,
    cx: &mut Context<ClaudeChat>,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .children(rows.iter().enumerate().map(|(index, row)| {
            let id = id.to_string();
            let selected = keyboard && index == highlighted;
            let primary = index == 0 && !matches!(row.choice, Choice::Deny);
            option_row(
                SharedString::from(format!("claude-row-{id}-{index}")),
                index,
                selected,
                ui,
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .when(primary, |label| label.font_weight(FontWeight::MEDIUM))
                    .child(code_label(&row.label, ui)),
            )
            .children(row.hint.filter(|_| keyboard).map(|hint| ui::keys(hint, ui)))
            .on_click(cx.listener(move |chat, _: &ClickEvent, window, cx| {
                chat.cards.keyboard = Some(id.clone());
                take_row(chat, &id, index, window, cx);
            }))
            .into_any_element()
        }))
        .into_any_element()
}

/// A row of options: its number, highlighted when the keys are on it.
fn option_row(
    id: SharedString,
    index: usize,
    selected: bool,
    ui: UiColors,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(8.))
        .min_h(px(28.))
        .px(px(8.))
        .py(px(3.))
        .rounded(px(ui::RADIUS_SM))
        .cursor_pointer()
        .when(selected, |row| row.bg(ui.list_selected))
        .when(!selected, move |row| row.hover(move |style| style.bg(ui.hover)))
        .child(
            div()
                .flex_none()
                .w(px(14.))
                .text_size(px(theme::TEXT_SM))
                .text_color(if selected { ui.accent_text } else { ui.dim })
                .child(if index < 9 { (index + 1).to_string() } else { String::new() }),
        )
}

/// A label with the parts in backticks styled as code.
fn code_label(label: &str, ui: UiColors) -> StyledText {
    let mut text = String::new();
    let mut highlights = Vec::new();
    for (index, part) in label.split('`').enumerate() {
        let start = text.len();
        text.push_str(part);
        if index % 2 == 1 && !part.is_empty() {
            highlights.push((
                start..text.len(),
                HighlightStyle {
                    color: Some(ui.accent_text),
                    background_color: Some(UiColors::tint(ui.accent, 0.12)),
                    ..Default::default()
                },
            ));
        }
    }
    StyledText::new(text).with_highlights(highlights)
}

/// The question on screen: its chip and its text.
fn render_question_text(questions: &[Question], step: usize, ui: UiColors) -> AnyElement {
    let Some(question) = questions.get(step.min(questions.len().saturating_sub(1))) else {
        return div().into_any_element();
    };
    div()
        .flex()
        .items_start()
        .gap(px(8.))
        .when(!question.header.is_empty(), |row| {
            row.child(ui::badge(question.header.clone(), ui.accent_text))
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .font_weight(FontWeight::MEDIUM)
                .child(question.question.clone()),
        )
        .when(question.multi_select, |row| {
            row.child(
                div()
                    .flex_none()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(tr("Several answers")),
            )
        })
        .into_any_element()
}

/// The options of the question on screen, "Other" last; the highlighted option's preview below.
fn render_question_options(
    id: &str,
    questions: &[Question],
    card: &CardState,
    ui: UiColors,
    cx: &mut Context<ClaudeChat>,
) -> AnyElement {
    let step = card.step.min(questions.len().saturating_sub(1));
    let Some(question) = questions.get(step) else {
        return div().into_any_element();
    };
    let chosen = card.chosen.get(step).cloned().unwrap_or_default();
    let other_open = card.other.get(step).is_some_and(Option::is_some);
    let mut rows: Vec<AnyElement> = question
        .options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let on = chosen.contains(&index);
            let mark = if question.multi_select {
                ui::checkbox(
                    SharedString::from(format!("claude-q-check-{id}-{step}-{index}")),
                    ui::CheckState::from_bool(on),
                    ui,
                )
                .into_any_element()
            } else {
                ui::radio(
                    SharedString::from(format!("claude-q-radio-{id}-{step}-{index}")),
                    on,
                    ui,
                )
                .into_any_element()
            };
            let id = id.to_string();
            option_row(
                SharedString::from(format!("claude-q-{id}-{step}-{index}")),
                index,
                index == card.highlighted,
                ui,
            )
            .child(mark)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(option.label.clone())
                    .when(!option.description.is_empty(), |text| {
                        text.child(
                            div()
                                .text_size(px(theme::TEXT_SM))
                                .text_color(ui.text_muted)
                                .child(option.description.clone()),
                        )
                    }),
            )
            .on_click(cx.listener(move |chat, _: &ClickEvent, window, cx| {
                chat.cards.keyboard = Some(id.clone());
                take_row(chat, &id, index, window, cx);
            }))
            .into_any_element()
        })
        .collect();
    let other_index = question.options.len();
    let other_id = id.to_string();
    rows.push(
        option_row(
            SharedString::from(format!("claude-q-{id}-{step}-other")),
            other_index,
            card.highlighted == other_index,
            ui,
        )
        .child(if question.multi_select {
            ui::checkbox(
                SharedString::from(format!("claude-q-check-{id}-{step}-other")),
                ui::CheckState::from_bool(other_open),
                ui,
            )
            .into_any_element()
        } else {
            ui::radio(
                SharedString::from(format!("claude-q-radio-{id}-{step}-other")),
                other_open && chosen.is_empty(),
                ui,
            )
            .into_any_element()
        })
        .child(div().flex_1().text_color(ui.text_muted).child(tr("Other…")))
        .on_click(cx.listener(move |chat, _: &ClickEvent, window, cx| {
            chat.cards.keyboard = Some(other_id.clone());
            take_row(chat, &other_id, other_index, window, cx);
        }))
        .into_any_element(),
    );
    let preview = question
        .options
        .get(card.highlighted)
        .and_then(|option| option.preview.clone());
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .children(rows)
        .children(preview.map(|preview| {
            div()
                .id(SharedString::from(format!("claude-q-preview-{id}")))
                .mt(px(6.))
                .max_h(px(PREVIEW_MAX_HEIGHT))
                .overflow_y_scroll()
                .px(px(8.))
                .py(px(6.))
                .rounded(px(ui::RADIUS_SM))
                .bg(ui.input_background)
                .border_1()
                .border_color(ui.input_border)
                .font_family(theme::code_font())
                .text_size(px(theme::TEXT_SM))
                .whitespace_nowrap()
                .children(preview.lines().map(|line| div().child(line.to_string())).collect::<Vec<_>>())
        }))
        .into_any_element()
}

/// Back / Next / Submit of Claude's questions.
fn render_question_footer(
    id: &str,
    questions: &[Question],
    card: &CardState,
    ui: UiColors,
    cx: &mut Context<ClaudeChat>,
) -> AnyElement {
    let step = card.step.min(questions.len().saturating_sub(1));
    let last = step + 1 >= questions.len();
    let answered = card.chosen.get(step).is_some_and(|chosen| !chosen.is_empty())
        || card.other.get(step).is_some_and(Option::is_some);
    let label = if last { tr("Submit") } else { tr("Next") };
    let (next_id, back_id) = (id.to_string(), id.to_string());
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .when(questions.len() > 1, |row| {
            row.child(
                div()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(trf("{0} of {1}", &[&(step + 1), &questions.len()])),
            )
        })
        .child(div().flex_1())
        .when(step > 0, |row| {
            row.child(
                ui::text_button(
                    SharedString::from(format!("claude-q-back-{id}")),
                    tr("Back"),
                    false,
                    ui,
                )
                .on_click(cx.listener(move |chat, _: &ClickEvent, window, cx| {
                    if let Some(card) = chat.cards.cards.get_mut(&back_id) {
                        card.step = card.step.saturating_sub(1);
                        card.highlighted = 0;
                    }
                    if let Some(focus) = &chat.cards.focus {
                        window.focus(focus);
                    }
                    cx.notify();
                })),
            )
        })
        .child(
            ui::primary_button(
                SharedString::from(format!("claude-q-next-{id}")),
                label,
                answered,
                ui,
            )
            .on_click(cx.listener(move |chat, _: &ClickEvent, window, cx| {
                let Some(pending) = pending_of(chat, &next_id, cx) else {
                    return;
                };
                if let PendingKind::Questions(questions) = &pending.kind {
                    next_question(chat, &next_id, &pending, questions, window, cx);
                }
            })),
        )
        .into_any_element()
}

/// The field of a card: what to do instead (deny, reject), the plan's feedback, an "Other" answer.
fn render_field(
    id: &str,
    pending: &Pending,
    card: &CardState,
    ui: UiColors,
    cx: &mut Context<ClaudeChat>,
) -> Option<AnyElement> {
    // The button: a refusal in the error color; keeping on planning isn't one.
    let (field, button, danger): (Entity<TextInput>, Option<SharedString>, bool) =
        match &pending.kind {
            PendingKind::Questions(_) => {
                let step = card.step;
                (card.other.get(step)?.clone()?, None, false)
            }
            PendingKind::Plan { .. } => {
                (card.field.clone()?, Some(tr("Keep Planning").into()), false)
            }
            PendingKind::Edit(_) => (card.field.clone()?, Some(tr("Reject").into()), true),
            PendingKind::Tool => (card.field.clone()?, Some(tr("Deny").into()), true),
        };
    let submit_id = id.to_string();
    let (enter_id, escape_id) = (id.to_string(), id.to_string());
    Some(
        div()
            .key_context(FIELD_CONTEXT)
            .on_action(cx.listener(move |chat, _: &SubmitField, window, cx| {
                submit_field(chat, &enter_id, window, cx)
            }))
            .on_action(cx.listener(move |chat, _: &CloseField, window, cx| {
                close_field(chat, &escape_id, window, cx)
            }))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(div().flex_1().min_w_0().child(field))
            .children(button.map(|label| {
                ui::text_button(
                    SharedString::from(format!("claude-field-submit-{submit_id}")),
                    label,
                    danger,
                    ui,
                )
                .on_click(cx.listener(move |chat, _: &ClickEvent, window, cx| {
                    submit_field(chat, &submit_id, window, cx)
                }))
            }))
            .into_any_element(),
    )
}

/// Claude's task list: "Tasks 2/5" and the task in progress; the list when expanded.
fn render_tasks(
    state: &CardsState,
    model: &Session,
    ui: UiColors,
    cx: &mut Context<ClaudeChat>,
) -> Option<AnyElement> {
    if model.tasks.is_empty() {
        return None;
    }
    let done = model
        .tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Completed)
        .count();
    let current = model
        .tasks
        .iter()
        .find(|task| task.status == TaskStatus::InProgress)
        .map(|task| task.active_form.clone().unwrap_or_else(|| task.subject.clone()));
    let collapsed = state.tasks_collapsed;
    let header = div()
        .id("claude-tasks-header")
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(24.))
        .px(px(4.))
        .rounded(px(ui::RADIUS_SM))
        .cursor_pointer()
        .hover(move |style| style.bg(ui.hover))
        .on_click(cx.listener(|chat, _: &ClickEvent, _, cx| {
            chat.cards.tasks_collapsed = !chat.cards.tasks_collapsed;
            cx.notify();
        }))
        .child(
            icon(
                if collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                },
                ui.dim,
            )
            .size(px(12.)),
        )
        .child(icon(IconName::Checklist, ui.text_muted).size(px(14.)))
        .child(
            div()
                .flex_none()
                .font_weight(FontWeight::MEDIUM)
                .child(tr("Tasks")),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.dim)
                .child(format!("{done}/{}", model.tasks.len())),
        )
        .children(current.filter(|_| collapsed).map(|current| {
            div()
                .min_w_0()
                .truncate()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.accent_text)
                .child(current)
        }));
    let list = (!collapsed).then(|| {
        div()
            .flex()
            .flex_col()
            .gap(px(1.))
            .pl(px(22.))
            .children(model.tasks.iter().enumerate().map(|(index, task)| {
                let (mark, color, text): (AnyElement, _, String) = match task.status {
                    TaskStatus::Pending => (
                        ui::radio(("claude-task", index), false, ui).into_any_element(),
                        ui.text_muted,
                        task.subject.clone(),
                    ),
                    TaskStatus::InProgress => (
                        ui::radio(("claude-task", index), true, ui).into_any_element(),
                        ui.foreground,
                        task.active_form.clone().unwrap_or_else(|| task.subject.clone()),
                    ),
                    TaskStatus::Completed => (
                        icon(IconName::CheckCircle, ui.success)
                            .size(px(14.))
                            .into_any_element(),
                        ui.dim,
                        task.subject.clone(),
                    ),
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_h(px(22.))
                    .child(mark)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(color)
                            .when(task.status == TaskStatus::Completed, |text| {
                                text.line_through()
                            })
                            .when(task.status == TaskStatus::InProgress, |text| {
                                text.font_weight(FontWeight::MEDIUM)
                            })
                            .child(text),
                    )
            }))
    });
    Some(
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(2.))
            .px(px(6.))
            .py(px(4.))
            .rounded(px(ui::RADIUS_MD))
            .border_1()
            .border_color(ui.island_border)
            .child(header)
            .children(list)
            .into_any_element(),
    )
}

/// Commands and agents running in the background, each with Stop.
fn render_background(
    session: &Entity<ClaudeSession>,
    model: &Session,
    ui: UiColors,
) -> Option<AnyElement> {
    if model.background.is_empty() {
        return None;
    }
    let rows = model.background.iter().map(|task| {
        let agent = task.task_type.contains("agent");
        let session = session.clone();
        let task_id = task.task_id.clone();
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .min_h(px(28.))
            .px(px(6.))
            .child(
                icon(
                    if agent {
                        IconName::Agent
                    } else {
                        IconName::Terminal
                    },
                    ui.text_muted,
                )
                .size(px(14.)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(task.description.clone()),
            )
            .child(
                ui::text_button(
                    SharedString::from(format!("claude-stop-task-{}", task.task_id)),
                    tr("Stop"),
                    false,
                    ui,
                )
                .on_click(move |_: &ClickEvent, _, cx| {
                    let task_id = task_id.clone();
                    session.update(cx, |session, cx| session.stop_task(task_id, cx))
                }),
            )
    });
    Some(
        div()
            .flex_none()
            .flex()
            .flex_col()
            .px(px(6.))
            .py(px(4.))
            .rounded(px(ui::RADIUS_MD))
            .border_1()
            .border_color(ui.island_border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(24.))
                    .px(px(4.))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(tr("In the background")),
                    )
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .child(model.background.len().to_string()),
                    ),
            )
            .children(rows)
            .into_any_element(),
    )
}

/// Why an edit doesn't apply, in the interface language (the reasons of `flux_claude::edits`).
fn reason_text(reason: &str) -> String {
    match reason {
        "The file doesn't exist" => tr("The file doesn't exist").to_string(),
        "The edit has no text to replace" => tr("The edit has no text to replace").to_string(),
        "The text to replace isn't in the file any more" => {
            tr("The text to replace isn't in the file any more").to_string()
        }
        "The text to replace occurs more than once" => {
            tr("The text to replace occurs more than once").to_string()
        }
        other => other.to_string(),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_claude::session::QuestionOption;
    use serde_json::json;
    use std::path::PathBuf;

    fn pending(tool: &str, kind: PendingKind, suggestions: Vec<Value>) -> Pending {
        Pending {
            id: "r1".into(),
            tool_use_id: "t1".into(),
            tool: tool.into(),
            display_name: tool.into(),
            description: None,
            input: json!({}),
            kind,
            suggestions,
            blocked_path: None,
            reason: None,
            default_to_no: false,
            suppress_always_allow: false,
            from_agent: false,
        }
    }

    #[test]
    fn suggestions_read_as_sentences() {
        let cwd = Path::new("/p");
        let rule = json!({"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "touch:*"}],
                          "behavior": "allow", "destination": "localSettings"});
        assert_eq!(
            suggestion_label(&rule, cwd, false).unwrap(),
            "Allow, and don't ask again for `touch:*` in this project"
        );
        let mode = json!({"type": "setMode", "mode": "acceptEdits", "destination": "session"});
        assert_eq!(suggestion_label(&mode, cwd, true).unwrap(), "Accept all edits in this session");
        assert_eq!(
            suggestion_label(&mode, cwd, false).unwrap(),
            "Allow, and accept edits in this session"
        );
        let fetch = json!({"type": "addRules", "rules": [{"toolName": "WebFetch", "ruleContent": "domain:example.com"}],
                           "behavior": "allow", "destination": "localSettings"});
        assert_eq!(
            suggestion_label(&fetch, cwd, false).unwrap(),
            "Allow, and don't ask again for fetches from example.com in this project"
        );
        let read = json!({"type": "addRules", "rules": [{"toolName": "Read", "ruleContent": "//tmp/data/**"}],
                          "behavior": "allow", "destination": "session"});
        assert_eq!(
            suggestion_label(&read, cwd, false).unwrap(),
            "Allow, and don't ask again for reading `/tmp/data/**` in this session"
        );
        let dirs = json!({"type": "addDirectories", "directories": ["/p/sub"], "destination": "session"});
        assert_eq!(
            suggestion_label(&dirs, cwd, false).unwrap(),
            "Allow, and allow access to `sub` in this session"
        );
        let deny = json!({"type": "addRules", "rules": [{"toolName": "Bash"}], "behavior": "deny", "destination": "session"});
        assert_eq!(suggestion_label(&deny, cwd, false), None);
    }

    #[test]
    fn permission_rows_end_with_deny() {
        let suggestion = json!({"type": "setMode", "mode": "acceptEdits", "destination": "session"});
        let rows = rows(
            &pending("Bash", PendingKind::Tool, vec![suggestion.clone()]),
            Path::new("/p"),
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].choice, Choice::Allow { remember: vec![] });
        assert_eq!(rows[1].choice, Choice::Allow { remember: vec![suggestion.clone()] });
        assert_eq!(rows[2].choice, Choice::Deny);
        let mut hidden = pending("Bash", PendingKind::Tool, vec![suggestion]);
        hidden.suppress_always_allow = true;
        assert_eq!(rows_len(&hidden), 2);
        let plan = pending("ExitPlanMode", PendingKind::Plan { plan: "x".into(), path: None }, vec![]);
        assert_eq!(rows_len(&plan), 3);
    }

    fn rows_len(pending: &Pending) -> usize {
        rows(pending, Path::new("/p")).len()
    }

    #[test]
    fn a_default_no_card_starts_on_deny() {
        let mut card = pending("Bash", PendingKind::Tool, vec![]);
        card.default_to_no = true;
        assert_eq!(new_card(&card).highlighted, 1);
    }

    #[test]
    fn answers_join_several_labels() {
        let option = |label: &str| QuestionOption {
            label: label.into(),
            description: String::new(),
            preview: None,
        };
        let questions = vec![
            Question {
                question: "Color?".into(),
                header: "Color".into(),
                options: vec![option("Red"), option("Green")],
                multi_select: false,
            },
            Question {
                question: "Sizes?".into(),
                header: "Size".into(),
                options: vec![option("S"), option("M"), option("L")],
                multi_select: true,
            },
        ];
        let answers = question_answers(
            &questions,
            &[vec![1], vec![0, 2]],
            &[String::new(), "XL".into()],
        );
        assert_eq!(
            answers,
            vec![
                ("Color?".to_string(), "Green".to_string()),
                ("Sizes?".to_string(), "S, L, XL".to_string()),
            ]
        );
    }

    #[test]
    fn deny_without_words_stops_the_turn() {
        assert!(matches!(deny("  "), Answer::Deny { interrupt: true, .. }));
        match deny("use cargo") {
            Answer::Deny { message, interrupt } => {
                assert!(!interrupt);
                assert!(message.ends_with("use cargo"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(keep_planning(""), Answer::KeepPlanning(text) if text == KEEP_PLANNING));
    }

    #[test]
    fn the_preview_shows_the_first_block() {
        let lines = preview_lines("a\nb\nc\n", "a\nB\nc\n", 6);
        assert_eq!(
            lines,
            vec![
                (PreviewKind::Removed, Some(2), "b".to_string()),
                (PreviewKind::Added, Some(2), "B".to_string()),
            ]
        );
        assert_eq!(line_counts("a\nb\n", "a\nB\nC\n"), (2, 1));
        let long: String = (0..20).map(|n| format!("{n}\n")).collect();
        let lines = preview_lines("", &long, 6);
        assert_eq!(lines.len(), 7);
        assert_eq!(lines[6].0, PreviewKind::More);
        let _ = PathBuf::new();
    }

    #[test]
    fn paths_are_short() {
        assert_eq!(short_path("/p/src/main.rs", Path::new("/p")), "src/main.rs");
        assert_eq!(rule_path("//tmp/x/**", Path::new("/p")), "/tmp/x/**");
        assert_eq!(rule_path("src/**", Path::new("/p")), "src/**");
    }
}
