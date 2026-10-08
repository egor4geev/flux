//! Server edits (`TextEdit`, `WorkspaceEdit`) → our character-range edits.

use std::ops::Range;
use std::path::PathBuf;

use flux_core::Rope;
use lsp_types::{DocumentChangeOperation, DocumentChanges, OneOf, TextDocumentEdit, WorkspaceEdit};

use crate::position::{Lines, path_from_uri};

/// Edits for one document as `(char range, new text)`: sorted by position, non-overlapping —
/// ready for `Transaction::change`. Server edits all refer to the original text. Insertions at the
/// same place keep the server's order; an edit overlapping the previous one is cut to start where
/// that one ends (the protocol forbids overlaps). The new text is as the server sent it (line
/// breaks included).
pub fn from_lsp(text: &Rope, edits: &[lsp_types::TextEdit]) -> Vec<(Range<usize>, String)> {
    let lines = Lines::new(text);
    let mut converted: Vec<(Range<usize>, String)> = edits
        .iter()
        .map(|edit| (lines.range_from_lsp(edit.range), edit.new_text.clone()))
        .collect();
    // Stable: equal starts keep the server's order.
    converted.sort_by_key(|(range, _)| range.start);
    let mut end = 0;
    for (range, _) in &mut converted {
        range.start = range.start.max(end);
        range.end = range.end.max(range.start);
        end = range.end;
    }
    converted
}

/// File edits of a workspace edit (`documentChanges` if present, else `changes`), in a stable
/// order: `documentChanges` in their order, `changes` sorted by path. With `documentChanges`, a
/// file may appear more than once; each entry then refers to the text after the previous ones, so
/// apply them in order. Resource operations (create, rename, delete files) and non-`file` URIs are
/// not supported: `Err` with a message.
pub fn workspace_files(
    edit: &WorkspaceEdit,
) -> Result<Vec<(PathBuf, Vec<lsp_types::TextEdit>)>, String> {
    let mut files = Vec::new();
    if let Some(changes) = &edit.document_changes {
        let document_edits: Vec<&TextDocumentEdit> = match changes {
            DocumentChanges::Edits(edits) => edits.iter().collect(),
            DocumentChanges::Operations(operations) => {
                let mut edits = Vec::new();
                for operation in operations {
                    match operation {
                        DocumentChangeOperation::Edit(edit) => edits.push(edit),
                        DocumentChangeOperation::Op(_) => {
                            return Err(
                                "creating, renaming, or deleting files is not supported".into()
                            );
                        }
                    }
                }
                edits
            }
        };
        for document in document_edits {
            let path = file_path(&document.text_document.uri)?;
            let edits = document
                .edits
                .iter()
                .map(|edit| match edit {
                    OneOf::Left(edit) => edit.clone(),
                    OneOf::Right(annotated) => annotated.text_edit.clone(),
                })
                .collect();
            files.push((path, edits));
        }
    } else if let Some(changes) = &edit.changes {
        for (uri, edits) in changes {
            files.push((file_path(uri)?, edits.clone()));
        }
        files.sort_by(|(a, _), (b, _)| a.cmp(b));
    }
    Ok(files)
}

fn file_path(uri: &lsp_types::Uri) -> Result<PathBuf, String> {
    path_from_uri(uri).ok_or_else(|| format!("not a local file: {}", uri.as_str()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;

    use lsp_types::{
        AnnotatedTextEdit, CreateFile, OptionalVersionedTextDocumentIdentifier, Position,
        ResourceOp, TextEdit, Uri,
    };

    use super::*;

    fn edit(start: (u32, u32), end: (u32, u32), text: &str) -> TextEdit {
        TextEdit::new(
            lsp_types::Range::new(Position::new(start.0, start.1), Position::new(end.0, end.1)),
            text.to_string(),
        )
    }

    fn uri(s: &str) -> Uri {
        Uri::from_str(s).unwrap()
    }

    #[test]
    fn edits_are_sorted_with_inserts_in_server_order() {
        let text = Rope::from("let x = 1;\nlet y = 2;\n");
        let edits = [
            edit((1, 4), (1, 5), "z"),
            edit((0, 0), (0, 0), "a"),
            edit((0, 0), (0, 0), "b"),
            edit((0, 4), (0, 5), "w"),
        ];
        assert_eq!(
            from_lsp(&text, &edits),
            vec![
                (0..0, "a".to_string()),
                (0..0, "b".to_string()),
                (4..5, "w".to_string()),
                (15..16, "z".to_string()),
            ]
        );
    }

    #[test]
    fn overlapping_edits_are_cut() {
        let text = Rope::from("abcdef");
        let edits = [edit((0, 1), (0, 4), "X"), edit((0, 2), (0, 5), "Y")];
        assert_eq!(
            from_lsp(&text, &edits),
            vec![(1..4, "X".to_string()), (4..5, "Y".to_string())]
        );
    }

    #[test]
    fn edits_apply_as_a_transaction() {
        let text = Rope::from("fn  main(){\n}\n");
        let edits = from_lsp(
            &text,
            &[edit((0, 2), (0, 4), " "), edit((0, 10), (0, 10), " ")],
        );
        let tx = flux_core::Transaction::change(
            &text,
            edits.into_iter().map(|(r, t)| (r.start, r.end, Some(t))),
        );
        let mut result = text.clone();
        tx.changes.apply(&mut result);
        assert_eq!(result.to_string(), "fn main() {\n}\n");
    }

    #[test]
    // `Uri` caches its parse in a `Cell`; `WorkspaceEdit` keys its map by it all the same.
    #[allow(clippy::mutable_key_type)]
    fn changes_are_sorted_by_path() {
        let mut changes = HashMap::new();
        changes.insert(uri("file:///b.rs"), vec![edit((0, 0), (0, 0), "b")]);
        changes.insert(uri("file:///a%20x.rs"), vec![edit((0, 0), (0, 0), "a")]);
        let files = workspace_files(&WorkspaceEdit::new(changes)).unwrap();
        let paths: Vec<_> = files.iter().map(|(p, _)| p.clone()).collect();
        assert_eq!(paths, [PathBuf::from("/a x.rs"), PathBuf::from("/b.rs")]);
    }

    #[test]
    fn document_changes_keep_their_order_and_annotated_edits() {
        let document = |path: &str, edits| TextDocumentEdit {
            text_document: OptionalVersionedTextDocumentIdentifier {
                uri: uri(path),
                version: Some(1),
            },
            edits,
        };
        let edit = WorkspaceEdit {
            document_changes: Some(DocumentChanges::Edits(vec![
                document("file:///z.rs", vec![OneOf::Left(edit((0, 0), (0, 0), "z"))]),
                document(
                    "file:///a.rs",
                    vec![OneOf::Right(AnnotatedTextEdit {
                        text_edit: edit((0, 0), (0, 1), "a"),
                        annotation_id: "rename".into(),
                    })],
                ),
            ])),
            // Ignored when `documentChanges` are present.
            changes: Some(HashMap::from([(
                uri("file:///ignored.rs"),
                vec![edit((0, 0), (0, 0), "x")],
            )])),
            ..Default::default()
        };
        let files = workspace_files(&edit).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].0, PathBuf::from("/z.rs"));
        assert_eq!(
            files[1],
            (
                PathBuf::from("/a.rs"),
                vec![self::edit((0, 0), (0, 1), "a")]
            )
        );
    }

    #[test]
    fn resource_operations_and_remote_files_are_errors() {
        let create = WorkspaceEdit {
            document_changes: Some(DocumentChanges::Operations(vec![
                DocumentChangeOperation::Op(ResourceOp::Create(CreateFile {
                    uri: uri("file:///new.rs"),
                    options: None,
                    annotation_id: None,
                })),
            ])),
            ..Default::default()
        };
        assert!(workspace_files(&create).is_err());
        let remote = WorkspaceEdit::new(HashMap::from([(
            uri("https://example.com/a.rs"),
            vec![edit((0, 0), (0, 0), "x")],
        )]));
        assert!(workspace_files(&remote).is_err());
    }
}
