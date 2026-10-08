//! Navigation and refactoring through the language server: go to definition (cmd-b, cmd-click),
//! find usages (alt-f7), reformat (cmd-alt-l), rename (shift-f6), and back / forward along the
//! jumps (cmd-[ / cmd-]). Handled at the Workspace level: results may be in other files.
//!
//! Keys follow the JetBrains macOS keymap: cmd-b on a definition itself shows its usages, as
//! there.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use flux_core::Rope;
use flux_core::movement;
use flux_core::text::{CharClass, char_class, line_start};
use flux_lsp::lsp_types::request::{
    Formatting, GotoDefinition, PrepareRenameRequest, RangeFormatting, References,
};
use flux_lsp::lsp_types::{
    DocumentFormattingParams, DocumentRangeFormattingParams, FormattingOptions,
    GotoDefinitionParams, OneOf, Position, PrepareRenameResponse, ReferenceContext,
    ReferenceParams, RenameOptions, TextDocumentIdentifier, TextDocumentPositionParams, TextEdit,
    Uri,
};
use flux_lsp::{LanguageServer, RequestError, position};
use futures::future::Either;
use gpui::{
    App, AsyncWindowContext, Context, Div, Entity, KeyBinding, MouseDownEvent, Task, WeakEntity,
    Window, actions, prelude::*,
};

use crate::editor::Editor;
use crate::i18n::{tr, trf};
use crate::locations::{self, ListKind, NavTarget};
use crate::rename;
use crate::theme;
use crate::workspace::Workspace;

actions!(
    navigation,
    [
        GoToDefinition,
        FindUsages,
        ReformatCode,
        RenameSymbol,
        NavigateBack,
        NavigateForward,
    ]
);

/// How many places Back remembers.
const HISTORY_LIMIT: usize = 100;

/// The JSON-RPC error of a method the server doesn't have.
const METHOD_NOT_FOUND: i64 = -32601;

pub fn init(cx: &mut App) {
    let editor = Some("Editor");
    let workspace = Some("Workspace");
    cx.bind_keys([
        KeyBinding::new("cmd-b", GoToDefinition, editor),
        KeyBinding::new("alt-f7", FindUsages, editor),
        KeyBinding::new("cmd-alt-l", ReformatCode, editor),
        KeyBinding::new("shift-f6", RenameSymbol, editor),
        KeyBinding::new("cmd-[", NavigateBack, workspace),
        KeyBinding::new("cmd-]", NavigateForward, workspace),
        KeyBinding::new("cmd-alt-left", NavigateBack, workspace),
        KeyBinding::new("cmd-alt-right", NavigateForward, workspace),
    ]);
    rename::init(cx);
}

/// Registers the workspace actions of this module.
pub fn actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(cx.listener(go_to_definition))
        .on_action(cx.listener(find_usages))
        .on_action(cx.listener(reformat))
        .on_action(cx.listener(rename_symbol))
        .on_action(cx.listener(|this, _: &NavigateBack, window, cx| {
            let current = current_place(this, cx);
            if let Some(place) = this.navigation.back(current) {
                go_to_place(this, place, window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &NavigateForward, window, cx| {
            let current = current_place(this, cx);
            if let Some(place) = this.navigation.forward(current) {
                go_to_place(this, place, window, cx);
            }
        }))
}

/// From `Editor::on_mouse_down`: cmd-click on a word goes to its definition. `true` if handled.
pub fn cmd_click(
    editor: &mut Editor,
    event: &MouseDownEvent,
    window: &mut Window,
    cx: &mut Context<Editor>,
) -> bool {
    let modifiers = event.modifiers;
    if !modifiers.platform
        || modifiers.shift
        || modifiers.alt
        || modifiers.control
        || event.click_count != 1
    {
        return false;
    }
    let text = editor.document.text();
    let Some(position) = editor
        .layout
        .as_ref()
        .map(|layout| layout.position_for_point(text, event.position))
    else {
        return false;
    };
    // Away from words (spaces, punctuation, past the end of a line) it is an ordinary click.
    let Some(word) = word_at(text, position) else {
        return false;
    };
    // Right after the last letter counts as on the word; the request goes from inside it.
    let position = position.min(word.end - 1);
    editor.select_range(position..position, cx);
    window.dispatch_action(Box::new(GoToDefinition), cx);
    true
}

/// A place in a file: Back and Forward return to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Place {
    pub path: PathBuf,
    /// Character index; clamped to the document when going there.
    pub position: usize,
}

/// Places for Back and Forward, and the request in flight.
#[derive(Default)]
pub struct NavState {
    back: Vec<Place>,
    forward: Vec<Place>,
    /// The latest request: a new one replaces it, and the dropped task drops the server request
    /// (which cancels it on the server).
    pending: Option<Task<()>>,
}

impl NavState {
    /// A jump from `from`: Back will return there; the forward branch is forgotten.
    pub(crate) fn record(&mut self, from: Place) {
        if self.back.last() != Some(&from) {
            self.back.push(from);
            if self.back.len() > HISTORY_LIMIT {
                self.back.remove(0);
            }
        }
        self.forward.clear();
    }

    /// The place to go back to; `current` goes to the forward list.
    fn back(&mut self, current: Option<Place>) -> Option<Place> {
        let place = self.back.pop()?;
        self.forward.extend(current);
        Some(place)
    }

    fn forward(&mut self, current: Option<Place>) -> Option<Place> {
        let place = self.forward.pop()?;
        self.back.extend(current);
        Some(place)
    }
}

/// The cursor of the active document, if it has a file.
fn current_place(workspace: &Workspace, cx: &App) -> Option<Place> {
    let editor = workspace.active_editor()?;
    let document = &editor.read(cx).document;
    Some(Place {
        path: document.path()?.to_path_buf(),
        position: document.selection().primary().head,
    })
}

/// Opens a place from the history, cursor at its position.
fn go_to_place(
    workspace: &mut Workspace,
    place: Place,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    workspace.open_and(place.path, true, window, cx, move |workspace, cx| {
        if let Some(editor) = workspace.active_editor() {
            editor.update(cx, |editor, cx| {
                let position = place.position.min(editor.document.text().len_chars());
                editor.select_range(position..position, cx);
            });
        }
    });
}

/// Jumps to a place the server named (`Back` returns to `origin`): opens the file, cursor at the
/// start of the range, the line in view.
pub(crate) fn jump(
    workspace: &mut Workspace,
    target: NavTarget,
    origin: Option<Place>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(origin) = origin {
        workspace.navigation.record(origin);
    }
    let range = target.range;
    workspace.open_and(target.path, true, window, cx, move |workspace, cx| {
        if let Some(editor) = workspace.active_editor() {
            editor.update(cx, |editor, cx| {
                let start = position::from_lsp(editor.document.text(), range.start);
                editor.select_range(start..start, cx);
            });
        }
    });
}

/// What a request about the active document needs.
pub(crate) struct Request {
    pub editor: Entity<Editor>,
    pub server: LanguageServer,
    pub uri: Uri,
    /// The primary cursor, as a character index and as an LSP position.
    pub cursor: usize,
    pub position: Position,
}

impl Request {
    /// The active document on its language server; otherwise the status bar says why not.
    fn active(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> Option<Self> {
        let editor = workspace.active_editor()?;
        let found = {
            let state = editor.read(cx);
            match crate::lsp::document_server(state) {
                Some((server, uri)) => {
                    let text = state.document.text();
                    let cursor = state.document.selection().primary().head;
                    Ok((server, uri, cursor, position::to_lsp(text, cursor)))
                }
                None => Err(workspace
                    .lsp
                    .read(cx)
                    .waiting(editor.entity_id())
                    .unwrap_or_else(|| tr("No language server for this file").to_string())),
            }
        };
        match found {
            Ok((server, uri, cursor, position)) => Some(Self {
                editor,
                server,
                uri,
                cursor,
                position,
            }),
            Err(message) => {
                workspace.show_message(message.into(), cx);
                None
            }
        }
    }

    pub fn position_params(&self) -> TextDocumentPositionParams {
        TextDocumentPositionParams {
            text_document: self.document(),
            position: self.position,
        }
    }

    fn document(&self) -> TextDocumentIdentifier {
        TextDocumentIdentifier {
            uri: self.uri.clone(),
        }
    }
}

/// An error for the status bar: "Find usages failed: …". `None` when there is nothing to say: the
/// answer is outdated (the request was replaced by a newer one, or the text changed meanwhile).
pub(crate) fn error_message(
    template: &str,
    error: &RequestError,
    server: &LanguageServer,
) -> Option<String> {
    if error.is_outdated() {
        return None;
    }
    let reason = match error {
        RequestError::Server { code, .. } if *code == METHOD_NOT_FOUND => {
            trf("{0} doesn't support this", &[&server.name()])
        }
        RequestError::Server { message, .. } => message.clone(),
        RequestError::Exited => trf("{0} has stopped", &[&server.name()]),
        RequestError::Decode(detail) => trf("unexpected response: {0}", &[detail]),
        RequestError::NotReady => tr("the language server is starting").to_string(),
    };
    Some(trf(template, &[&reason]))
}

fn report(
    workspace: &mut Workspace,
    template: &str,
    error: &RequestError,
    server: &LanguageServer,
    cx: &mut Context<Workspace>,
) {
    if let Some(message) = error_message(template, error, server) {
        workspace.show_message(message.into(), cx);
    }
}

/// The word under the cursor, for titles ("Filter usages of foo…").
fn symbol_at(editor: &Editor, position: usize) -> Option<String> {
    let text = editor.document.text();
    word_at(text, position).map(|word| text.slice(word).to_string())
}

/// The word at a position: the one under it or, if there is none, the one ending right there
/// (`name|(` — where the cursor is after typing a name).
pub(crate) fn word_at(text: &Rope, position: usize) -> Option<Range<usize>> {
    let is_word = |at: usize| at < text.len_chars() && char_class(text.char(at)) == CharClass::Word;
    let probe = if is_word(position) {
        position
    } else if position > 0 && is_word(position - 1) {
        position - 1
    } else {
        return None;
    };
    let word = movement::word_range_at(text, probe);
    Some(word.from()..word.to())
}

// --- Go to definition and find usages ---

fn go_to_definition(
    workspace: &mut Workspace,
    _: &GoToDefinition,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(request) = Request::active(workspace, cx) else {
        return;
    };
    let params = GotoDefinitionParams {
        text_document_position_params: request.position_params(),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let response = request.server.request::<GotoDefinition>(params);
    let task = cx.spawn_in(window, async move |this, cx| {
        let result = response.await;
        let targets = match result {
            Ok(response) => locations::from_definition(response),
            Err(error) => {
                this.update(cx, |this, cx| {
                    report(
                        this,
                        "Go to definition failed: {0}",
                        &error,
                        &request.server,
                        cx,
                    )
                })
                .ok();
                return;
            }
        };
        let on_itself = this
            .update(cx, |this, cx| {
                targets.len() == 1 && is_at_cursor(this, &request, &targets[0], cx)
            })
            .unwrap_or(false);
        // On the definition itself, cmd-b shows its usages, as in JetBrains IDEs.
        if on_itself {
            return usages(this, request, cx).await;
        }
        match targets.len() {
            0 => {
                this.update(cx, |this, cx| {
                    this.show_message(tr("No definition found").into(), cx)
                })
                .ok();
            }
            1 => {
                let target = targets.into_iter().next().expect("one target");
                this.update_in(cx, |this, window, cx| {
                    let origin = current_place(this, cx);
                    jump(this, target, origin, window, cx);
                })
                .ok();
            }
            _ => show_list(this, ListKind::Definitions, targets, cx).await,
        }
    });
    workspace.navigation.pending = Some(task);
}

/// The target is where the cursor already is: the same file, the cursor within its range.
fn is_at_cursor(workspace: &Workspace, request: &Request, target: &NavTarget, cx: &App) -> bool {
    let editor = request.editor.read(cx);
    let same_file = editor
        .document
        .path()
        .is_some_and(|path| canonical(path) == canonical(&target.path));
    if !same_file || workspace.active_editor().as_ref() != Some(&request.editor) {
        return false;
    }
    let range = position::range_from_lsp(editor.document.text(), target.range);
    range.contains(&request.cursor) || range.end == request.cursor
}

fn find_usages(
    workspace: &mut Workspace,
    _: &FindUsages,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(request) = Request::active(workspace, cx) else {
        return;
    };
    let task = cx.spawn_in(window, async move |this, cx| {
        usages(this, request, cx).await
    });
    workspace.navigation.pending = Some(task);
}

/// Asks for the usages of the symbol at the cursor and shows them in a list.
async fn usages(this: WeakEntity<Workspace>, request: Request, cx: &mut AsyncWindowContext) {
    let params = ReferenceParams {
        text_document_position: request.position_params(),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        context: ReferenceContext {
            include_declaration: true,
        },
    };
    let symbol = cx
        .update(|_, cx| symbol_at(request.editor.read(cx), request.cursor))
        .ok()
        .flatten();
    let result = request.server.request::<References>(params).await;
    let targets = match result {
        Ok(locations) => locations::from_locations(locations.unwrap_or_default()),
        Err(error) => {
            this.update(cx, |this, cx| {
                report(this, "Find usages failed: {0}", &error, &request.server, cx)
            })
            .ok();
            return;
        }
    };
    if targets.is_empty() {
        this.update(cx, |this, cx| {
            this.show_message(tr("No usages found").into(), cx)
        })
        .ok();
        return;
    }
    show_list(this, ListKind::Usages { symbol }, targets, cx).await;
}

/// Reads the lines of the places in the background (open documents as they are in the editor,
/// other files from disk), then shows the list.
async fn show_list(
    this: WeakEntity<Workspace>,
    kind: ListKind,
    targets: Vec<NavTarget>,
    cx: &mut AsyncWindowContext,
) {
    let Ok((texts, root, origin)) = this.update(cx, |this, cx| {
        let texts = this
            .editors(cx)
            .iter()
            .filter_map(|editor| {
                let document = &editor.read(cx).document;
                Some((canonical(document.path()?), document.text().clone()))
            })
            .collect();
        let root = this.root().map(Path::to_path_buf);
        (texts, root, current_place(this, cx))
    }) else {
        return;
    };
    let first = origin.as_ref().map(|place| canonical(&place.path));
    let rows = cx
        .background_spawn(async move {
            locations::rows(targets, &texts, root.as_deref(), first.as_deref())
        })
        .await;
    this.update_in(cx, |this, window, cx| {
        locations::open(this, kind, rows, origin, window, cx)
    })
    .ok();
}

// --- Reformat ---

fn reformat(
    workspace: &mut Workspace,
    _: &ReformatCode,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(mut request) = Request::active(workspace, cx) else {
        return;
    };
    // Formatting may be another server's job (Python: ruff next to pyright).
    if let Some((server, uri)) = crate::lsp::document_server_with(request.editor.read(cx), |caps| {
        supported(&caps.document_formatting_provider)
            || supported(&caps.document_range_formatting_provider)
    }) {
        request.server = server;
        request.uri = uri;
    }
    let options = FormattingOptions {
        tab_size: theme::TAB_WIDTH as u32,
        insert_spaces: true,
        ..Default::default()
    };
    let (selection, version) = {
        let editor = request.editor.read(cx);
        let primary = editor.document.selection().primary();
        let selection = (!primary.is_empty())
            .then(|| position::range_to_lsp(editor.document.text(), primary.from()..primary.to()));
        (selection, crate::lsp::document_version(editor))
    };
    let can_format_range = request
        .server
        .capabilities()
        .is_some_and(|capabilities| supported(&capabilities.document_range_formatting_provider));
    // With a selection, only the selection is formatted, if the server can do that.
    let response = match selection {
        Some(range) if can_format_range => Either::Left(request.server.request::<RangeFormatting>(
            DocumentRangeFormattingParams {
                text_document: request.document(),
                range,
                options,
                work_done_progress_params: Default::default(),
            },
        )),
        _ => Either::Right(
            request
                .server
                .request::<Formatting>(DocumentFormattingParams {
                    text_document: request.document(),
                    options,
                    work_done_progress_params: Default::default(),
                }),
        ),
    };
    let task = cx.spawn_in(window, async move |this, cx| {
        let result = response.await;
        this.update(cx, |this, cx| match result {
            Ok(edits) => request.editor.update(cx, |editor, cx| {
                // The edits are for the text the request was made for.
                if crate::lsp::document_version(editor) != version {
                    let message = tr("The file changed while reformatting — try again");
                    return editor.show_status(message.into(), cx);
                }
                let message = if apply_formatting(editor, &edits.unwrap_or_default(), cx) {
                    tr("Reformatted")
                } else {
                    tr("Already formatted")
                };
                editor.show_status(message.into(), cx);
            }),
            Err(error) => report(this, "Reformat failed: {0}", &error, &request.server, cx),
        })
        .ok();
    });
    workspace.navigation.pending = Some(task);
}

/// Server edits of a document as ours: character ranges, and new text with the document's line
/// endings.
pub(crate) fn server_edits(
    text: &Rope,
    edits: &[TextEdit],
    line_ending: &str,
) -> Vec<(Range<usize>, String)> {
    flux_lsp::edit::from_lsp(text, edits)
        .into_iter()
        .map(|(range, new)| (range, with_line_ending(&new, line_ending)))
        .collect()
}

/// Servers send `\n` (some `\r\n`) whatever the file uses: a CRLF document gets `\r\n`.
pub(crate) fn with_line_ending(text: &str, line_ending: &str) -> String {
    let text = text.replace("\r\n", "\n");
    match line_ending {
        "\n" => text,
        ending => text.replace('\n', ending),
    }
}

/// A capability given as `true` or as options.
fn supported<T>(capability: &Option<OneOf<bool, T>>) -> bool {
    matches!(capability, Some(OneOf::Left(true) | OneOf::Right(_)))
}

/// Applies formatting edits as one undo step; `false` if nothing changes. The cursor follows its
/// text; if the server replaced the whole document, it keeps its line and column.
fn apply_formatting(editor: &mut Editor, edits: &[TextEdit], cx: &mut Context<Editor>) -> bool {
    let text = editor.document.text();
    let mut changes = server_edits(text, edits, editor.document.line_ending());
    // Some servers send edits that change nothing: they must not mark the file modified.
    changes.retain(|(range, new)| text.slice(range.clone()) != new.as_str());
    if changes.is_empty() {
        return false;
    }
    let whole = changes.len() == 1 && changes[0].0 == (0..text.len_chars());
    let head = editor.document.selection().primary().head;
    let line = text.char_to_line(head);
    let column = head - line_start(text, line);
    editor.replace_ranges(changes, cx);
    if whole {
        let position = editor.position(line, column);
        editor.select_range(position..position, cx);
    }
    true
}

// --- Rename ---

fn rename_symbol(
    workspace: &mut Workspace,
    _: &RenameSymbol,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(request) = Request::active(workspace, cx) else {
        return;
    };
    let prepare = request.server.capabilities().is_some_and(|capabilities| {
        matches!(
            capabilities.rename_provider,
            Some(OneOf::Right(RenameOptions {
                prepare_provider: Some(true),
                ..
            }))
        )
    });
    if !prepare {
        return rename::open(workspace, request, None, None, window, cx);
    }
    // The server says what can be renamed and where its name is.
    let response = request
        .server
        .request::<PrepareRenameRequest>(request.position_params());
    let task = cx.spawn_in(window, async move |this, cx| {
        let result = response.await;
        this.update_in(cx, |this, window, cx| match result {
            Ok(Some(PrepareRenameResponse::Range(range))) => {
                rename::open(this, request, Some(range), None, window, cx)
            }
            Ok(Some(PrepareRenameResponse::RangeWithPlaceholder { range, placeholder })) => {
                rename::open(this, request, Some(range), Some(placeholder), window, cx)
            }
            Ok(Some(PrepareRenameResponse::DefaultBehavior { .. })) => {
                rename::open(this, request, None, None, window, cx)
            }
            Ok(None) => this.show_message(tr("This symbol can't be renamed").into(), cx),
            Err(error) => report(this, "Can't rename: {0}", &error, &request.server, cx),
        })
        .ok();
    });
    workspace.navigation.pending = Some(task);
}

/// Path for comparing files: canonical, or as is if it can't be resolved.
pub(crate) fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(path: &str, position: usize) -> Place {
        Place {
            path: PathBuf::from(path),
            position,
        }
    }

    #[test]
    fn back_and_forward_walk_the_jumps() {
        let mut nav = NavState::default();
        // a.rs:1 → b.rs:2 → c.rs:3
        nav.record(place("a.rs", 1));
        nav.record(place("b.rs", 2));
        assert_eq!(nav.back(Some(place("c.rs", 3))), Some(place("b.rs", 2)));
        assert_eq!(nav.back(Some(place("b.rs", 2))), Some(place("a.rs", 1)));
        assert_eq!(nav.back(Some(place("a.rs", 1))), None);
        assert_eq!(nav.forward(Some(place("a.rs", 1))), Some(place("b.rs", 2)));
        assert_eq!(nav.forward(Some(place("b.rs", 2))), Some(place("c.rs", 3)));
        assert_eq!(nav.forward(Some(place("c.rs", 3))), None);
    }

    #[test]
    fn a_new_jump_forgets_the_forward_branch() {
        let mut nav = NavState::default();
        nav.record(place("a.rs", 1));
        assert_eq!(nav.back(Some(place("b.rs", 2))), Some(place("a.rs", 1)));
        nav.record(place("a.rs", 1));
        assert_eq!(nav.forward(Some(place("c.rs", 3))), None);
        assert_eq!(nav.back(Some(place("c.rs", 3))), Some(place("a.rs", 1)));
    }

    #[test]
    fn repeated_places_are_kept_once_and_history_is_limited() {
        let mut nav = NavState::default();
        nav.record(place("a.rs", 1));
        nav.record(place("a.rs", 1));
        assert_eq!(nav.back.len(), 1);
        for i in 0..HISTORY_LIMIT + 10 {
            nav.record(place("a.rs", i + 2));
        }
        assert_eq!(nav.back.len(), HISTORY_LIMIT);
        assert_eq!(nav.back.last(), Some(&place("a.rs", HISTORY_LIMIT + 11)));
    }

    #[test]
    fn the_word_at_the_cursor_includes_one_ending_there() {
        let text = Rope::from_str("sum_up(2) + b");
        assert_eq!(word_at(&text, 0), Some(0..6));
        assert_eq!(word_at(&text, 3), Some(0..6));
        // `sum_up|(`: the word ends at the cursor.
        assert_eq!(word_at(&text, 6), Some(0..6));
        assert_eq!(word_at(&text, 7), Some(7..8));
        // `) |+`: neither under nor right before the cursor.
        assert_eq!(word_at(&text, 10), None);
        assert_eq!(word_at(&text, 12), Some(12..13));
        assert_eq!(word_at(&text, 13), Some(12..13));
        assert_eq!(word_at(&Rope::new(), 0), None);
    }

    #[test]
    fn server_text_gets_the_document_line_endings() {
        assert_eq!(with_line_ending("a\nb\n", "\r\n"), "a\r\nb\r\n");
        assert_eq!(with_line_ending("a\r\nb", "\r\n"), "a\r\nb");
        assert_eq!(with_line_ending("a\r\nb\n", "\n"), "a\nb\n");
        assert_eq!(with_line_ending("name", "\r\n"), "name");
    }

    #[test]
    fn capabilities_given_as_options_count() {
        assert!(supported::<()>(&Some(OneOf::Left(true))));
        assert!(supported(&Some(OneOf::<bool, ()>::Right(()))));
        assert!(!supported::<()>(&Some(OneOf::Left(false))));
        assert!(!supported::<()>(&None));
    }
}
