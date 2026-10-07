//! Движение курсора. Каждая функция берёт выделение и возвращает новое;
//! `extend` — растягивать выделение (Shift) или переносить курсор.

use ropey::Rope;

use crate::selection::Range;
use crate::text::{
    CharClass, char_class, line_end, line_len, line_start, next_grapheme_boundary,
    prev_grapheme_boundary,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Backward,
    Forward,
}

pub fn move_horizontally(text: &Rope, range: Range, dir: Direction, extend: bool) -> Range {
    // Как в обычных редакторах: стрелка без Shift сначала схлопывает выделение.
    if !extend && !range.is_empty() {
        let pos = match dir {
            Direction::Backward => range.from(),
            Direction::Forward => range.to(),
        };
        return Range::point(pos);
    }
    let slice = text.slice(..);
    let pos = match dir {
        Direction::Backward => prev_grapheme_boundary(slice, range.head),
        Direction::Forward => next_grapheme_boundary(slice, range.head),
    };
    range.put_cursor(pos, extend)
}

pub fn move_vertically(
    text: &Rope,
    range: Range,
    dir: Direction,
    count: usize,
    extend: bool,
) -> Range {
    let line = text.char_to_line(range.head);
    let column = range
        .goal_column
        .unwrap_or_else(|| range.head - line_start(text, line));
    let last_line = text.len_lines() - 1;

    let pos = match dir {
        Direction::Backward if line == 0 => 0,
        Direction::Forward if line == last_line => text.len_chars(),
        Direction::Backward => {
            let target = line.saturating_sub(count);
            line_start(text, target) + column.min(line_len(text, target))
        }
        Direction::Forward => {
            let target = (line + count).min(last_line);
            line_start(text, target) + column.min(line_len(text, target))
        }
    };
    range.put_cursor(pos, extend).with_goal_column(Some(column))
}

/// «Умный» Home: сначала к первому непробельному символу, повторно — в колонку 0.
pub fn move_line_start(text: &Rope, range: Range, extend: bool) -> Range {
    let line = text.char_to_line(range.head);
    let start = line_start(text, line);
    let first_non_blank = text
        .line(line)
        .chars()
        .take(line_len(text, line))
        .position(|c| c != ' ' && c != '\t')
        .map_or(line_end(text, line), |offset| start + offset);
    let pos = if range.head == first_non_blank {
        start
    } else {
        first_non_blank
    };
    range.put_cursor(pos, extend)
}

pub fn move_line_end(text: &Rope, range: Range, extend: bool) -> Range {
    let line = text.char_to_line(range.head);
    range.put_cursor(line_end(text, line), extend)
}

pub fn move_document_start(range: Range, extend: bool) -> Range {
    range.put_cursor(0, extend)
}

pub fn move_document_end(text: &Rope, range: Range, extend: bool) -> Range {
    range.put_cursor(text.len_chars(), extend)
}

/// Конец следующего слова (Alt+→).
pub fn next_word_end(text: &Rope, pos: usize) -> usize {
    let len = text.len_chars();
    let mut i = pos;
    while i < len && char_class(text.char(i)) == CharClass::Whitespace {
        i += 1;
    }
    if i < len {
        let class = char_class(text.char(i));
        while i < len && char_class(text.char(i)) == class {
            i += 1;
        }
    }
    i
}

/// Начало предыдущего слова (Alt+←).
pub fn prev_word_start(text: &Rope, pos: usize) -> usize {
    let mut i = pos;
    while i > 0 && char_class(text.char(i - 1)) == CharClass::Whitespace {
        i -= 1;
    }
    if i > 0 {
        let class = char_class(text.char(i - 1));
        while i > 0 && char_class(text.char(i - 1)) == class {
            i -= 1;
        }
    }
    i
}

/// Слово (или серия пунктуации/пробелов) под позицией — для двойного клика.
pub fn word_range_at(text: &Rope, pos: usize) -> Range {
    let len = text.len_chars();
    if len == 0 {
        return Range::point(0);
    }
    // Клик сразу за словом выделяет это слово, а не пробел после него.
    let probe = if pos >= len
        || (pos > 0 && char_class(text.char(pos)) == CharClass::Whitespace
            && char_class(text.char(pos - 1)) != CharClass::Whitespace)
    {
        pos.min(len) - 1
    } else {
        pos
    };
    let class = char_class(text.char(probe));
    let mut from = probe;
    while from > 0 && char_class(text.char(from - 1)) == class && text.char(from - 1) != '\n' {
        from -= 1;
    }
    let mut to = probe + 1;
    while to < len && char_class(text.char(to)) == class && text.char(to) != '\n' {
        to += 1;
    }
    Range::new(from, to)
}

pub fn move_word(text: &Rope, range: Range, dir: Direction, extend: bool) -> Range {
    let pos = match dir {
        Direction::Backward => prev_word_start(text, range.head),
        Direction::Forward => next_word_end(text, range.head),
    };
    range.put_cursor(pos, extend)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Direction::*;

    #[test]
    fn vertical_movement_remembers_column() {
        let text = Rope::from_str("long line\nab\nanother line");
        let r = Range::point(7);
        let r = move_vertically(&text, r, Forward, 1, false);
        assert_eq!(r.head, 12); // конец короткой строки "ab"
        let r = move_vertically(&text, r, Forward, 1, false);
        assert_eq!(r.head, 13 + 7);
    }

    #[test]
    fn vertical_movement_at_edges() {
        let text = Rope::from_str("abc\ndef");
        let r = move_vertically(&text, Range::point(2), Backward, 1, false);
        assert_eq!(r.head, 0);
        let r = move_vertically(&text, Range::point(5), Forward, 1, false);
        assert_eq!(r.head, 7);
    }

    #[test]
    fn horizontal_collapses_selection_first() {
        let text = Rope::from_str("hello");
        let r = move_horizontally(&text, Range::new(1, 4), Backward, false);
        assert_eq!(r, Range::point(1));
        let r = move_horizontally(&text, Range::new(1, 4), Forward, true);
        assert_eq!(r, Range::new(1, 5));
    }

    #[test]
    fn smart_home_toggles() {
        let text = Rope::from_str("    let x;");
        let r = move_line_start(&text, Range::point(8), false);
        assert_eq!(r.head, 4);
        let r = move_line_start(&text, r, false);
        assert_eq!(r.head, 0);
        let r = move_line_start(&text, r, false);
        assert_eq!(r.head, 4);
    }

    #[test]
    fn word_movement() {
        let text = Rope::from_str("foo_bar.baz  qux");
        assert_eq!(next_word_end(&text, 0), 7);
        assert_eq!(next_word_end(&text, 7), 8);
        assert_eq!(next_word_end(&text, 11), 16);
        assert_eq!(prev_word_start(&text, 16), 13);
        assert_eq!(prev_word_start(&text, 13), 8);
        assert_eq!(prev_word_start(&text, 7), 0);
    }

    #[test]
    fn word_under_cursor() {
        let text = Rope::from_str("let foo_bar = 1;");
        assert_eq!(word_range_at(&text, 5), Range::new(4, 11));
        assert_eq!(word_range_at(&text, 11), Range::new(4, 11));
        assert_eq!(word_range_at(&text, 12), Range::new(12, 13));
        assert_eq!(word_range_at(&text, 16), Range::new(15, 16));
    }
}
