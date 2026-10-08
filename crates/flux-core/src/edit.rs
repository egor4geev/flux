//! Text edits. Each function builds a [`Transaction`] for all selections at once, so multi-cursor
//! works without any special code.

use ropey::Rope;

use crate::movement::prev_word_start;
use crate::selection::{Range, Selection};
use crate::text::{
    indentation, line_end, line_len, line_start, next_grapheme_boundary, prev_grapheme_boundary,
};
use crate::transaction::{Assoc, Change, ChangeSet, Transaction};

/// Applies `f` to every selection; after the edit, each cursor is placed at the end of its changed
/// range.
fn change_by_selection(
    text: &Rope,
    selection: &Selection,
    f: impl FnMut(&Range) -> Change,
) -> Transaction {
    let changes = ChangeSet::from_changes(text.len_chars(), selection.iter().map(f));
    let selection = selection.transform(|range| {
        let end = range.to();
        Range::point(changes.map_pos(end, Assoc::After))
    });
    Transaction::new(changes).with_selection(selection)
}

/// Replaces every selection with `insert` (regular typing and paste).
pub fn insert_text(text: &Rope, selection: &Selection, insert: &str) -> Transaction {
    change_by_selection(text, selection, |range| {
        (range.from(), range.to(), Some(insert.to_owned()))
    })
}

/// A newline that keeps the indentation of the current line.
pub fn insert_newline(text: &Rope, selection: &Selection, line_ending: &str) -> Transaction {
    change_by_selection(text, selection, |range| {
        let line = text.char_to_line(range.from());
        let indent: String = indentation(text, line)
            .chars()
            .take(range.from() - line_start(text, line))
            .collect();
        (range.from(), range.to(), Some(format!("{line_ending}{indent}")))
    })
}

/// Tab: spaces up to the next tab stop.
pub fn insert_tab(text: &Rope, selection: &Selection, tab_width: usize) -> Transaction {
    change_by_selection(text, selection, |range| {
        let line = text.char_to_line(range.from());
        let column = range.from() - line_start(text, line);
        let spaces = tab_width - column % tab_width;
        (range.from(), range.to(), Some(" ".repeat(spaces)))
    })
}

/// Backspace: deletes the selection or the grapheme before the cursor.
pub fn delete_backward(text: &Rope, selection: &Selection) -> Transaction {
    change_by_selection(text, selection, |range| {
        if range.is_empty() {
            (prev_grapheme_boundary(text.slice(..), range.head), range.head, None)
        } else {
            (range.from(), range.to(), None)
        }
    })
}

/// Delete: deletes the selection or the grapheme after the cursor.
pub fn delete_forward(text: &Rope, selection: &Selection) -> Transaction {
    change_by_selection(text, selection, |range| {
        if range.is_empty() {
            (range.head, next_grapheme_boundary(text.slice(..), range.head), None)
        } else {
            (range.from(), range.to(), None)
        }
    })
}

/// Alt+Backspace: deletes the word before the cursor.
pub fn delete_word_backward(text: &Rope, selection: &Selection) -> Transaction {
    change_by_selection(text, selection, |range| {
        if range.is_empty() {
            (prev_word_start(text, range.head), range.head, None)
        } else {
            (range.from(), range.to(), None)
        }
    })
}

/// Cmd+Backspace: deletes whole lines that contain cursors and selections, together with the
/// newline (like Delete Line in JetBrains). A selection that ends at the start of a line (the line
/// after a triple click) does not touch that line. The cursor is placed at the same column of the
/// line that moved up into the place of the deleted ones, or, if the last lines were deleted, of
/// the line above them.
pub fn delete_lines(text: &Rope, selection: &Selection) -> Transaction {
    let last_line = text.len_lines() - 1;
    // Line spans `(first, last, cursor column)` in ascending order; overlapping and adjacent ones
    // are merged, and the merged span takes the column of the first selection.
    let mut spans: Vec<(usize, usize, usize)> = Vec::new();
    let mut primary = 0;
    for (index, range) in selection.iter().enumerate() {
        let first = text.char_to_line(range.from());
        let mut last = text.char_to_line(range.to());
        if last > first && range.to() == line_start(text, last) {
            last -= 1;
        }
        let head_line = text.char_to_line(range.head);
        let column = range.head - line_start(text, head_line);
        match spans.last_mut() {
            Some((_, end, _)) if first <= *end + 1 => *end = (*end).max(last),
            _ => spans.push((first, last, column)),
        }
        if index == selection.primary_index() {
            primary = spans.len() - 1;
        }
    }
    let deleted = spans.iter().map(|&(first, last, _)| {
        let (from, to) = if last < last_line {
            (line_start(text, first), line_start(text, last + 1))
        } else if first > 0 {
            // The last lines are removed together with the newline before them.
            (line_end(text, first - 1), text.len_chars())
        } else {
            (0, text.len_chars())
        };
        (from, to, None)
    });
    let changes = ChangeSet::from_changes(text.len_chars(), deleted);
    let cursors = spans
        .iter()
        .map(|&(first, last, column)| {
            let line = if last < last_line {
                Some(last + 1)
            } else {
                first.checked_sub(1)
            };
            let pos = line.map_or(0, |line| {
                changes.map_pos(line_start(text, line), Assoc::After)
                    + column.min(line_len(text, line))
            });
            Range::point(pos)
        })
        .collect();
    Transaction::new(changes).with_selection(Selection::new(cursors, primary))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, selection: Selection, f: impl Fn(&Rope, &Selection) -> Transaction) -> (String, Selection) {
        let mut rope = Rope::from_str(text);
        let tx = f(&rope, &selection);
        tx.changes.apply(&mut rope);
        (rope.to_string(), tx.selection.unwrap())
    }

    #[test]
    fn typing_with_multiple_cursors() {
        let sel = Selection::new(vec![Range::point(0), Range::point(4)], 0);
        let (text, sel) = run("abc\ndef", sel, |t, s| insert_text(t, s, "//"));
        assert_eq!(text, "//abc\n//def");
        assert_eq!(sel.ranges(), &[Range::point(2), Range::point(8)]);
    }

    #[test]
    fn typing_replaces_selection() {
        let (text, sel) = run("hello world", Selection::single(6, 11), |t, s| {
            insert_text(t, s, "rust")
        });
        assert_eq!(text, "hello rust");
        assert_eq!(sel.primary(), Range::point(10));
    }

    #[test]
    fn newline_keeps_indent() {
        let (text, sel) = run("    foo", Selection::point(7), |t, s| insert_newline(t, s, "\n"));
        assert_eq!(text, "    foo\n    ");
        assert_eq!(sel.primary(), Range::point(12));
    }

    #[test]
    fn backspace_removes_grapheme_and_crlf() {
        let (text, sel) = run("a👍🏽", Selection::point(3), delete_backward);
        assert_eq!(text, "a");
        assert_eq!(sel.primary(), Range::point(1));

        let (text, _) = run("a\r\nb", Selection::point(3), delete_backward);
        assert_eq!(text, "ab");
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let rope = Rope::from_str("abc");
        let tx = delete_backward(&rope, &Selection::point(0));
        assert!(tx.changes.is_empty());
    }

    #[test]
    fn tab_aligns_to_tab_stop() {
        let (text, _) = run("ab", Selection::point(2), |t, s| insert_tab(t, s, 4));
        assert_eq!(text, "ab  ");
    }

    #[test]
    fn delete_lines_removes_the_line_and_keeps_the_column() {
        // A cursor in "bbb" at column 1 ends up at column 1 of the line "cc".
        let (text, sel) = run("a\nbbb\ncc", Selection::point(3), delete_lines);
        assert_eq!(text, "a\ncc");
        assert_eq!(sel.primary(), Range::point(3));
        // A column past the edge of a short line means the end of that line.
        let (text, sel) = run("abcd\nx\n", Selection::point(3), delete_lines);
        assert_eq!(text, "x\n");
        assert_eq!(sel.primary(), Range::point(1));
    }

    #[test]
    fn deleting_the_last_line_takes_the_line_break_before_it() {
        let (text, sel) = run("a\nbb", Selection::point(4), delete_lines);
        assert_eq!(text, "a");
        assert_eq!(sel.primary(), Range::point(1));
        // The empty last line after the final newline.
        let (text, _) = run("a\nb\n", Selection::point(4), delete_lines);
        assert_eq!(text, "a\nb");
        // The only line: an empty document.
        let (text, sel) = run("abc", Selection::point(2), delete_lines);
        assert_eq!(text, "");
        assert_eq!(sel.primary(), Range::point(0));
    }

    #[test]
    fn delete_lines_with_cursors_and_selections() {
        // Cursors on lines 0 and 2: both lines go, and the cursors land on the lines that moved up.
        let sel = Selection::new(vec![Range::point(0), Range::point(4)], 1);
        let (text, sel) = run("a\nb\nc\nd", sel, delete_lines);
        assert_eq!(text, "b\nd");
        assert_eq!(sel.ranges(), &[Range::point(0), Range::point(2)]);
        assert_eq!(sel.primary_index(), 1);
        // Cursors on adjacent lines, and two on one line: a single deletion.
        let sel = Selection::new(vec![Range::point(2), Range::point(3), Range::point(5)], 0);
        let (text, sel) = run("a\nbb\nc\nd", sel, delete_lines);
        assert_eq!(text, "a\nd");
        assert_eq!(sel.ranges(), &[Range::point(2)]);
        // A selection across lines 1–2 that ends at the start of line 3: line 3 stays.
        let (text, _) = run("a\nb\nc\nd", Selection::single(2, 6), delete_lines);
        assert_eq!(text, "a\nd");
    }

    #[test]
    fn delete_lines_keeps_crlf() {
        let (text, sel) = run("a\r\nb\r\nc", Selection::point(3), delete_lines);
        assert_eq!(text, "a\r\nc");
        assert_eq!(sel.primary(), Range::point(3));
        let (text, _) = run("a\r\nb", Selection::point(3), delete_lines);
        assert_eq!(text, "a");
    }
}
