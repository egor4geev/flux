//! Правки текста. Каждая функция строит [`Transaction`] для всех выделений
//! сразу — так мультикурсор работает без отдельного кода.

use ropey::Rope;

use crate::movement::prev_word_start;
use crate::selection::{Range, Selection};
use crate::text::{indentation, line_start, next_grapheme_boundary, prev_grapheme_boundary};
use crate::transaction::{Assoc, Change, ChangeSet, Transaction};

/// Применяет `f` к каждому выделению; после правки каждый курсор встаёт
/// в конец своего изменённого участка.
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

/// Заменяет каждое выделение на `insert` (обычный ввод и вставка).
pub fn insert_text(text: &Rope, selection: &Selection, insert: &str) -> Transaction {
    change_by_selection(text, selection, |range| {
        (range.from(), range.to(), Some(insert.to_owned()))
    })
}

/// Перевод строки с сохранением отступа текущей строки.
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

/// Tab: пробелы до следующей позиции табуляции.
pub fn insert_tab(text: &Rope, selection: &Selection, tab_width: usize) -> Transaction {
    change_by_selection(text, selection, |range| {
        let line = text.char_to_line(range.from());
        let column = range.from() - line_start(text, line);
        let spaces = tab_width - column % tab_width;
        (range.from(), range.to(), Some(" ".repeat(spaces)))
    })
}

/// Backspace: удаляет выделение или графему перед курсором.
pub fn delete_backward(text: &Rope, selection: &Selection) -> Transaction {
    change_by_selection(text, selection, |range| {
        if range.is_empty() {
            (prev_grapheme_boundary(text.slice(..), range.head), range.head, None)
        } else {
            (range.from(), range.to(), None)
        }
    })
}

/// Delete: удаляет выделение или графему после курсора.
pub fn delete_forward(text: &Rope, selection: &Selection) -> Transaction {
    change_by_selection(text, selection, |range| {
        if range.is_empty() {
            (range.head, next_grapheme_boundary(text.slice(..), range.head), None)
        } else {
            (range.from(), range.to(), None)
        }
    })
}

/// Alt+Backspace: удаляет слово перед курсором.
pub fn delete_word_backward(text: &Rope, selection: &Selection) -> Transaction {
    change_by_selection(text, selection, |range| {
        if range.is_empty() {
            (prev_word_start(text, range.head), range.head, None)
        } else {
            (range.from(), range.to(), None)
        }
    })
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
}
