//! Правки текста. Каждая функция строит [`Transaction`] для всех выделений
//! сразу — так мультикурсор работает без отдельного кода.

use ropey::Rope;

use crate::movement::prev_word_start;
use crate::selection::{Range, Selection};
use crate::text::{
    indentation, line_end, line_len, line_start, next_grapheme_boundary, prev_grapheme_boundary,
};
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

/// Cmd+Backspace: удаляет целиком строки, на которых стоят курсоры и выделения, вместе с
/// переводом строки (как Delete Line в JetBrains). Выделение, которое кончается в начале
/// строки (строка после тройного щелчка), эту строку не задевает. Курсор встаёт в ту же
/// колонку строки, поднявшейся на место удалённых, а если удалены последние строки — строки
/// над ними.
pub fn delete_lines(text: &Rope, selection: &Selection) -> Transaction {
    let last_line = text.len_lines() - 1;
    // Участки строк `(первая, последняя, колонка курсора)` по возрастанию; пересекающиеся
    // и соседние слиты — у слитого колонка первого выделения.
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
            // Последние строки уходят вместе с переводом строки перед ними.
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
        // Курсор в «bbb» на колонке 1 — встаёт на колонку 1 строки «cc».
        let (text, sel) = run("a\nbbb\ncc", Selection::point(3), delete_lines);
        assert_eq!(text, "a\ncc");
        assert_eq!(sel.primary(), Range::point(3));
        // Колонка за краем короткой строки — её конец.
        let (text, sel) = run("abcd\nx\n", Selection::point(3), delete_lines);
        assert_eq!(text, "x\n");
        assert_eq!(sel.primary(), Range::point(1));
    }

    #[test]
    fn deleting_the_last_line_takes_the_line_break_before_it() {
        let (text, sel) = run("a\nbb", Selection::point(4), delete_lines);
        assert_eq!(text, "a");
        assert_eq!(sel.primary(), Range::point(1));
        // Пустая последняя строка после финального перевода строки.
        let (text, _) = run("a\nb\n", Selection::point(4), delete_lines);
        assert_eq!(text, "a\nb");
        // Единственная строка — пустой документ.
        let (text, sel) = run("abc", Selection::point(2), delete_lines);
        assert_eq!(text, "");
        assert_eq!(sel.primary(), Range::point(0));
    }

    #[test]
    fn delete_lines_with_cursors_and_selections() {
        // Курсоры на строках 0 и 2 — обе строки, курсоры на поднявшихся.
        let sel = Selection::new(vec![Range::point(0), Range::point(4)], 1);
        let (text, sel) = run("a\nb\nc\nd", sel, delete_lines);
        assert_eq!(text, "b\nd");
        assert_eq!(sel.ranges(), &[Range::point(0), Range::point(2)]);
        assert_eq!(sel.primary_index(), 1);
        // Курсоры на соседних строках и два на одной — одно удаление.
        let sel = Selection::new(vec![Range::point(2), Range::point(3), Range::point(5)], 0);
        let (text, sel) = run("a\nbb\nc\nd", sel, delete_lines);
        assert_eq!(text, "a\nd");
        assert_eq!(sel.ranges(), &[Range::point(2)]);
        // Выделение через строки 1–2, кончается в начале строки 3: строка 3 остаётся.
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
