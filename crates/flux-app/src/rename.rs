//! Rename (shift-f6): a field with the symbol's name in a popover under the symbol; Enter asks the
//! language server to rename and applies its edit to every file.
//!
//! Open documents are edited through their editors (one undo step each); other files are edited
//! on disk (read, edit, atomic write — as `Document::save`), and the server is told they changed.

use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};

use flux_core::{ChangeSet, Document, EditKind, Rope, Transaction};
use flux_lsp::lsp_types::notification::DidChangeWatchedFiles;
use flux_lsp::lsp_types::request::Rename;
use flux_lsp::lsp_types::{
    self, DidChangeWatchedFilesParams, FileChangeType, FileEvent, RenameParams,
    TextDocumentPositionParams, TextEdit, WorkspaceEdit,
};
use flux_lsp::{LanguageServer, position};
use gpui::{
    App, Bounds, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding,
    Pixels, Point, Render, SharedString, Subscription, Task, WeakEntity, Window, actions, div,
    point, prelude::*, px,
};

use crate::editor::Editor;
use crate::i18n::{tr, trf, trn};
use crate::icons::{IconName, icon};
use crate::input::{InputEvent, TextInput};
use crate::navigation::{Request, canonical, error_message, server_edits, word_at};
use crate::theme::{self, Theme};
use crate::ui;
use crate::workspace::Workspace;

actions!(rename, [Confirm, Dismiss]);

const WIDTH: f32 = 320.;
/// From the popover's left edge to the text in its field: the border, the padding, the field's
/// border and padding. The field is placed so that the name in it starts under the symbol.
const TEXT_INSET: f32 = 1. + 8. + 1. + 10.;
/// Gap between the symbol's line and the popover.
const OFFSET_Y: f32 = 4.;

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", Confirm, Some("RenameField")),
        KeyBinding::new("escape", Dismiss, Some("RenameField")),
    ]);
}

/// Opens the rename field for the symbol at the cursor. `range` and `placeholder` come from
/// `prepareRename` when the server supports it; otherwise the word under the cursor is renamed.
pub(crate) fn open(
    workspace: &mut Workspace,
    request: Request,
    range: Option<lsp_types::Range>,
    placeholder: Option<String>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let found = {
        let editor = request.editor.read(cx);
        let text = editor.document.text();
        let range = match range {
            Some(range) => Some(position::range_from_lsp(text, range)),
            None => word_at(text, request.cursor),
        };
        range.map(|range| {
            let name = placeholder
                .clone()
                .unwrap_or_else(|| text.slice(range.clone()).to_string());
            // Under the symbol, if it is on screen; otherwise at the top, like other popovers.
            let anchor = editor
                .layout
                .as_ref()
                .and_then(|layout| layout.bounds_for_position(text, range.start))
                .map(anchor_for);
            (name, anchor)
        })
    };
    let Some((name, anchor)) = found else {
        return workspace.show_message(tr("Place the cursor on a symbol to rename it").into(), cx);
    };
    let weak = cx.entity().downgrade();
    workspace.toggle_modal(window, cx, move |window, cx| {
        RenameField::new(request, name, weak, window, cx)
    });
    if let Some(anchor) = anchor {
        workspace.anchor_modal(anchor);
    }
}

enum State {
    Editing,
    /// The request is in flight; closing the field cancels it.
    Renaming {
        _task: Task<()>,
    },
    Failed(SharedString),
}

pub struct RenameField {
    server: LanguageServer,
    /// The document and the cursor position the rename was started at.
    position: TextDocumentPositionParams,
    old_name: String,
    input: Entity<TextInput>,
    workspace: WeakEntity<Workspace>,
    state: State,
    _subscription: Subscription,
}

impl EventEmitter<DismissEvent> for RenameField {}

impl RenameField {
    fn new(
        request: Request,
        name: String,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let mut input = TextInput::new(tr("New name"), cx).code();
            input.set_text(&name, cx);
            input.select_all(cx);
            input
        });
        // Typing clears the error of the previous attempt.
        let subscription = cx.subscribe_in(&input, window, |this, _, event, _, cx| match event {
            InputEvent::Changed => {
                if matches!(this.state, State::Failed(_)) {
                    this.state = State::Editing;
                    cx.notify();
                }
            }
        });
        Self {
            position: request.position_params(),
            server: request.server,
            old_name: name,
            input,
            workspace,
            state: State::Editing,
            _subscription: subscription,
        }
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.state, State::Renaming { .. }) {
            return;
        }
        let new_name = self.input.read(cx).text().trim().to_string();
        if new_name.is_empty() {
            self.state = State::Failed(tr("Type a new name").into());
            return cx.notify();
        }
        if new_name == self.old_name {
            return cx.emit(DismissEvent);
        }
        let response = self.server.request::<Rename>(RenameParams {
            text_document_position: self.position.clone(),
            new_name,
            work_done_progress_params: Default::default(),
        });
        let server = self.server.clone();
        let workspace = self.workspace.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let failure: SharedString = match response.await {
                Ok(Some(edit)) => {
                    let applied = workspace.update_in(cx, |workspace, window, cx| {
                        apply_workspace_edit(workspace, &edit, server.clone(), window, cx)
                    });
                    match applied {
                        Ok(Ok(())) => {
                            this.update(cx, |_, cx| cx.emit(DismissEvent)).ok();
                            return;
                        }
                        Ok(Err(error)) => error.into(),
                        Err(_) => return,
                    }
                }
                Ok(None) => tr("Nothing to rename here").into(),
                Err(error) => match error_message("{0}", &error, &server) {
                    Some(message) => message.into(),
                    None => return,
                },
            };
            this.update(cx, |this, cx| {
                this.state = State::Failed(failure);
                cx.notify();
            })
            .ok();
        });
        self.state = State::Renaming { _task: task };
        cx.notify();
    }
}

/// Applies a workspace edit: open documents through their editors (one undo step each), other
/// files on disk in the background; the status bar reports the result. `Err` — nothing was applied
/// (the edit creates, renames, or deletes files, which is not supported).
pub(crate) fn apply_workspace_edit(
    workspace: &mut Workspace,
    edit: &WorkspaceEdit,
    server: LanguageServer,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<(), String> {
    let files = group_by_file(flux_lsp::edit::workspace_files(edit)?);
    let editors: Vec<(PathBuf, Entity<Editor>)> = workspace
        .editors()
        .into_iter()
        .filter_map(|editor| {
            let path = canonical(editor.read(cx).document.path()?);
            Some((path, editor))
        })
        .collect();
    let mut in_editors = 0;
    let mut on_disk = Vec::new();
    for (path, batches) in files {
        let key = canonical(&path);
        let open = editors
            .iter()
            .find(|(open, _)| *open == key)
            .map(|(_, editor)| editor.clone());
        match open {
            Some(editor) => {
                editor.update(cx, |editor, cx| {
                    let document = &editor.document;
                    let changes = document_edits(document.text(), &batches, document.line_ending());
                    editor.replace_ranges(changes, cx);
                });
                in_editors += 1;
            }
            None => on_disk.push((path, batches)),
        }
    }
    let written = cx.background_spawn(async move {
        on_disk
            .into_iter()
            .map(|(path, batches)| {
                let result = apply_to_file(&path, &batches);
                (path, result)
            })
            .collect::<Vec<_>>()
    });
    cx.spawn_in(window, async move |this, cx| {
        let written = written.await;
        let changed: Vec<FileEvent> = written
            .iter()
            .filter(|(_, result)| result.is_ok())
            .map(|(path, _)| FileEvent {
                uri: position::uri_from_path(path),
                typ: FileChangeType::CHANGED,
            })
            .collect();
        let message = summary(in_editors + changed.len(), &written);
        // The server reads files it doesn't have open from disk: they changed under it.
        if !changed.is_empty() {
            server
                .notify::<DidChangeWatchedFiles>(DidChangeWatchedFilesParams { changes: changed });
        }
        this.update(cx, |this, cx| this.show_message(message.into(), cx))
            .ok();
    })
    .detach();
    Ok(())
}

/// A file may come several times (`documentChanges`), each batch of edits relative to the text
/// after the previous ones: the batches are grouped per file, in order.
fn group_by_file(files: Vec<(PathBuf, Vec<TextEdit>)>) -> Vec<(PathBuf, Vec<Vec<TextEdit>>)> {
    let mut grouped: Vec<(PathBuf, PathBuf, Vec<Vec<TextEdit>>)> = Vec::new();
    for (path, edits) in files {
        let key = canonical(&path);
        match grouped.iter_mut().find(|(_, other, _)| *other == key) {
            Some((_, _, batches)) => batches.push(edits),
            None => grouped.push((path, key, vec![edits])),
        }
    }
    grouped
        .into_iter()
        .map(|(path, _, batches)| (path, batches))
        .collect()
}

/// Edits of the original text that the batches amount to, with the document's line endings. One
/// batch is used as it is; several are applied in turn to a copy, and the result becomes one
/// replacement of the span that differs — still one undo step.
fn document_edits(
    text: &Rope,
    batches: &[Vec<TextEdit>],
    line_ending: &str,
) -> Vec<(Range<usize>, String)> {
    if let [batch] = batches {
        return server_edits(text, batch, line_ending);
    }
    let mut result = text.clone();
    for batch in batches {
        let edits = server_edits(&result, batch, line_ending);
        let changes = edits
            .into_iter()
            .map(|(range, new)| (range.start, range.end, Some(new)));
        ChangeSet::from_changes(result.len_chars(), changes).apply(&mut result);
    }
    difference(text, &result).into_iter().collect()
}

/// The span where `new` differs from `old` (after the common start and before the common end),
/// and what `new` has there; `None` if the texts are equal.
fn difference(old: &Rope, new: &Rope) -> Option<(Range<usize>, String)> {
    let prefix = old
        .chars()
        .zip(new.chars())
        .take_while(|(a, b)| a == b)
        .count();
    if prefix == old.len_chars() && prefix == new.len_chars() {
        return None;
    }
    let room = old.len_chars().min(new.len_chars()) - prefix;
    let suffix = old
        .chars_at(old.len_chars())
        .reversed()
        .zip(new.chars_at(new.len_chars()).reversed())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    let changed = new.slice(prefix..new.len_chars() - suffix).to_string();
    Some((prefix..old.len_chars() - suffix, changed))
}

/// Applies edit batches to a file that isn't open: read, edit, atomic write.
fn apply_to_file(path: &Path, batches: &[Vec<TextEdit>]) -> io::Result<()> {
    if !path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            tr("file not found"),
        ));
    }
    let mut document = Document::open(path)?;
    let changes = document_edits(document.text(), batches, document.line_ending())
        .into_iter()
        .map(|(range, text)| (range.start, range.end, Some(text)));
    let transaction = Transaction::change(document.text(), changes);
    document.apply(transaction, EditKind::Other);
    document.save()
}

/// "Renamed in 3 files", with the files that couldn't be written.
fn summary(changed: usize, written: &[(PathBuf, io::Result<()>)]) -> String {
    let renamed = trf(
        "Renamed {0}",
        &[&trn(changed, "in {n} file", "in {n} files")],
    );
    let failed: Vec<String> = written
        .iter()
        .filter_map(|(path, result)| {
            let error = result.as_ref().err()?;
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into(),
            );
            Some(format!("{name}: {error}"))
        })
        .collect();
    if failed.is_empty() {
        renamed
    } else {
        trf("{0}; not written: {1}", &[&renamed, &failed.join(", ")])
    }
}

impl Focusable for RenameField {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for RenameField {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let footer = match &self.state {
            State::Editing => {
                ui::hint_bar(&[("↵", tr("rename")), ("esc", tr("cancel"))], ui).into_any_element()
            }
            State::Renaming { .. } => div().child(tr("Renaming…")).into_any_element(),
            State::Failed(error) => div()
                .flex()
                .items_center()
                .gap_1p5()
                .text_color(ui.error)
                .child(icon(IconName::Warning, ui.error).size(px(13.)))
                .child(div().min_w_0().child(error.clone()))
                .into_any_element(),
        };
        ui::popover(ui)
            .key_context("RenameField")
            .w(px(WIDTH))
            .p_2()
            .flex()
            .flex_col()
            .gap_2()
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .child(self.input.clone())
            .child(
                div()
                    .px_1()
                    .pb_0p5()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(footer),
            )
    }
}

/// Where the popover goes for a symbol at `symbol` on screen (its first character, a line high):
/// under its line, so that the name in the field starts under the symbol.
fn anchor_for(symbol: Bounds<Pixels>) -> Point<Pixels> {
    point(
        symbol.left() - px(TEXT_INSET),
        symbol.bottom() + px(OFFSET_Y),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_lsp::lsp_types::Position;
    use gpui::size;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("flux-rename-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn edit(line: u32, from: u32, to: u32, text: &str) -> TextEdit {
        TextEdit::new(
            lsp_types::Range::new(Position::new(line, from), Position::new(line, to)),
            text.to_string(),
        )
    }

    #[test]
    fn edits_are_written_to_files_that_are_not_open() {
        let dir = temp_dir("write");
        let path = dir.join("lib.rs");
        std::fs::write(&path, "pub fn total() {}\n\nfn x() { total(); }\n").unwrap();
        // The server's edits all refer to the original text, in any order.
        let edits = vec![vec![edit(2, 9, 14, "sum"), edit(0, 7, 12, "sum")]];
        apply_to_file(&path, &edits).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "pub fn sum() {}\n\nfn x() { sum(); }\n"
        );
        // A missing file is an error, not a new file.
        let missing = dir.join("missing.rs");
        assert!(apply_to_file(&missing, &edits).is_err());
        assert!(!missing.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn crlf_files_keep_their_line_endings() {
        let dir = temp_dir("crlf");
        let path = dir.join("main.go");
        std::fs::write(&path, "func total() {}\r\ntotal()\r\n").unwrap();
        // The server's line breaks are `\n`: the file keeps its `\r\n`.
        let edits = vec![vec![
            edit(0, 5, 10, "sum"),
            edit(0, 14, 14, "\n\treturn\n"),
            edit(1, 0, 5, "sum"),
        ]];
        apply_to_file(&path, &edits).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "func sum() {\r\n\treturn\r\n}\r\nsum()\r\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn batches_of_one_file_apply_in_turn_as_one_edit() {
        let text = Rope::from_str("fn total() { total }\nfn other() {}\n");
        // The second batch refers to the text after the first one.
        let batches = vec![
            vec![edit(0, 3, 8, "sum"), edit(0, 13, 18, "sum")],
            vec![edit(0, 0, 0, "pub "), edit(0, 14, 14, "()")],
        ];
        let edits = document_edits(&text, &batches, "\n");
        assert_eq!(edits.len(), 1);
        let mut result = text.clone();
        let changes = edits
            .into_iter()
            .map(|(range, new)| (range.start, range.end, Some(new)));
        ChangeSet::from_changes(text.len_chars(), changes).apply(&mut result);
        assert_eq!(
            result.to_string(),
            "pub fn sum() { sum() }\nfn other() {}\n"
        );
    }

    #[test]
    fn the_difference_is_the_changed_middle() {
        let diff = |a: &str, b: &str| difference(&Rope::from_str(a), &Rope::from_str(b));
        assert_eq!(diff("abc", "abc"), None);
        assert_eq!(
            diff("a total b", "a sum b"),
            Some((2..7, "sum".to_string()))
        );
        // A repeated letter at the edge is not counted twice.
        assert_eq!(diff("aa", "aaa"), Some((2..2, "a".to_string())));
        assert_eq!(diff("abc", ""), Some((0..3, String::new())));
    }

    #[test]
    fn edits_of_one_file_are_grouped_in_order() {
        let files = vec![
            (PathBuf::from("/p/a.rs"), vec![edit(0, 0, 0, "1")]),
            (PathBuf::from("/p/b.rs"), vec![edit(0, 0, 0, "2")]),
            (PathBuf::from("/p/a.rs"), vec![edit(0, 0, 0, "3")]),
        ];
        let grouped = group_by_file(files);
        let summary: Vec<(&str, Vec<&str>)> = grouped
            .iter()
            .map(|(path, batches)| {
                let texts = batches.iter().map(|b| b[0].new_text.as_str()).collect();
                (path.to_str().unwrap(), texts)
            })
            .collect();
        assert_eq!(
            summary,
            vec![("/p/a.rs", vec!["1", "3"]), ("/p/b.rs", vec!["2"])]
        );
    }

    #[test]
    fn summary_counts_files_and_lists_failures() {
        assert_eq!(summary(1, &[]), "Renamed in 1 file");
        let written = vec![
            (PathBuf::from("/p/a.rs"), Ok(())),
            (
                PathBuf::from("/p/b.rs"),
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")),
            ),
        ];
        assert_eq!(
            summary(3, &written),
            "Renamed in 3 files; not written: b.rs: denied"
        );
    }

    #[test]
    fn the_name_in_the_field_starts_under_the_symbol() {
        let symbol = Bounds::new(point(px(200.), px(100.)), size(px(8.), px(21.)));
        let anchor = anchor_for(symbol);
        assert_eq!(anchor, point(px(200. - TEXT_INSET), px(125.)));
    }
}
