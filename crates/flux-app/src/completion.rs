//! Code completion: the menu at the cursor with the language server's suggestions.
//!
//! A session starts when a word begins (its first character is typed), on a trigger character of
//! the server (`.`, `:`), or on ctrl-space: a `textDocument/completion` request goes out, and the
//! menu shows when the answer has matches. While it is open, the word from the session's start to
//! the cursor (the query) filters and sorts the items on our side: fuzzy, by `filterText`, prefix
//! matches first, the server's order (`sortText`) among equally good matches. An incomplete list
//! (`isIncomplete`) is requested again whenever the query changes. The items' edits refer to the
//! text at the request: the session keeps the changes made since and maps positions through them.
//!
//! The session ends on Esc, when the query stops being a word (a space, `(`), when the cursor
//! moves without an edit (arrows, a click), when the editor loses focus, and when an item is
//! accepted: Enter inserts (replaces the word up to the cursor), Tab replaces the whole word. The
//! item's additional edits (imports) go into the same undo step; if the item still has to be
//! resolved for them (`completionItem/resolve`), they follow as soon as it is.

use std::collections::BTreeMap;
use std::ops::Range;

use flux_core::text::{CharClass, char_class, indentation, line_start};
use flux_core::transaction::Operation;
use flux_core::{
    Assoc, ChangeSet, EditKind, Range as Cursor, Rope, Selection, TextChange, Transaction,
};
use flux_lsp::lsp_types::request::{Completion, ResolveCompletionItem};
use flux_lsp::lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionItemTag, CompletionParams,
    CompletionResponse, CompletionTextEdit, CompletionTriggerKind, Documentation, InsertTextFormat,
    InsertTextMode, TextDocumentIdentifier, TextDocumentPositionParams, Uri,
};
use flux_lsp::snippet::{self, Snippet};
use flux_lsp::{LanguageServer, Lines, RequestError};
use flux_search::match_list;
use flux_syntax::language_for_path;
use gpui::{
    AnyElement, App, ClickEvent, Context, Corner, Div, FontWeight, HighlightStyle, Hsla,
    KeyBinding, KeyContext, Pixels, ScrollStrategy, StyledText, Subscription, Task,
    UniformListScrollHandle, Window, actions, anchored, deferred, div, point, prelude::*, px,
    uniform_list,
};

use crate::editor::Editor;
use crate::markdown::{self, Block};
use crate::picker::byte_ranges;
use crate::popup::{self, POPUP_GAP, Side, WINDOW_MARGIN};
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_SM, RADIUS_XS};

/// Height of a menu row.
const ROW_HEIGHT: f32 = 26.;
const MAX_VISIBLE_ROWS: usize = 10;
const MENU_WIDTH: f32 = 440.;
/// Inset of the rows from the menu edge, and of a row's content from the row edge.
const MENU_PADDING: f32 = 4.;
const ROW_PADDING: f32 = 6.;
/// The kind badge before the label, and the gap after it.
const KIND_SIZE: f32 = 16.;
const KIND_GAP: f32 = 8.;
/// The documentation of the selected item, beside the menu.
const DOCS_WIDTH: f32 = 360.;
const DOCS_MAX_HEIGHT: f32 = 320.;
/// Past this length the "word" is not one (a minified line): the session ends.
const MAX_QUERY_CHARS: usize = 256;

actions!(editor, [ShowCompletions]);
actions!(
    completion,
    [
        SelectNext,
        SelectPrevious,
        SelectNextPage,
        SelectPreviousPage,
        ConfirmInsert,
        ConfirmReplace,
        Dismiss,
    ]
);

/// The menu's keys win over the editor's (same context depth, bound later) only while it shows.
const MENU_CONTEXT: &str = "Editor && showing_completions";

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-space", ShowCompletions, Some("Editor")),
        KeyBinding::new("down", SelectNext, Some(MENU_CONTEXT)),
        KeyBinding::new("up", SelectPrevious, Some(MENU_CONTEXT)),
        KeyBinding::new("ctrl-n", SelectNext, Some(MENU_CONTEXT)),
        KeyBinding::new("ctrl-p", SelectPrevious, Some(MENU_CONTEXT)),
        KeyBinding::new("pagedown", SelectNextPage, Some(MENU_CONTEXT)),
        KeyBinding::new("pageup", SelectPreviousPage, Some(MENU_CONTEXT)),
        KeyBinding::new("enter", ConfirmInsert, Some(MENU_CONTEXT)),
        KeyBinding::new("tab", ConfirmReplace, Some(MENU_CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(MENU_CONTEXT)),
    ]);
}

/// Completion state of an editor (`Editor::completion`): the open session, and accepted items
/// whose additional edits are still being resolved. `None` in the editor when there is neither.
pub struct CompletionMenu {
    session: Option<Session>,
    pending: Vec<PendingEdits>,
    next_pending: u64,
}

struct Session {
    server: LanguageServer,
    uri: Uri,
    /// The server fills documentation and edits in on `completionItem/resolve`.
    resolve_provider: bool,
    /// Where the completed word starts, in current coordinates.
    word_start: usize,
    /// The character before `word_start` at the start: if an edit changes it (the trigger `.` is
    /// deleted), the session ends.
    before_start: Option<char>,
    /// The text from `word_start` to the cursor, as last filtered by.
    query: String,
    list: Option<ItemList>,
    /// The request in flight; dropping it cancels the request.
    request: Option<Request>,
    matches: Vec<Match>,
    selected: usize,
    scroll: UniformListScrollHandle,
    /// The selection after the last edit: a change without an edit means the cursor moved away.
    selection: Selection,
    /// The text changed since the last refresh.
    edited: bool,
    /// An edit away from the cursors (undo elsewhere, a replacement): the session ends.
    foreign: bool,
    /// Resolving and preparing the documentation of the selected item: `(list id, item)`.
    details: Option<((u64, usize), Task<()>)>,
    next_list_id: u64,
    _subscriptions: [Subscription; 2],
}

/// An answer: the items, and the text their edits refer to.
struct ItemList {
    id: u64,
    items: Vec<Item>,
    /// Typing should ask again: the list is not complete.
    incomplete: bool,
    origin: Origin,
}

/// The text a request was made for, and the changes since: maps positions of the answer to the
/// current text.
#[derive(Clone)]
struct Origin {
    text: Rope,
    changes: Vec<ChangeSet>,
}

impl Origin {
    fn new(text: Rope) -> Self {
        Self {
            text,
            changes: Vec::new(),
        }
    }

    fn map(&self, pos: usize, assoc: Assoc) -> usize {
        self.changes
            .iter()
            .fold(pos, |pos, changes| changes.map_pos(pos, assoc))
    }
}

struct Request {
    origin: Origin,
    _task: Task<()>,
}

struct Item {
    lsp: CompletionItem,
    /// Resolved, or nothing to resolve: documentation and edits are final.
    resolved: bool,
    /// Start of the item's edit in the request's text: the item is filtered by the text from there
    /// to the cursor.
    edit_start: Option<usize>,
    /// The documentation as blocks, once prepared for the side panel.
    docs: Option<Vec<Block>>,
}

impl Item {
    fn new(lsp: CompletionItem, lines: &Lines, resolved: bool) -> Self {
        let edit_start = lsp.text_edit.as_ref().map(|edit| {
            let range = match edit {
                CompletionTextEdit::Edit(edit) => edit.range,
                CompletionTextEdit::InsertAndReplace(edit) => edit.insert,
            };
            lines.from_lsp(range.start)
        });
        Self {
            lsp,
            resolved,
            edit_start,
            docs: None,
        }
    }

    fn filter_text(&self) -> &str {
        self.lsp.filter_text.as_deref().unwrap_or(&self.lsp.label)
    }

    fn sort_text(&self) -> &str {
        self.lsp.sort_text.as_deref().unwrap_or(&self.lsp.label)
    }

    /// The row's text on the right: the label's description, otherwise the detail (first line).
    fn description(&self) -> Option<String> {
        self.lsp
            .label_details
            .as_ref()
            .and_then(|details| details.description.as_deref())
            .or(self.lsp.detail.as_deref())
            .and_then(|text| text.lines().next())
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    }

    fn deprecated(&self) -> bool {
        self.lsp.deprecated == Some(true)
            || self
                .lsp
                .tags
                .as_ref()
                .is_some_and(|tags| tags.contains(&CompletionItemTag::DEPRECATED))
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Match {
    item: usize,
    score: u32,
    /// The query starts the filter text (ignoring case): such matches go first.
    prefix: bool,
    /// Matched characters of the label, for highlighting.
    positions: Vec<usize>,
}

/// An accepted item waiting for `completionItem/resolve` to bring its additional edits.
struct PendingEdits {
    id: u64,
    origin: Origin,
    _task: Task<()>,
}

/// How an accepted item replaces the word: up to the cursor (Enter) or the whole word (Tab).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Insert,
    Replace,
}

// --- Hooks from the editor ---

/// From `Editor::text_changed`: positions follow the edit; the next refresh refilters.
pub fn text_changed(editor: &mut Editor, changes: &[TextChange]) {
    let Some(menu) = &mut editor.completion else {
        return;
    };
    for change in changes {
        if let Some(session) = &mut menu.session {
            session.edited = true;
            if !at_cursors(&change.changes, &session.selection) {
                session.foreign = true;
            }
            session.selection = session.selection.map(&change.changes);
            session.word_start = change.changes.map_pos(session.word_start, Assoc::Before);
            let origins = session.list.iter_mut().map(|list| &mut list.origin).chain(
                session
                    .request
                    .iter_mut()
                    .map(|request| &mut request.origin),
            );
            for origin in origins {
                origin.changes.push(change.changes.clone());
            }
        }
        for pending in &mut menu.pending {
            pending.origin.changes.push(change.changes.clone());
        }
    }
}

/// From `replace_text_in_range`, after the typed text is in: a trigger character starts a new
/// session, the first character of a word starts one if there is none. Filtering by the rest of
/// the word happens on refresh.
pub fn typed(editor: &mut Editor, text: &str, window: &mut Window, cx: &mut Context<Editor>) {
    let Some(last) = text.chars().last() else {
        return;
    };
    let Some((server, _)) = crate::lsp::document_server(editor) else {
        return;
    };
    let Some(capabilities) = server.capabilities() else {
        return;
    };
    let Some(options) = &capabilities.completion_provider else {
        return;
    };
    let primary = editor.document.selection().primary();
    if !primary.is_empty() {
        return;
    }
    let trigger = options
        .trigger_characters
        .iter()
        .flatten()
        .any(|trigger| trigger.chars().eq([last]));
    if trigger {
        let context = CompletionContext {
            trigger_kind: CompletionTriggerKind::TRIGGER_CHARACTER,
            trigger_character: Some(last.to_string()),
        };
        return start(editor, context, window, cx);
    }
    if session(editor).is_some() || !is_word_char(last) {
        return;
    }
    let cursor = primary.head;
    // Only when a word begins: all of it came with this input.
    if cursor - word_start(editor.document.text(), cursor) > text.chars().count() {
        return;
    }
    if in_comment_or_string(editor, cursor, cx) {
        return;
    }
    start(editor, invoked(), window, cx);
}

/// Adds `showing_completions` to the editor's key context while the menu shows.
pub fn extend_key_context(editor: &Editor, context: &mut KeyContext) {
    if is_showing(editor) {
        context.add("showing_completions");
    }
}

/// Registers the editor actions: ctrl-space always, the menu's keys only while it shows (so the
/// command palette doesn't list them).
pub fn actions(root: Div, editor: &Editor, cx: &mut Context<Editor>) -> Div {
    let root =
        root.on_action(cx.listener(|editor, _: &ShowCompletions, window, cx| {
            start(editor, invoked(), window, cx)
        }));
    if !is_showing(editor) {
        return root;
    }
    let page = MAX_VISIBLE_ROWS as isize - 1;
    root.on_action(cx.listener(|editor, _: &SelectNext, _, cx| move_selection(editor, 1, true, cx)))
        .on_action(
            cx.listener(|editor, _: &SelectPrevious, _, cx| move_selection(editor, -1, true, cx)),
        )
        .on_action(cx.listener(move |editor, _: &SelectNextPage, _, cx| {
            move_selection(editor, page, false, cx)
        }))
        .on_action(cx.listener(move |editor, _: &SelectPreviousPage, _, cx| {
            move_selection(editor, -page, false, cx)
        }))
        .on_action(cx.listener(|editor, _: &ConfirmInsert, window, cx| {
            confirm(editor, Mode::Insert, window, cx)
        }))
        .on_action(cx.listener(|editor, _: &ConfirmReplace, window, cx| {
            confirm(editor, Mode::Replace, window, cx)
        }))
        .on_action(cx.listener(|editor, _: &Dismiss, _, cx| close(editor, cx)))
}

// --- Session ---

fn invoked() -> CompletionContext {
    CompletionContext {
        trigger_kind: CompletionTriggerKind::INVOKED,
        trigger_character: None,
    }
}

fn session(editor: &Editor) -> Option<&Session> {
    editor.completion.as_ref()?.session.as_ref()
}

fn session_mut(editor: &mut Editor) -> Option<&mut Session> {
    editor.completion.as_mut()?.session.as_mut()
}

/// The menu is drawn: there are matches, and the word is on screen.
fn is_showing(editor: &Editor) -> bool {
    session(editor).is_some_and(|session| {
        !session.matches.is_empty() && popup::anchor(editor, session.word_start).is_some()
    })
}

/// Starts a session at the cursor (replacing the open one) and requests the items.
fn start(
    editor: &mut Editor,
    context: CompletionContext,
    window: &mut Window,
    cx: &mut Context<Editor>,
) {
    let Some((server, uri)) = crate::lsp::document_server(editor) else {
        return;
    };
    let Some(capabilities) = server.capabilities() else {
        return;
    };
    let Some(options) = &capabilities.completion_provider else {
        return;
    };
    let selection = editor.document.selection().clone();
    let primary = selection.primary();
    if !primary.is_empty() {
        return;
    }
    let text = editor.document.text();
    let cursor = primary.head;
    let word_start = word_start(text, cursor);
    let session = Session {
        server,
        uri,
        resolve_provider: options.resolve_provider == Some(true),
        word_start,
        before_start: word_start.checked_sub(1).map(|i| text.char(i)),
        query: text.slice(word_start..cursor).to_string(),
        list: None,
        request: None,
        matches: Vec::new(),
        selected: 0,
        scroll: UniformListScrollHandle::new(),
        selection,
        edited: false,
        foreign: false,
        details: None,
        next_list_id: 0,
        _subscriptions: [
            cx.observe_self(refresh),
            cx.on_blur(&editor.focus_handle, window, |editor, _, cx| {
                close(editor, cx)
            }),
        ],
    };
    let menu = editor.completion.get_or_insert_with(|| CompletionMenu {
        session: None,
        pending: Vec::new(),
        next_pending: 0,
    });
    menu.session = Some(session);
    request(editor, context, cx);
}

/// Requests the items at the cursor; the answer replaces the list when it comes.
fn request(editor: &mut Editor, context: CompletionContext, cx: &mut Context<Editor>) {
    let text = editor.document.text().clone();
    let cursor = editor.document.selection().primary().head;
    let Some(session) = session_mut(editor) else {
        return;
    };
    let params = CompletionParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: session.uri.clone(),
            },
            position: flux_lsp::position::to_lsp(&text, cursor),
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        context: Some(context),
    };
    let response = session.server.request::<Completion>(params);
    let task = cx.spawn(async move |this, cx| {
        let response = response.await;
        this.update(cx, |editor, cx| receive(editor, response, cx))
            .ok();
    });
    session.request = Some(Request {
        origin: Origin::new(text),
        _task: task,
    });
}

fn receive(
    editor: &mut Editor,
    response: Result<Option<CompletionResponse>, RequestError>,
    cx: &mut Context<Editor>,
) {
    let Some(session) = session_mut(editor) else {
        return;
    };
    let Some(request) = session.request.take() else {
        return;
    };
    let (items, incomplete) = match response {
        Ok(Some(CompletionResponse::Array(items))) => (items, false),
        Ok(Some(CompletionResponse::List(list))) => (list.items, list.is_incomplete),
        Ok(None) => (Vec::new(), false),
        // The document changed under the request, or the server failed: keep the previous list,
        // if there is one; a failure is not worth a message while typing.
        Err(_) => {
            if session.list.is_none() {
                close(editor, cx);
            }
            return;
        }
    };
    let lines = Lines::new(&request.origin.text);
    let resolved = !session.resolve_provider;
    let items = items
        .into_iter()
        .map(|item| Item::new(item, &lines, resolved))
        .collect();
    let id = session.next_list_id;
    session.next_list_id += 1;
    session.list = Some(ItemList {
        id,
        items,
        incomplete,
        origin: request.origin,
    });
    refilter(editor, cx);
    cx.notify();
}

/// After every notify of the editor (an edit, a cursor movement, a blink): a cursor movement
/// without an edit ends the session; an edit changes the query, and the items are filtered again
/// (and requested again, if the list is incomplete).
fn refresh(editor: &mut Editor, cx: &mut Context<Editor>) {
    let selection = editor.document.selection();
    let Some(session) = editor
        .completion
        .as_mut()
        .and_then(|menu| menu.session.as_mut())
    else {
        return;
    };
    if !session.edited {
        if *selection != session.selection {
            close(editor, cx);
        }
        return;
    }
    if session.foreign {
        return close(editor, cx);
    }
    session.edited = false;
    session.selection = selection.clone();
    let query = query_at(
        editor.document.text(),
        selection,
        session.word_start,
        session.before_start,
    );
    let Some(query) = query else {
        return close(editor, cx);
    };
    if query == session.query {
        return;
    }
    session.query = query;
    if session.list.as_ref().is_some_and(|list| list.incomplete) {
        let context = CompletionContext {
            trigger_kind: CompletionTriggerKind::TRIGGER_FOR_INCOMPLETE_COMPLETIONS,
            trigger_character: None,
        };
        request(editor, context, cx);
    }
    refilter(editor, cx);
}

/// Filters and sorts the items by the query; selects the first match (or the preselected item
/// before anything is typed). With nothing left to show and nothing to wait for, the session ends.
fn refilter(editor: &mut Editor, cx: &mut Context<Editor>) {
    let text = editor.document.text();
    let cursor = editor.document.selection().primary().head;
    let Some(session) = editor
        .completion
        .as_mut()
        .and_then(|menu| menu.session.as_mut())
    else {
        return;
    };
    let Some(list) = &session.list else {
        return;
    };
    let groups = query_groups(list, text, cursor, session.word_start);
    session.matches = rank(&list.items, &groups);
    session.selected = if session.query.is_empty() {
        session
            .matches
            .iter()
            .position(|m| list.items[m.item].lsp.preselect == Some(true))
            .unwrap_or(0)
    } else {
        0
    };
    session
        .scroll
        .scroll_to_item(session.selected, ScrollStrategy::Top);
    if session.matches.is_empty() && !list.incomplete && session.request.is_none() {
        return close(editor, cx);
    }
    load_details(editor, cx);
}

/// Ends the session (accepted items keep waiting for their edits).
fn close(editor: &mut Editor, cx: &mut Context<Editor>) {
    let Some(menu) = &mut editor.completion else {
        return;
    };
    if menu.session.take().is_some() {
        cx.notify();
    }
    if menu.pending.is_empty() {
        editor.completion = None;
    }
}

fn move_selection(editor: &mut Editor, delta: isize, wrap: bool, cx: &mut Context<Editor>) {
    let Some(session) = session_mut(editor) else {
        return;
    };
    let count = session.matches.len() as isize;
    if count == 0 {
        return;
    }
    let target = session.selected as isize + delta;
    let index = if wrap {
        target.rem_euclid(count)
    } else {
        target.clamp(0, count - 1)
    } as usize;
    let strategy = if index < session.selected {
        ScrollStrategy::Top
    } else {
        ScrollStrategy::Bottom
    };
    session.selected = index;
    session.scroll.scroll_to_item(index, strategy);
    load_details(editor, cx);
    cx.notify();
}

/// Resolves the selected item (if the server resolves lazily) and prepares its documentation for
/// the side panel, in the background.
fn load_details(editor: &mut Editor, cx: &mut Context<Editor>) {
    let language = editor.document.path().and_then(language_for_path);
    let Some(session) = session_mut(editor) else {
        return;
    };
    let Some(list) = &session.list else {
        return;
    };
    let Some(index) = session.matches.get(session.selected).map(|m| m.item) else {
        session.details = None;
        return;
    };
    let key = (list.id, index);
    let item = &list.items[index];
    if item.resolved && item.docs.is_some()
        || session.details.as_ref().is_some_and(|(at, _)| *at == key)
    {
        return;
    }
    let resolve = (!item.resolved).then(|| {
        session
            .server
            .request::<ResolveCompletionItem>(item.lsp.clone())
    });
    let current = item.lsp.clone();
    let scopes = markdown::scopes(cx);
    let task = cx.spawn(async move |this, cx| {
        let lsp = match resolve {
            Some(resolve) => resolve.await.unwrap_or(current),
            None => current,
        };
        let documentation = lsp.documentation.clone();
        let docs = cx
            .background_executor()
            .spawn(async move { documentation_blocks(documentation, language, &scopes) })
            .await;
        this.update(cx, |editor, cx| {
            let Some(session) = session_mut(editor) else {
                return;
            };
            session.details = None;
            let Some(list) = session.list.as_mut().filter(|list| list.id == key.0) else {
                return;
            };
            let item = &mut list.items[key.1];
            item.lsp = lsp;
            item.resolved = true;
            item.docs = Some(docs);
            cx.notify();
        })
        .ok();
    });
    session.details = Some((key, task));
}

/// Accepts the selected item: its edit at the cursor (and at the other cursors with the same word
/// around them) and its additional edits, as one undo step.
fn confirm(editor: &mut Editor, mode: Mode, window: &mut Window, cx: &mut Context<Editor>) {
    let Some(menu) = &mut editor.completion else {
        return;
    };
    let Some(mut session) = menu.session.take() else {
        return;
    };
    let (Some(list), Some(index)) = (
        session.list.take(),
        session.matches.get(session.selected).map(|m| m.item),
    ) else {
        return close(editor, cx);
    };
    let item = &list.items[index];
    let edit = ItemEdit::new(&item.lsp, &list.origin.text);
    let parsed = edit.snippet.then(|| snippet::parse(&edit.text));
    let additional = item
        .lsp
        .additional_text_edits
        .as_deref()
        .map(|edits| flux_lsp::edit::from_lsp(&list.origin.text, edits))
        .unwrap_or_default();
    let target = Target {
        text: editor.document.text(),
        selection: editor.document.selection(),
        word_start: session.word_start,
        origin: &list.origin,
        line_ending: editor.document.line_ending(),
    };
    let tx = accept(&edit, parsed, &additional, mode, &target);
    let command = item
        .lsp
        .command
        .as_ref()
        .map(|command| command.command.clone());
    // Edits may only arrive with the resolved item: they follow it.
    if !item.resolved && item.lsp.additional_text_edits.is_none() {
        let resolve = session
            .server
            .request::<ResolveCompletionItem>(item.lsp.clone());
        let id = menu.next_pending;
        menu.next_pending += 1;
        let task = cx.spawn(async move |this, cx| {
            let resolved = resolve.await.ok();
            this.update(cx, |editor, cx| apply_pending(editor, id, resolved, cx))
                .ok();
        });
        menu.pending.push(PendingEdits {
            id,
            origin: list.origin.clone(),
            _task: task,
        });
    }
    if menu.pending.is_empty() {
        editor.completion = None;
    }
    cx.notify();
    editor.apply(tx, EditKind::Other, cx);
    if command.as_deref() == Some("editor.action.triggerSuggest") {
        start(editor, invoked(), window, cx);
    }
}

/// The resolved item of an accepted completion: its additional edits, mapped through everything
/// typed since, as a separate undo step.
fn apply_pending(
    editor: &mut Editor,
    id: u64,
    resolved: Option<CompletionItem>,
    cx: &mut Context<Editor>,
) {
    let Some(menu) = &mut editor.completion else {
        return;
    };
    let Some(at) = menu.pending.iter().position(|pending| pending.id == id) else {
        return;
    };
    let pending = menu.pending.remove(at);
    if menu.pending.is_empty() && menu.session.is_none() {
        editor.completion = None;
    }
    let Some(edits) = resolved.and_then(|item| item.additional_text_edits) else {
        return;
    };
    let edits = flux_lsp::edit::from_lsp(&pending.origin.text, &edits);
    let tx = additional_transaction(
        editor.document.text(),
        &edits,
        &pending.origin,
        editor.document.line_ending(),
    );
    if let Some(tx) = tx {
        editor.apply(tx, EditKind::Other, cx);
    }
}

/// Every place the edit changes (in the text before it) touches a cursor of `selection`: typing,
/// Backspace, Delete, a paste at the cursors. Anything else ends the session.
fn at_cursors(changes: &ChangeSet, selection: &Selection) -> bool {
    let touches = |from: usize, to: usize| {
        selection
            .iter()
            .any(|range| from <= range.head && range.head <= to)
    };
    let mut old = 0;
    for op in changes.ops() {
        match op {
            Operation::Retain(n) => old += n,
            Operation::Delete(n) => {
                if !touches(old, old + n) {
                    return false;
                }
                old += n;
            }
            Operation::Insert(_) => {
                if !touches(old, old) {
                    return false;
                }
            }
        }
    }
    true
}

// --- Words ---

fn is_word_char(c: char) -> bool {
    char_class(c) == CharClass::Word
}

/// Start of the word that ends at `pos`.
fn word_start(text: &Rope, pos: usize) -> usize {
    let mut start = pos;
    while start > 0 && pos - start < MAX_QUERY_CHARS && is_word_char(text.char(start - 1)) {
        start -= 1;
    }
    start
}

/// End of the word that starts at `pos`.
fn word_end(text: &Rope, pos: usize) -> usize {
    let mut end = pos;
    while end < text.len_chars() && end - pos < MAX_QUERY_CHARS && is_word_char(text.char(end)) {
        end += 1;
    }
    end
}

/// The query of a session that starts at `word_start`: the word from there to the cursor, if the
/// cursor is still completing it.
fn query_at(
    text: &Rope,
    selection: &Selection,
    word_start: usize,
    before_start: Option<char>,
) -> Option<String> {
    let primary = selection.primary();
    let cursor = primary.head;
    if !primary.is_empty() || cursor < word_start || cursor - word_start > MAX_QUERY_CHARS {
        return None;
    }
    if word_start.checked_sub(1).map(|i| text.char(i)) != before_start {
        return None;
    }
    let query = text.slice(word_start..cursor).to_string();
    query.chars().all(is_word_char).then_some(query)
}

/// The typed character at `pos - 1` is in a comment or a string (by the highlighting): words typed
/// there don't open the menu (trigger characters and ctrl-space still do).
fn in_comment_or_string(editor: &Editor, pos: usize, cx: &App) -> bool {
    let text = editor.document.text();
    let line = text.char_to_line(pos);
    let column = pos - line_start(text, line);
    let Some(spans) = editor
        .highlighter
        .highlight_lines(text, line..line + 1)
        .into_iter()
        .next()
    else {
        return false;
    };
    let theme = Theme::get(cx);
    let scope_at = |column: usize| {
        spans
            .iter()
            .find(|span| span.start <= column && column < span.end)
            .and_then(|span| theme.syntax.get(span.highlight.0))
            .map(|(scope, _)| scope.as_str())
    };
    // Right after an edit the tree may not cover the new character yet: then the one before it.
    let scope = column
        .checked_sub(1)
        .and_then(scope_at)
        .or_else(|| column.checked_sub(2).and_then(scope_at));
    scope.is_some_and(|scope| scope.starts_with("comment") || scope.starts_with("string"))
}

// --- Filtering ---

/// Items grouped by where their query starts (their edit's start, or the word's): the query text
/// of each group and its items. Almost always a single group.
fn query_groups(
    list: &ItemList,
    text: &Rope,
    cursor: usize,
    word_start: usize,
) -> Vec<(String, Vec<usize>)> {
    let line = line_start(text, text.char_to_line(cursor));
    let mut starts: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut mapped: BTreeMap<usize, usize> = BTreeMap::new();
    for (i, item) in list.items.iter().enumerate() {
        let start = item
            .edit_start
            .map(|start| {
                *mapped
                    .entry(start)
                    .or_insert_with(|| list.origin.map(start, Assoc::Before))
            })
            .filter(|&start| start >= line && start <= cursor && cursor - start <= MAX_QUERY_CHARS)
            .unwrap_or(word_start.min(cursor));
        starts.entry(start).or_default().push(i);
    }
    starts
        .into_iter()
        .map(|(start, items)| (text.slice(start..cursor).to_string(), items))
        .collect()
}

/// Fuzzy matches of the items against their group's query, best first: prefix matches, then by
/// score, then in the server's order.
fn rank(items: &[Item], groups: &[(String, Vec<usize>)]) -> Vec<Match> {
    let mut matches = Vec::new();
    for (query, members) in groups {
        let pattern = escape_query(query);
        let filters: Vec<&str> = members.iter().map(|&i| items[i].filter_text()).collect();
        let found = match_list(&pattern, &filters);
        // Positions are in the filter text; when the label doesn't start with it, they are
        // computed on the label.
        let mut on_label = Vec::new();
        for fuzzy in found {
            let index = members[fuzzy.index];
            let filter = filters[fuzzy.index];
            let label = items[index].lsp.label.as_str();
            let positions = if label.starts_with(filter) {
                fuzzy.positions
            } else {
                on_label.push(matches.len());
                Vec::new()
            };
            matches.push(Match {
                item: index,
                score: fuzzy.score,
                prefix: starts_with_ignoring_case(filter, query),
                positions,
            });
        }
        if !on_label.is_empty() && !query.is_empty() {
            let labels: Vec<&str> = on_label
                .iter()
                .map(|&m| items[matches[m].item].lsp.label.as_str())
                .collect();
            for fuzzy in match_list(&pattern, &labels) {
                matches[on_label[fuzzy.index]].positions = fuzzy.positions;
            }
        }
    }
    matches.sort_by(|a, b| {
        b.prefix
            .cmp(&a.prefix)
            .then(b.score.cmp(&a.score))
            .then_with(|| items[a.item].sort_text().cmp(items[b.item].sort_text()))
            .then_with(|| items[a.item].lsp.label.cmp(&items[b.item].lsp.label))
    });
    matches
}

/// The query is a word, but nucleo gives `!`, `^`, `'` at the start and `$` at the end a meaning:
/// those are escaped.
fn escape_query(query: &str) -> String {
    let mut pattern = String::with_capacity(query.len() + 2);
    if query.starts_with(['!', '^', '\'']) {
        pattern.push('\\');
    }
    pattern.push_str(query);
    if pattern.ends_with('$') {
        pattern.insert(pattern.len() - 1, '\\');
    }
    pattern
}

fn starts_with_ignoring_case(text: &str, prefix: &str) -> bool {
    let mut text = text.chars().flat_map(char::to_lowercase);
    prefix
        .chars()
        .flat_map(char::to_lowercase)
        .all(|c| text.next() == Some(c))
}

// --- Accepting ---

/// What accepting an item replaces, in the coordinates of the request's text.
#[derive(Debug, Clone, PartialEq)]
struct ItemEdit {
    /// The replaced range for Enter and for Tab; without a text edit from the server, the word
    /// at the cursor is replaced.
    insert: Option<Range<usize>>,
    replace: Option<Range<usize>>,
    text: String,
    snippet: bool,
    /// Continue the indentation of the line on the inserted lines.
    adjust_indentation: bool,
}

impl ItemEdit {
    fn new(item: &CompletionItem, request_text: &Rope) -> Self {
        let lines = Lines::new(request_text);
        let (insert, replace, text) = match &item.text_edit {
            Some(CompletionTextEdit::Edit(edit)) => {
                let range = lines.range_from_lsp(edit.range);
                (Some(range.clone()), Some(range), edit.new_text.clone())
            }
            Some(CompletionTextEdit::InsertAndReplace(edit)) => (
                Some(lines.range_from_lsp(edit.insert)),
                Some(lines.range_from_lsp(edit.replace)),
                edit.new_text.clone(),
            ),
            None => (
                None,
                None,
                item.insert_text
                    .clone()
                    .unwrap_or_else(|| item.label.clone()),
            ),
        };
        Self {
            insert,
            replace,
            text,
            snippet: item.insert_text_format == Some(InsertTextFormat::SNIPPET),
            adjust_indentation: item.insert_text_mode != Some(InsertTextMode::AS_IS),
        }
    }
}

/// Where an accepted item goes.
struct Target<'a> {
    text: &'a Rope,
    selection: &'a Selection,
    /// Start of the completed word, in current coordinates.
    word_start: usize,
    /// The text the item's edits refer to, and the changes since.
    origin: &'a Origin,
    line_ending: &'a str,
}

/// The transaction that accepts an item. The edit replaces its range around the primary cursor
/// (mapped to the current text; it always covers what was typed since the request); the other
/// empty cursors get the same completion if the same text surrounds them. Additional edits that
/// overlap a completion are dropped. The cursor goes to the first tab stop of the snippet, or after
/// the inserted text.
fn accept(
    edit: &ItemEdit,
    snippet: Option<Snippet>,
    additional: &[(Range<usize>, String)],
    mode: Mode,
    target: &Target,
) -> Transaction {
    let text = target.text;
    let primary_index = target.selection.primary_index();
    let cursor = target.selection.primary().head;
    let range = match mode {
        Mode::Insert => &edit.insert,
        Mode::Replace => &edit.replace,
    };
    let (start, end) = match range {
        Some(range) => (
            target.origin.map(range.start, Assoc::Before),
            target.origin.map(range.end, Assoc::After),
        ),
        None => (
            target.word_start,
            match mode {
                Mode::Insert => cursor,
                Mode::Replace => word_end(text, cursor),
            },
        ),
    };
    let (start, end) = (start.min(cursor), end.max(cursor));

    let (body, tabstops) = match snippet {
        Some(snippet) => (snippet.text, snippet.tabstops),
        None => (edit.text.clone(), Vec::new()),
    };
    let indent = if edit.adjust_indentation {
        indentation(text, text.char_to_line(start))
    } else {
        String::new()
    };
    let (body, tabstops) = expand(&body, &tabstops, &indent, target.line_ending);

    // (range, new text, the selection range it belongs to)
    let mut edits: Vec<(Range<usize>, String, Option<usize>)> = Vec::new();
    let (before, after) = (cursor - start, end - cursor);
    let prefix = text.slice(start..cursor);
    let suffix = text.slice(cursor..end);
    for (i, range) in target.selection.ranges().iter().enumerate() {
        if i == primary_index {
            edits.push((start..end, body.clone(), Some(i)));
            continue;
        }
        let at = range.head;
        let same_word = range.is_empty()
            && at >= before
            && at + after <= text.len_chars()
            && text.slice(at - before..at) == prefix
            && text.slice(at..at + after) == suffix;
        if same_word {
            edits.push((at - before..at + after, body.clone(), Some(i)));
        }
    }
    for (range, new_text) in additional {
        let from = target.origin.map(range.start, Assoc::After);
        let to = target.origin.map(range.end, Assoc::Before).max(from);
        let (new_text, _) = expand(new_text, &[], "", target.line_ending);
        edits.push((from..to, new_text, None));
    }
    edits.sort_by_key(|(range, _, owner)| (range.start, range.end, owner.is_none()));
    let mut kept: Vec<(Range<usize>, String, Option<usize>)> = Vec::new();
    for edit in edits {
        match kept.last_mut() {
            Some(last) if edit.0.start < last.0.end => {
                // A completion wins over an additional edit; otherwise the earlier one stays.
                if last.2.is_none() && edit.2.is_some() {
                    *last = edit;
                }
            }
            _ => kept.push(edit),
        }
    }

    // Edits that change nothing are left out (accepting a word typed in full).
    let mut placed = vec![None; target.selection.len()];
    let mut changes = Vec::new();
    let mut delta = 0isize;
    for (range, new_text, owner) in &kept {
        if let Some(i) = owner {
            placed[*i] = Some(range.start.saturating_add_signed(delta));
        }
        let old_len = range.end - range.start;
        delta += new_text.chars().count() as isize - old_len as isize;
        if text.slice(range.clone()) != new_text.as_str() {
            changes.push((range.start, range.end, Some(new_text.clone())));
        }
    }
    let changes = ChangeSet::from_changes(text.len_chars(), changes);
    let body_len = body.chars().count();
    let ranges = target
        .selection
        .ranges()
        .iter()
        .zip(placed)
        .map(|(range, placed)| match placed {
            Some(at) => match tabstops.first() {
                Some(stop) => Cursor::new(at + stop.start, at + stop.end),
                None => Cursor::point(at + body_len),
            },
            None => range.map(&changes),
        })
        .collect();
    Transaction::new(changes).with_selection(Selection::new(ranges, primary_index))
}

/// Additional edits that arrived after the item was accepted, mapped to the current text; `None`
/// if nothing is left to change.
fn additional_transaction(
    text: &Rope,
    edits: &[(Range<usize>, String)],
    origin: &Origin,
    line_ending: &str,
) -> Option<Transaction> {
    let mut mapped: Vec<(usize, usize, Option<String>)> = edits
        .iter()
        .map(|(range, new_text)| {
            let from = origin.map(range.start, Assoc::After);
            let to = origin.map(range.end, Assoc::Before).max(from);
            let (new_text, _) = expand(new_text, &[], "", line_ending);
            (from, to, Some(new_text))
        })
        .collect();
    mapped.sort_by_key(|(from, to, _)| (*from, *to));
    let mut end = 0;
    mapped.retain(|(from, to, _)| {
        let keep = *from >= end;
        end = end.max(*to);
        keep
    });
    (!mapped.is_empty()).then(|| Transaction::change(text, mapped))
}

/// Text as inserted: after each line break comes `indent` (the indentation of the line the
/// completion goes into), and line breaks become the document's. Tab stops (character ranges) move
/// with the text.
fn expand(
    text: &str,
    tabstops: &[Range<usize>],
    indent: &str,
    line_ending: &str,
) -> (String, Vec<Range<usize>>) {
    if !text.contains('\n') {
        return (text.to_string(), tabstops.to_vec());
    }
    let mut expanded = String::with_capacity(text.len());
    let mut offsets = Vec::with_capacity(text.len() + 1);
    let mut position = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        offsets.push(position);
        match c {
            // A CRLF of the server's is one line break.
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                expanded.push_str(line_ending);
                expanded.push_str(indent);
                position += line_ending.chars().count() + indent.chars().count();
            }
            c => {
                expanded.push(c);
                position += 1;
            }
        }
    }
    offsets.push(position);
    let at = |i: usize| offsets[i.min(offsets.len() - 1)];
    let tabstops = tabstops
        .iter()
        .map(|stop| at(stop.start)..at(stop.end))
        .collect();
    (expanded, tabstops)
}

// --- Documentation ---

fn documentation_blocks(
    documentation: Option<Documentation>,
    language: Option<&'static flux_syntax::Language>,
    scopes: &[String],
) -> Vec<Block> {
    let mut blocks = match documentation {
        None => Vec::new(),
        Some(Documentation::String(text)) => markdown::plain(&text),
        Some(Documentation::MarkupContent(content)) => markdown::markup(&content),
    };
    markdown::highlight(&mut blocks, language, scopes);
    blocks
}

// --- Drawing ---

/// The menu under (or above) the word, with the selected item's documentation beside it; `None`
/// while there is nothing to show.
pub fn render(
    editor: &Editor,
    window: &mut Window,
    cx: &mut Context<Editor>,
) -> Option<AnyElement> {
    let session = session(editor)?;
    let list = session.list.as_ref()?;
    if session.matches.is_empty() || !editor.focus_handle.is_focused(window) {
        return None;
    }
    let anchor = popup::anchor(editor, session.word_start)?;
    // The scroll is about to change: the menu follows in the next frame.
    if editor.autoscroll.is_some() {
        window.request_animation_frame();
    }
    let theme = Theme::get(cx);
    let ui = theme.ui;
    let viewport = window.viewport_size();

    let chrome = MENU_PADDING * 2. + 2.;
    let rows = session.matches.len().min(MAX_VISIBLE_ROWS);
    let height = px(ROW_HEIGHT * rows as f32 + chrome);
    let (side, room) = popup::side(&anchor, height, viewport.height, Side::Below);
    let rows = rows.min((((room - px(chrome)) / px(ROW_HEIGHT)).floor() as usize).max(1));

    // The labels line up with the word.
    let label_offset = 1. + MENU_PADDING + ROW_PADDING + KIND_SIZE + KIND_GAP;
    let x = (anchor.x - px(label_offset))
        .min(viewport.width - px(MENU_WIDTH + WINDOW_MARGIN))
        .max(px(WINDOW_MARGIN));
    let selected = session.matches.get(session.selected)?;
    let docs = docs_panel(
        &list.items[selected.item],
        selected.item,
        room.min(px(DOCS_MAX_HEIGHT)),
        theme,
    );
    let docs_width = px(DOCS_WIDTH + POPUP_GAP);
    let docs_right = x + px(MENU_WIDTH) + docs_width <= viewport.width - px(WINDOW_MARGIN);
    let docs_left = !docs_right && x - docs_width >= px(WINDOW_MARGIN);
    let docs = docs.filter(|_| docs_right || docs_left);
    let origin_x = if docs.is_some() && docs_left {
        x - docs_width
    } else {
        x
    };

    let menu = popup::panel(ui)
        .occlude()
        .w(px(MENU_WIDTH))
        .p(px(MENU_PADDING))
        .child(
            uniform_list(
                "completions",
                session.matches.len(),
                cx.processor(|editor, range: Range<usize>, _, cx| {
                    range
                        .map(|index| render_row(editor, index, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(session.scroll.clone())
            .h(px(ROW_HEIGHT * rows as f32)),
        );
    let content = div()
        .flex()
        .gap(px(POPUP_GAP))
        .map(|row| match side {
            Side::Below => row.items_start(),
            Side::Above => row.items_end(),
        })
        .map(|row| match docs {
            Some(docs) if docs_left => row.child(docs).child(menu),
            Some(docs) => row.child(menu).child(docs),
            None => row.child(menu),
        });
    let (corner, y) = match side {
        Side::Below => (Corner::TopLeft, anchor.line_bottom + px(POPUP_GAP)),
        Side::Above => (Corner::BottomLeft, anchor.line_top - px(POPUP_GAP)),
    };
    Some(
        deferred(
            anchored()
                .anchor(corner)
                .position(point(origin_x, y))
                .snap_to_window_with_margin(px(WINDOW_MARGIN))
                .child(content),
        )
        .with_priority(2)
        .into_any_element(),
    )
}

fn render_row(editor: &mut Editor, index: usize, cx: &mut Context<Editor>) -> AnyElement {
    let ui = Theme::ui(cx);
    let Some((session, list)) = session(editor).and_then(|s| Some((s, s.list.as_ref()?))) else {
        return div().into_any_element();
    };
    let Some(m) = session.matches.get(index) else {
        return div().into_any_element();
    };
    let item = &list.items[m.item];
    let selected = index == session.selected;
    let (letter, color) = kind_style(item.lsp.kind, ui);
    let deprecated = item.deprecated();
    let inline_detail = item
        .lsp
        .label_details
        .as_ref()
        .and_then(|d| d.detail.clone());
    let description = item.description();
    let label = label_text(&item.lsp.label, &m.positions, ui);
    div()
        .w_full()
        .h(px(ROW_HEIGHT))
        .child(
            div()
                .id(index)
                .size_full()
                .px(px(ROW_PADDING))
                .flex()
                .items_center()
                .gap(px(KIND_GAP))
                .overflow_hidden()
                .rounded(px(RADIUS_SM))
                .cursor_pointer()
                .when(selected, |row| row.bg(ui.list_selected))
                .when(!selected, |row| row.hover(|style| style.bg(ui.hover)))
                .on_click(cx.listener(move |editor, _: &ClickEvent, window, cx| {
                    if let Some(session) = session_mut(editor) {
                        session.selected = index;
                    }
                    confirm(editor, Mode::Insert, window, cx)
                }))
                .child(kind_badge(letter, color))
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_baseline()
                        .whitespace_nowrap()
                        .font_family(theme::code_font())
                        .text_color(if deprecated { ui.dim } else { ui.foreground })
                        .when(deprecated, |label| label.line_through())
                        .child(label)
                        .children(
                            inline_detail.map(|detail| div().text_color(ui.dim).child(detail)),
                        ),
                )
                // Right-aligned; a description too long is cut at its end, not at its start.
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .overflow_hidden()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .children(description.map(|text| {
                            div().ml_auto().flex_none().whitespace_nowrap().child(text)
                        })),
                ),
        )
        .into_any_element()
}

/// The label with the matched characters highlighted.
fn label_text(label: &str, positions: &[usize], ui: UiColors) -> StyledText {
    let style = HighlightStyle {
        color: Some(ui.match_text),
        font_weight: Some(FontWeight::SEMIBOLD),
        ..Default::default()
    };
    StyledText::new(label.to_string()).with_highlights(
        byte_ranges(label, positions)
            .into_iter()
            .map(|r| (r, style)),
    )
}

/// The kind of an item as a letter on a tinted square: the hue groups kinds (functions violet,
/// types orange, values blue, keywords red).
fn kind_badge(letter: &'static str, color: Hsla) -> Div {
    div()
        .flex_none()
        .size(px(KIND_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(RADIUS_XS))
        .bg(UiColors::tint(color, 0.16))
        .font_family(theme::code_font())
        .text_size(px(theme::TEXT_XS))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color)
        .child(letter)
}

fn kind_style(kind: Option<CompletionItemKind>, ui: UiColors) -> (&'static str, Hsla) {
    use CompletionItemKind as Kind;
    match kind {
        Some(Kind::METHOD) => ("m", ui.violet),
        Some(Kind::FUNCTION) => ("f", ui.violet),
        Some(Kind::CONSTRUCTOR) => ("c", ui.violet),
        Some(Kind::FIELD) => ("f", ui.blue),
        Some(Kind::PROPERTY) => ("p", ui.blue),
        Some(Kind::VARIABLE) => ("v", ui.cyan),
        Some(Kind::VALUE) => ("v", ui.blue),
        Some(Kind::CONSTANT) => ("c", ui.blue),
        Some(Kind::ENUM_MEMBER) => ("e", ui.blue),
        Some(Kind::CLASS) => ("C", ui.orange),
        Some(Kind::STRUCT) => ("S", ui.orange),
        Some(Kind::ENUM) => ("E", ui.orange),
        Some(Kind::TYPE_PARAMETER) => ("T", ui.orange),
        Some(Kind::INTERFACE) => ("I", ui.teal),
        Some(Kind::MODULE) => ("M", ui.amber),
        Some(Kind::EVENT) => ("e", ui.amber),
        Some(Kind::KEYWORD) => ("k", ui.red),
        Some(Kind::OPERATOR) => ("o", ui.red),
        Some(Kind::SNIPPET) => ("s", ui.green),
        Some(Kind::COLOR) => ("c", ui.pink),
        Some(Kind::FOLDER) => ("D", ui.folder),
        Some(Kind::FILE | Kind::REFERENCE | Kind::UNIT) => ("r", ui.dim),
        _ => ("t", ui.dim),
    }
}

/// The selected item's signature and documentation; `None` if it has neither (yet).
fn docs_panel(item: &Item, index: usize, max_height: Pixels, theme: &Theme) -> Option<AnyElement> {
    let ui = theme.ui;
    let docs = item.docs.as_deref().filter(|blocks| !blocks.is_empty());
    // Above the documentation the signature helps even if the row has it; alone, only if the row
    // doesn't show it already.
    let detail = item
        .lsp
        .detail
        .as_deref()
        .filter(|detail| !detail.trim().is_empty())
        .filter(|detail| docs.is_some() || item.description().as_deref() != Some(*detail));
    if detail.is_none() && docs.is_none() {
        return None;
    }
    Some(
        popup::panel(ui)
            .occlude()
            .w(px(DOCS_WIDTH))
            .child(
                div()
                    .id(("completion-docs", index))
                    .max_h(max_height)
                    .overflow_y_scroll()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(detail.map(|detail| {
                        div()
                            .font_family(theme::code_font())
                            .text_color(ui.text_muted)
                            .child(detail.to_string())
                    }))
                    .when(detail.is_some() && docs.is_some(), |panel| {
                        panel.child(ui::divider(ui))
                    })
                    .children(docs.map(|blocks| markdown::render(blocks, ui.foreground, theme))),
            )
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_lsp::lsp_types::{InsertReplaceEdit, Position, TextEdit};

    fn item(label: &str) -> Item {
        Item {
            lsp: CompletionItem {
                label: label.to_string(),
                ..Default::default()
            },
            resolved: true,
            edit_start: None,
            docs: None,
        }
    }

    fn labels(items: &[Item], matches: &[Match]) -> Vec<String> {
        matches
            .iter()
            .map(|m| items[m.item].lsp.label.clone())
            .collect()
    }

    fn ranked(items: &[Item], query: &str) -> Vec<String> {
        let all = (0..items.len()).collect();
        labels(items, &rank(items, &[(query.to_string(), all)]))
    }

    #[test]
    fn prefix_matches_come_first_then_server_order() {
        let mut items: Vec<Item> = ["file_len", "length", "len", "LineEnding", "zzz"]
            .into_iter()
            .map(item)
            .collect();
        // The server ranks `length` above `len`.
        items[1].lsp.sort_text = Some("0".into());
        items[2].lsp.sort_text = Some("1".into());
        let le = ranked(&items, "le");
        assert_eq!(le[..2], ["length", "len"]);
        // The rest by score; "zzz" doesn't match.
        assert_eq!(le.len(), 4);
        assert!(le.contains(&"LineEnding".to_string()) && le.contains(&"file_len".to_string()));
        // Without a query: all of them, in the server's order (sortText, then label).
        assert_eq!(
            ranked(&items, ""),
            ["length", "len", "LineEnding", "file_len", "zzz"]
        );
    }

    #[test]
    fn filter_text_filters_and_label_positions_highlight() {
        let mut items = vec![item("push(…)"), item("pop()")];
        items[0].lsp.filter_text = Some("push".into());
        let all = vec![0, 1];
        let matches = rank(&items, &[("pu".to_string(), all)]);
        assert_eq!(labels(&items, &matches), ["push(…)"]);
        assert_eq!(matches[0].positions, [0, 1]);
        // The filter text is not the label's start: positions come from the label.
        let mut items = vec![item("self.value")];
        items[0].lsp.filter_text = Some("value".into());
        let matches = rank(&items, &[("val".to_string(), vec![0])]);
        assert_eq!(matches[0].positions, [5, 6, 7]);
    }

    #[test]
    fn queries_with_nucleo_syntax_are_literal() {
        let items = vec![item("$el"), item("el$"), item("element")];
        assert_eq!(escape_query("el$"), "el\\$");
        assert_eq!(escape_query("^a"), "\\^a");
        assert!(ranked(&items, "el$").contains(&"el$".to_string()));
        assert_eq!(ranked(&items, "$e"), ["$el"]);
    }

    #[test]
    fn only_edits_at_the_cursors_keep_the_session() {
        let selection = Selection::new(vec![Cursor::point(3), Cursor::point(9)], 1);
        let edit = |changes: Vec<(usize, usize, Option<&str>)>| {
            let changes = changes
                .into_iter()
                .map(|(from, to, text)| (from, to, text.map(str::to_string)));
            ChangeSet::from_changes(12, changes)
        };
        // Typing at both cursors, Backspace, Delete.
        assert!(at_cursors(
            &edit(vec![(3, 3, Some("x")), (9, 9, Some("x"))]),
            &selection
        ));
        assert!(at_cursors(&edit(vec![(8, 9, None)]), &selection));
        assert!(at_cursors(&edit(vec![(9, 10, None)]), &selection));
        // An undo that restores text elsewhere.
        assert!(!at_cursors(&edit(vec![(5, 5, Some("y"))]), &selection));
        assert!(!at_cursors(&edit(vec![(0, 2, None)]), &selection));
    }

    #[test]
    fn words_and_queries() {
        let text = Rope::from_str("let foo_bar = x.ba\n");
        assert_eq!(word_start(&text, 11), 4);
        assert_eq!(word_start(&text, 18), 16);
        assert_eq!(word_end(&text, 5), 11);
        let at = |pos| Selection::point(pos);
        // Still in the word started at 16 ("ba" after the dot).
        assert_eq!(query_at(&text, &at(18), 16, Some('.')), Some("ba".into()));
        // Before the word start, past a non-word character, or the dot is gone: the session ends.
        assert_eq!(query_at(&text, &at(15), 16, Some('.')), None);
        assert_eq!(query_at(&text, &at(19), 16, Some('.')), None);
        assert_eq!(query_at(&text, &at(18), 16, Some(':')), None);
        assert_eq!(
            query_at(&text, &Selection::single(16, 18), 16, Some('.')),
            None
        );
        assert!(starts_with_ignoring_case("LineEnding", "lin"));
        assert!(!starts_with_ignoring_case("le", "len"));
    }

    fn target<'a>(
        text: &'a Rope,
        selection: &'a Selection,
        word_start: usize,
        origin: &'a Origin,
    ) -> Target<'a> {
        Target {
            text,
            selection,
            word_start,
            origin,
            line_ending: "\n",
        }
    }

    /// Applies a transaction: the text and the selection ranges after it.
    fn applied(text: &Rope, tx: Transaction) -> (String, Vec<(usize, usize)>) {
        let mut text = text.clone();
        tx.changes.apply(&mut text);
        let selection = tx.selection.unwrap();
        let ranges = selection.iter().map(|r| (r.anchor, r.head)).collect();
        (text.to_string(), ranges)
    }

    fn text_edit(start: u32, end: u32, new_text: &str) -> CompletionTextEdit {
        CompletionTextEdit::Edit(TextEdit {
            range: flux_lsp::lsp_types::Range::new(Position::new(0, start), Position::new(0, end)),
            new_text: new_text.to_string(),
        })
    }

    #[test]
    fn accepting_replaces_what_was_typed_after_the_request() {
        // Requested at "v.pu|"; then "s" was typed: "v.pus|".
        let request = Rope::from_str("v.pu");
        let typed = ChangeSet::from_changes(4, [(4, 4, Some("s".to_string()))]);
        let origin = Origin {
            text: request.clone(),
            changes: vec![typed],
        };
        let text = Rope::from_str("v.pus");
        let selection = Selection::point(5);
        let mut lsp = CompletionItem {
            label: "push".into(),
            text_edit: Some(text_edit(2, 4, "push($1)$0")),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            ..Default::default()
        };
        let edit = ItemEdit::new(&lsp, &request);
        assert_eq!(edit.insert, Some(2..4));
        let snippet = Snippet {
            text: "push()".into(),
            tabstops: vec![5..5, 6..6],
        };
        let tx = accept(
            &edit,
            Some(snippet),
            &[],
            Mode::Insert,
            &target(&text, &selection, 2, &origin),
        );
        assert_eq!(applied(&text, tx), ("v.push()".into(), vec![(7, 7)]));

        // A plain item without an edit replaces the word up to the cursor (Enter) or the whole
        // word (Tab).
        lsp.text_edit = None;
        lsp.insert_text_format = None;
        lsp.insert_text = Some("pushed".into());
        let edit = ItemEdit::new(&lsp, &request);
        let text = Rope::from_str("v.pus_rest;");
        let tx = accept(
            &edit,
            None,
            &[],
            Mode::Insert,
            &target(&text, &selection, 2, &origin),
        );
        assert_eq!(applied(&text, tx).0, "v.pushed_rest;");
        let tx = accept(
            &edit,
            None,
            &[],
            Mode::Replace,
            &target(&text, &selection, 2, &origin),
        );
        assert_eq!(applied(&text, tx), ("v.pushed;".into(), vec![(8, 8)]));
    }

    #[test]
    fn insert_and_replace_ranges_follow_the_key() {
        let origin = Origin::new(Rope::from_str("fobar"));
        let lsp = CompletionItem {
            label: "foo".into(),
            text_edit: Some(CompletionTextEdit::InsertAndReplace(InsertReplaceEdit {
                new_text: "foo".into(),
                insert: flux_lsp::lsp_types::Range::new(Position::new(0, 0), Position::new(0, 2)),
                replace: flux_lsp::lsp_types::Range::new(Position::new(0, 0), Position::new(0, 5)),
            })),
            ..Default::default()
        };
        let text = Rope::from_str("fobar");
        let edit = ItemEdit::new(&lsp, &text);
        let selection = Selection::point(2);
        let insert = accept(
            &edit,
            None,
            &[],
            Mode::Insert,
            &target(&text, &selection, 0, &origin),
        );
        assert_eq!(applied(&text, insert).0, "foobar");
        let replace = accept(
            &edit,
            None,
            &[],
            Mode::Replace,
            &target(&text, &selection, 0, &origin),
        );
        assert_eq!(applied(&text, replace).0, "foo");
    }

    #[test]
    fn additional_edits_go_in_the_same_step_and_overlaps_are_dropped() {
        let text = Rope::from_str("fn main() {\n    HashM\n}\n");
        let origin = Origin::new(text.clone());
        let selection = Selection::point(21);
        let lsp = CompletionItem {
            label: "HashMap".into(),
            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                range: flux_lsp::lsp_types::Range::new(Position::new(1, 4), Position::new(1, 9)),
                new_text: "HashMap".into(),
            })),
            ..Default::default()
        };
        let edit = ItemEdit::new(&lsp, &text);
        let import = (0..0, "use std::collections::HashMap;\n".to_string());
        // Overlaps the completion: dropped.
        let bad = (18..20, "x".to_string());
        let tx = accept(
            &edit,
            None,
            &[import, bad],
            Mode::Insert,
            &target(&text, &selection, 16, &origin),
        );
        let (result, ranges) = applied(&text, tx);
        assert_eq!(
            result,
            "use std::collections::HashMap;\nfn main() {\n    HashMap\n}\n"
        );
        assert_eq!(ranges, [(54, 54)]);
    }

    #[test]
    fn other_cursors_with_the_same_word_get_the_completion() {
        // Three cursors after "ve"; the middle one has "xe" before it.
        let text = Rope::from_str("ve\nxe\nve\n");
        let origin = Origin::new(text.clone());
        let selection = Selection::new(
            vec![Cursor::point(2), Cursor::point(5), Cursor::point(8)],
            2,
        );
        let lsp = CompletionItem {
            label: "vec".into(),
            ..Default::default()
        };
        let edit = ItemEdit::new(&lsp, &text);
        let tx = accept(
            &edit,
            None,
            &[],
            Mode::Insert,
            &target(&text, &selection, 6, &origin),
        );
        let (result, ranges) = applied(&text, tx);
        assert_eq!(result, "vec\nxe\nvec\n");
        assert_eq!(ranges, [(3, 3), (6, 6), (10, 10)]);
    }

    #[test]
    fn multiline_snippets_continue_the_indentation() {
        let (text, stops) = expand("if x {\n\t$0\n}", &[8..8, 9..10], "    ", "\n");
        assert_eq!(text, "if x {\n    \t$0\n    }");
        assert_eq!(stops, [12..12, 13..14]);
        let (text, stops) = expand("a\r\nb", &[0..1, 3..4], "", "\r\n");
        assert_eq!(text, "a\r\nb");
        assert_eq!(stops, [0..1, 3..4]);
        // Without line breaks, nothing changes.
        let stops = [0..1, 1..2];
        assert_eq!(
            expand("abc", &stops, "  ", "\r\n"),
            ("abc".into(), stops.to_vec())
        );
    }

    #[test]
    fn accepting_a_word_typed_in_full_changes_nothing() {
        let text = Rope::from_str("len");
        let origin = Origin::new(text.clone());
        let selection = Selection::point(3);
        let lsp = CompletionItem {
            label: "len".into(),
            ..Default::default()
        };
        let edit = ItemEdit::new(&lsp, &text);
        let tx = accept(
            &edit,
            None,
            &[],
            Mode::Insert,
            &target(&text, &selection, 0, &origin),
        );
        assert!(tx.changes.is_empty());
        assert_eq!(tx.selection, Some(Selection::point(3)));
    }

    #[test]
    fn late_additional_edits_follow_the_text() {
        // The import arrives after "HashMap" was accepted and "::" typed: positions before the
        // cursor are unaffected, edits at the end of the document move with it.
        let origin_text = Rope::from_str("x\nHashM");
        let mut origin = Origin::new(origin_text.clone());
        origin.changes.push(ChangeSet::from_changes(
            7,
            [(2, 7, Some("HashMap::".to_string()))],
        ));
        let text = Rope::from_str("x\nHashMap::");
        let edits = vec![
            (0..0, "use std::collections::HashMap;\n".to_string()),
            (7..7, "\n// end".to_string()),
        ];
        let tx = additional_transaction(&text, &edits, &origin, "\n").unwrap();
        let mut result = text.clone();
        tx.changes.apply(&mut result);
        assert_eq!(
            result.to_string(),
            "use std::collections::HashMap;\nx\nHashMap::\n// end"
        );
    }

    #[test]
    fn kinds_have_letters_and_colors() {
        let ui = Theme::flux_night().ui;
        assert_eq!(
            kind_style(Some(CompletionItemKind::METHOD), ui),
            ("m", ui.violet)
        );
        assert_eq!(
            kind_style(Some(CompletionItemKind::STRUCT), ui),
            ("S", ui.orange)
        );
        assert_eq!(kind_style(None, ui), ("t", ui.dim));
    }
}
