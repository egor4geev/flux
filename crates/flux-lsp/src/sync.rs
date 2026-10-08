//! Document synchronization: our edits → `textDocument/didChange` content changes, and what the
//! server wants synchronized.

use flux_core::transaction::Operation;
use flux_core::{Rope, TextChange};
use lsp_types::{
    Position, Range, ServerCapabilities, TextDocumentContentChangeEvent,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncSaveOptions,
};

use crate::position::Lines;

/// Incremental content changes for one edit, in the order the server applies them (each change's
/// range refers to the text after the previous ones). Empty for an edit that changes nothing.
///
/// A change that would start or end between the `\r` and `\n` of a CRLF — a place LSP positions
/// can't express — is widened to the whole pair.
pub fn incremental_changes(change: &TextChange) -> Vec<TextDocumentContentChangeEvent> {
    let old = &change.old_text;
    let mut new = old.clone();
    change.changes.apply(&mut new);
    // Everything before the current change is already in its final state, so its start is found
    // in the new text; its end is the start moved over the old text it removes.
    let lines = Lines::new(&new);

    let mut events = Vec::new();
    let (mut old_pos, mut new_pos) = (0, 0);
    // Characters of the next `Retain` already covered by a change widened over a CRLF.
    let mut covered = 0;
    let mut ops = change.changes.ops().iter().peekable();
    while let Some(op) = ops.next() {
        let mut inserted = String::new();
        let mut deleted = 0;
        let mut op = Some(op);
        while let Some(current) = op {
            match current {
                Operation::Retain(n) => {
                    old_pos += n - covered;
                    new_pos += n - covered;
                    covered = 0;
                    break;
                }
                Operation::Insert(text) => inserted.push_str(text),
                Operation::Delete(n) => deleted += n,
            }
            op = ops.next_if(|op| !matches!(op, Operation::Retain(_)));
        }
        if inserted.is_empty() && deleted == 0 {
            continue;
        }

        let inserted_len = inserted.chars().count();
        let follows = |pos: usize| (pos < old.len_chars()).then(|| old.char(pos));
        let mut removed = Advance::default();
        let mut text = String::new();
        // The change starts after a '\r' whose '\n' follows: start before the '\r' instead.
        let start =
            if new_pos > 0 && new.char(new_pos - 1) == '\r' && follows(old_pos) == Some('\n') {
                removed.feed("\r");
                text.push('\r');
                lines.to_lsp(new_pos - 1)
            } else if new_pos > 0 && new.char(new_pos - 1) == '\r' {
                // A lone '\r' so far, even if the new text continues it with a '\n'.
                let cr = lines.to_lsp(new_pos - 1);
                Position::new(cr.line + 1, 0)
            } else {
                lines.to_lsp(new_pos)
            };
        text.push_str(&inserted);
        for chunk in old.slice(old_pos..old_pos + deleted).chunks() {
            removed.feed(chunk);
        }
        // The removed text ends with a '\r' whose '\n' follows: take the '\n' too.
        let mut end_widened = false;
        if removed.after_cr && follows(old_pos + deleted) == Some('\n') {
            removed.feed("\n");
            text.push('\n');
            end_widened = true;
        }
        events.push(TextDocumentContentChangeEvent {
            range: Some(Range::new(start, removed.end_from(start))),
            range_length: None,
            text,
        });
        old_pos += deleted + end_widened as usize;
        new_pos += inserted_len + end_widened as usize;
        covered = end_widened as usize;
    }
    events
}

/// The whole text, for servers that sync documents in full.
pub fn full_change(text: &Rope) -> TextDocumentContentChangeEvent {
    TextDocumentContentChangeEvent {
        range: None,
        range_length: None,
        text: text.to_string(),
    }
}

/// How the server wants `didChange`: `INCREMENTAL`, `FULL`, or `NONE` (not at all; also when the
/// server says nothing).
pub fn change_kind(capabilities: &ServerCapabilities) -> TextDocumentSyncKind {
    match &capabilities.text_document_sync {
        Some(TextDocumentSyncCapability::Kind(kind)) => *kind,
        Some(TextDocumentSyncCapability::Options(options)) => {
            options.change.unwrap_or(TextDocumentSyncKind::NONE)
        }
        None => TextDocumentSyncKind::NONE,
    }
}

/// Whether the server wants `didOpen`/`didClose`. A bare sync kind implies it, as in VS Code.
pub fn open_close(capabilities: &ServerCapabilities) -> bool {
    match &capabilities.text_document_sync {
        Some(TextDocumentSyncCapability::Kind(kind)) => *kind != TextDocumentSyncKind::NONE,
        Some(TextDocumentSyncCapability::Options(options)) => options.open_close.unwrap_or(false),
        None => false,
    }
}

/// Whether the server wants `didSave`, and with the text (`Some(true)`) or without it.
pub fn save_include_text(capabilities: &ServerCapabilities) -> Option<bool> {
    match &capabilities.text_document_sync {
        Some(TextDocumentSyncCapability::Kind(kind)) => {
            (*kind != TextDocumentSyncKind::NONE).then_some(false)
        }
        Some(TextDocumentSyncCapability::Options(options)) => match &options.save {
            Some(TextDocumentSyncSaveOptions::Supported(true)) => Some(false),
            Some(TextDocumentSyncSaveOptions::SaveOptions(save)) => {
                Some(save.include_text.unwrap_or(false))
            }
            Some(TextDocumentSyncSaveOptions::Supported(false)) | None => None,
        },
        None => None,
    }
}

/// How far a text moves an LSP position: line breaks passed (`\r\n` counts once) and UTF-16 units
/// after the last one.
#[derive(Default)]
struct Advance {
    lines: u32,
    /// UTF-16 units since the last line break, or since the start if there was none.
    units: u32,
    /// The last character fed was `\r`: a `\n` right after it completes the same break.
    after_cr: bool,
}

impl Advance {
    fn feed(&mut self, text: &str) {
        let bytes = text.as_bytes();
        let mut from = 0;
        for i in memchr::memchr2_iter(b'\n', b'\r', bytes) {
            if i > from {
                self.units += utf16_len(&bytes[from..i]);
                self.after_cr = false;
            }
            if !(bytes[i] == b'\n' && self.after_cr) {
                self.lines += 1;
            }
            self.units = 0;
            self.after_cr = bytes[i] == b'\r';
            from = i + 1;
        }
        if from < bytes.len() {
            self.units += utf16_len(&bytes[from..]);
            self.after_cr = false;
        }
    }

    fn end_from(&self, start: Position) -> Position {
        if self.lines == 0 {
            Position::new(start.line, start.character + self.units)
        } else {
            Position::new(start.line + self.lines, self.units)
        }
    }
}

/// UTF-16 length of UTF-8 text: one unit per character, two for those outside the BMP (four-byte
/// sequences).
fn utf16_len(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .map(|&b| ((b & 0xC0) != 0x80) as u32 + (b >= 0xF0) as u32)
        .sum()
}

#[cfg(test)]
mod tests {
    use flux_core::ChangeSet;

    use super::*;
    use crate::position::tests::Rng;

    /// Applies content changes to a string by the LSP rules, independently of `Lines`.
    fn apply_lsp(text: &str, events: &[TextDocumentContentChangeEvent]) -> String {
        let mut text = text.to_string();
        for event in events {
            let range = event.range.expect("incremental");
            let start = offset(&text, range.start);
            let end = offset(&text, range.end);
            assert!(start <= end, "{range:?} in {text:?}");
            text.replace_range(start..end, &event.text);
        }
        text
    }

    /// Byte offset of an LSP position: lines end at `\n`, `\r\n`, `\r`; columns in UTF-16.
    fn offset(text: &str, position: Position) -> usize {
        let bytes = text.as_bytes();
        let mut line_start = 0;
        for _ in 0..position.line {
            let rest = &bytes[line_start..];
            let Some(i) = rest.iter().position(|&b| b == b'\n' || b == b'\r') else {
                panic!("line {} is past the end of {text:?}", position.line);
            };
            line_start += i + if rest[i] == b'\r' && rest.get(i + 1) == Some(&b'\n') {
                2
            } else {
                1
            };
        }
        let mut units = 0;
        for (i, c) in text[line_start..].char_indices() {
            if units == position.character as usize {
                return line_start + i;
            }
            assert!(
                c != '\n' && c != '\r',
                "{position:?} is past the line end in {text:?}"
            );
            units += c.len_utf16();
        }
        assert_eq!(
            units, position.character as usize,
            "{position:?} in {text:?}"
        );
        text.len()
    }

    fn change(old: &str, edits: Vec<(usize, usize, &str)>) -> TextChange {
        let old_text = Rope::from(old);
        let edits = edits
            .into_iter()
            .map(|(from, to, text)| (from, to, (!text.is_empty()).then(|| text.to_string())));
        TextChange {
            changes: ChangeSet::from_changes(old_text.len_chars(), edits),
            old_text,
        }
    }

    fn check(old: &str, edits: Vec<(usize, usize, &str)>) -> Vec<TextDocumentContentChangeEvent> {
        let change = change(old, edits);
        let mut new = change.old_text.clone();
        change.changes.apply(&mut new);
        let events = incremental_changes(&change);
        assert_eq!(apply_lsp(old, &events), new.to_string(), "{events:?}");
        events
    }

    fn event(start: (u32, u32), end: (u32, u32), text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: Some(Range::new(
                Position::new(start.0, start.1),
                Position::new(end.0, end.1),
            )),
            range_length: None,
            text: text.to_string(),
        }
    }

    #[test]
    fn typing_and_deleting() {
        assert_eq!(
            check("fn main() {}\n", vec![(11, 11, "\n    ")]),
            vec![event((0, 11), (0, 11), "\n    ")]
        );
        assert_eq!(
            check("ab\ncd\nef", vec![(1, 7, "")]),
            vec![event((0, 1), (2, 1), "")]
        );
    }

    #[test]
    fn several_cursors_refer_to_the_text_after_the_previous_changes() {
        // Typing "x" at three cursors, one of them after a multi-line one.
        let events = check("a😀b\nc\nd", vec![(1, 1, "x\ny"), (3, 3, "x"), (6, 6, "x")]);
        assert_eq!(
            events,
            vec![
                event((0, 1), (0, 1), "x\ny"),
                // "y😀b" — the emoji is two units.
                event((1, 4), (1, 4), "x"),
                event((3, 0), (3, 0), "x"),
            ]
        );
    }

    #[test]
    fn replace_is_a_single_change() {
        assert_eq!(
            check("hello world", vec![(6, 11, "there")]),
            vec![event((0, 6), (0, 11), "there")]
        );
    }

    #[test]
    fn changes_inside_crlf_are_widened() {
        // Typing between '\r' and '\n'.
        assert_eq!(
            check("a\r\nb", vec![(2, 2, "x")]),
            vec![event((0, 1), (1, 0), "\rx\n")]
        );
        // Deleting the '\n' only.
        assert_eq!(
            check("a\r\nb", vec![(2, 3, "")]),
            vec![event((0, 1), (1, 0), "\r")]
        );
        // Deleting the '\r' only.
        assert_eq!(
            check("a\r\nb", vec![(1, 2, "")]),
            vec![event((0, 1), (1, 0), "\n")]
        );
    }

    #[test]
    fn a_new_crlf_does_not_shift_the_change_start() {
        // A lone '\r' gets a '\n' after it: the change starts on the next line of the old text.
        assert_eq!(
            check("a\rb", vec![(2, 2, "\n")]),
            vec![event((1, 0), (1, 0), "\n")]
        );
    }

    #[test]
    fn rope_only_breaks_are_characters() {
        assert_eq!(
            check("a\u{2028}b\nc", vec![(2, 2, "x"), (3, 4, "")]),
            vec![event((0, 2), (0, 2), "x"), event((0, 4), (1, 0), "")]
        );
    }

    #[test]
    fn random_edits_apply_to_the_same_text() {
        let mut rng = Rng::new(11);
        for _ in 0..3000 {
            let old = rng.text(40);
            let len = old.chars().count();
            let mut edits = Vec::new();
            let mut at = 0;
            while at <= len && edits.len() < 4 {
                let from = at + rng.below(len - at + 1);
                let to = from + rng.below((len - from).min(5) + 1);
                let text = rng.text(4);
                edits.push((from, to, text));
                // Adjacent edits too.
                at = to + rng.below(2);
            }
            let edits = edits
                .iter()
                .map(|(from, to, text)| (*from, *to, text.as_str()))
                .collect();
            check(&old, edits);
        }
    }

    #[test]
    fn utf16_length_of_utf8() {
        assert_eq!(utf16_len("aЯ😀é—".as_bytes()), 6);
    }

    #[test]
    fn sync_capabilities() {
        use lsp_types::{SaveOptions, TextDocumentSyncOptions};
        let mut capabilities = ServerCapabilities::default();
        assert_eq!(change_kind(&capabilities), TextDocumentSyncKind::NONE);
        assert!(!open_close(&capabilities));
        assert_eq!(save_include_text(&capabilities), None);

        capabilities.text_document_sync =
            Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL));
        assert_eq!(change_kind(&capabilities), TextDocumentSyncKind::FULL);
        assert!(open_close(&capabilities));
        assert_eq!(save_include_text(&capabilities), Some(false));

        capabilities.text_document_sync = Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::INCREMENTAL),
                save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                    include_text: Some(true),
                })),
                ..Default::default()
            },
        ));
        assert_eq!(
            change_kind(&capabilities),
            TextDocumentSyncKind::INCREMENTAL
        );
        assert!(open_close(&capabilities));
        assert_eq!(save_include_text(&capabilities), Some(true));
    }
}
