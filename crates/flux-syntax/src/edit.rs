//! [`ChangeSet`] (positions in characters) → [`InputEdit`] for tree-sitter (UTF-8 bytes and "line,
//! byte within the line" points).
//!
//! Edits are computed in the coordinates of the *old* text and in ascending order of position, but
//! are applied to the tree in reverse order: the edit applied first lies to the right of all the
//! others and doesn't shift their coordinates (like `generate_edits` in Helix).
//!
//! Points follow tree-sitter's rules: lines are split only on `\n`, and the column is counted in
//! **bytes** from the last `\n`. The tree-sitter lexer counts them the same way, and external
//! scanners that use `get_column` (heredoc in bash) rewind `byte - column` to the start of the
//! line, so an inexact column breaks incremental parsing.

use flux_core::Rope;
use flux_core::transaction::{ChangeSet, Operation};
use tree_sitter::{InputEdit, Point};

use crate::text::count_byte;

/// Tree edits for a single [`ChangeSet`].
pub(crate) struct EditBatch {
    /// In ascending order of position, in old-text coordinates. Apply to the tree in reverse order.
    pub edits: Vec<InputEdit>,
    /// The number of `\n` in the new text.
    pub newlines: usize,
}

/// `newlines` is the number of `\n` in `old_text`; `changes` must have been built for `old_text`
/// (`changes.len() == old_text.len_chars()`).
pub(crate) fn input_edits(old_text: &Rope, changes: &ChangeSet, newlines: usize) -> EditBatch {
    let mut points = Points::new(old_text, newlines);
    let mut edits = Vec::new();
    let (mut added, mut removed) = (0, 0);
    let ops = changes.ops();
    let mut pos = 0;
    let mut i = 0;
    while i < ops.len() {
        if let Operation::Retain(n) = ops[i] {
            pos += n;
            i += 1;
            continue;
        }
        // Consecutive insertions and deletions form a single edit.
        let start_byte = old_text.char_to_byte(pos);
        let start_position = points.at(start_byte);
        let mut new_end_byte = start_byte;
        let mut new_end_position = start_position;
        while let Some(op) = ops.get(i) {
            match op {
                Operation::Retain(_) => break,
                Operation::Delete(n) => pos += n,
                Operation::Insert(s) => {
                    new_end_byte += s.len();
                    new_end_position = advance(new_end_position, s);
                }
            }
            i += 1;
        }
        let old_end_byte = old_text.char_to_byte(pos);
        let old_end_position = points.at(old_end_byte);
        removed += old_end_position.row - start_position.row;
        added += new_end_position.row - start_position.row;
        edits.push(InputEdit {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_position,
            old_end_position,
            new_end_position,
        });
    }
    EditBatch {
        edits,
        // Saturation happens only with an incorrect `newlines` (the edit is not for this text).
        newlines: (newlines + added).saturating_sub(removed),
    }
}

/// The point after text `s` that starts at `point`.
fn advance(mut point: Point, s: &str) -> Point {
    let bytes = s.as_bytes();
    match bytes.iter().rposition(|&b| b == b'\n') {
        Some(last) => {
            point.row += count_byte(bytes, b'\n');
            point.column = bytes.len() - last - 1;
        }
        None => point.column += bytes.len(),
    }
    point
}

/// tree-sitter points for non-decreasing byte offsets into the text.
///
/// ropey splits lines not only on `\n` but also on a lone `\r`, VT, FF, NEL, U+2028 and U+2029. If
/// the text has no such "foreign" line breaks (ropey has exactly one more line than there are
/// `\n`), a ropey line is a tree-sitter line, and a point is found in O(log n). Otherwise, a plain
/// pass over the text from the start to the last edit: O(n), but such files are rare.
enum Points<'a> {
    Lines(&'a Rope),
    Scan {
        text: &'a Rope,
        /// How far the text has been scanned.
        byte: usize,
        /// How many `\n` were encountered before `byte`.
        row: usize,
        /// The byte immediately after the last `\n` before `byte`.
        row_start: usize,
    },
}

impl<'a> Points<'a> {
    fn new(text: &'a Rope, newlines: usize) -> Self {
        if text.len_lines() == newlines + 1 {
            Self::Lines(text)
        } else {
            Self::Scan {
                text,
                byte: 0,
                row: 0,
                row_start: 0,
            }
        }
    }

    fn at(&mut self, byte: usize) -> Point {
        match self {
            // A position between `\r` and `\n` stays on the `\r` line in ropey, just as in
            // tree-sitter.
            Self::Lines(text) => {
                let row = text.byte_to_line(byte);
                Point::new(row, byte - text.line_to_byte(row))
            }
            Self::Scan {
                text,
                byte: scanned,
                row,
                row_start,
            } => {
                debug_assert!(byte >= *scanned, "points must be requested in order");
                while *scanned < byte {
                    let (chunk, chunk_start, _, _) = text.chunk_at_byte(*scanned);
                    let to = (byte - chunk_start).min(chunk.len());
                    let bytes = &chunk.as_bytes()[*scanned - chunk_start..to];
                    if let Some(last) = bytes.iter().rposition(|&b| b == b'\n') {
                        *row += count_byte(bytes, b'\n');
                        *row_start = *scanned + last + 1;
                    }
                    *scanned += bytes.len();
                }
                Point::new(*row, byte - *row_start)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::count_newlines;

    /// A point by tree-sitter's definition, computed by direct counting.
    fn naive_point(text: &str, byte: usize) -> Point {
        let before = &text.as_bytes()[..byte];
        let row = count_byte(before, b'\n');
        let row_start = before
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1);
        Point::new(row, byte - row_start)
    }

    fn check_points(text: &str) {
        let rope = Rope::from_str(text);
        let mut points = Points::new(&rope, count_newlines(&rope));
        for byte in (0..=text.len()).filter(|&b| text.is_char_boundary(b)) {
            assert_eq!(
                points.at(byte),
                naive_point(text, byte),
                "{text:?} at {byte}"
            );
        }
    }

    #[test]
    fn points_with_plain_line_breaks() {
        let text = "fn main() {\r\n    let ы = \"👍🏽\";\n}\n";
        let rope = Rope::from_str(text);
        assert!(matches!(Points::new(&rope, 3), Points::Lines(_)));
        check_points(text);
        check_points("");
        check_points("\n\n");
        check_points("\r\n");
    }

    #[test]
    fn points_with_foreign_line_breaks() {
        for text in [
            "a\rb\nc",
            "a\u{2028}b\nв\u{2029}г\n",
            "x\u{b}y\u{c}z\u{85}w\n\r",
            "\r\r\r\n",
            "only\rcarriage\rreturns",
        ] {
            let rope = Rope::from_str(text);
            assert!(matches!(
                Points::new(&rope, count_newlines(&rope)),
                Points::Scan { .. }
            ));
            check_points(text);
        }
    }

    #[test]
    fn column_is_in_bytes() {
        let rope = Rope::from_str("ж\nыы👍x");
        let cs = ChangeSet::from_changes(rope.len_chars(), [(5, 5, Some("я".into()))]);
        let batch = input_edits(&rope, &cs, 1);
        let edit = batch.edits[0];
        assert_eq!(edit.start_byte, 2 + 1 + 4 + 4);
        assert_eq!(edit.start_position, Point::new(1, 8));
        assert_eq!(edit.new_end_byte, edit.start_byte + 2);
        assert_eq!(edit.new_end_position, Point::new(1, 10));
    }

    #[test]
    fn multi_range_edit_in_old_coordinates() {
        let rope = Rope::from_str("one\ntwo\nthree\n");
        let cs = ChangeSet::from_changes(
            rope.len_chars(),
            [
                (0, 0, Some("// ".into())),
                (4, 7, None),
                (8, 13, Some("3\n3".into())),
            ],
        );
        let batch = input_edits(&rope, &cs, 3);
        assert_eq!(batch.newlines, 4);
        let [first, second, third] = batch.edits[..] else {
            panic!("expected three edits: {:?}", batch.edits)
        };
        assert_eq!(
            (first.start_byte, first.old_end_byte, first.new_end_byte),
            (0, 0, 3)
        );
        assert_eq!(first.new_end_position, Point::new(0, 3));
        assert_eq!(
            (second.start_byte, second.old_end_byte, second.new_end_byte),
            (4, 7, 4)
        );
        assert_eq!(second.start_position, Point::new(1, 0));
        assert_eq!(second.old_end_position, Point::new(1, 3));
        assert_eq!(second.new_end_position, Point::new(1, 0));
        assert_eq!(
            (third.start_byte, third.old_end_byte, third.new_end_byte),
            (8, 13, 11)
        );
        assert_eq!(third.start_position, Point::new(2, 0));
        assert_eq!(third.old_end_position, Point::new(2, 5));
        assert_eq!(third.new_end_position, Point::new(3, 1));
    }

    #[test]
    fn touching_ranges_become_one_edit() {
        // A canonical ChangeSet merges adjacent edits.
        let rope = Rope::from_str("abcdef");
        let cs = ChangeSet::from_changes(6, [(1, 2, Some("X".into())), (2, 4, None)]);
        let batch = input_edits(&rope, &cs, 0);
        assert_eq!(batch.edits.len(), 1);
        assert_eq!(batch.edits[0].start_byte, 1);
        assert_eq!(batch.edits[0].old_end_byte, 4);
        assert_eq!(batch.edits[0].new_end_byte, 2);
    }

    #[test]
    fn newline_count_follows_edits() {
        let rope = Rope::from_str("a\r\nb\rc\u{2028}d\ne");
        let cs = ChangeSet::from_changes(
            rope.len_chars(),
            [(1, 4, Some("\n\n".into())), (6, 9, Some("x\r\ny".into()))],
        );
        let batch = input_edits(&rope, &cs, count_newlines(&rope));
        let mut new_rope = rope.clone();
        cs.apply(&mut new_rope);
        assert_eq!(batch.newlines, count_newlines(&new_rope));
    }
}
