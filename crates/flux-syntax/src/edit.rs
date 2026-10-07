//! [`ChangeSet`] (позиции в символах) → [`InputEdit`] для tree-sitter
//! (байты UTF-8 и точки «строка, байт в строке»).
//!
//! Правки считаются в координатах *старого* текста и по возрастанию позиции,
//! а к дереву применяются в обратном порядке: правка, применённая первой,
//! лежит правее всех остальных и не сдвигает их координаты (как `generate_edits`
//! в Helix).
//!
//! Точки — по правилам tree-sitter: строки делятся только по `\n`, колонка —
//! в **байтах** от последнего `\n`. Лексер tree-sitter считает их так же, а
//! внешние сканеры с `get_column` (heredoc в bash) отматывают `byte - column`
//! к началу строки, так что неточная колонка ломает инкрементальный разбор.

use flux_core::Rope;
use flux_core::transaction::{ChangeSet, Operation};
use tree_sitter::{InputEdit, Point};

use crate::text::count_byte;

/// Правки дерева для одного [`ChangeSet`].
pub(crate) struct EditBatch {
    /// По возрастанию позиции, в координатах старого текста.
    /// Применять к дереву в обратном порядке.
    pub edits: Vec<InputEdit>,
    /// Число `\n` в новом тексте.
    pub newlines: usize,
}

/// `newlines` — число `\n` в `old_text`; `changes` должен быть построен для
/// `old_text` (`changes.len() == old_text.len_chars()`).
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
        // Подряд идущие вставки и удаления — одна правка.
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
        // Насыщение — только от неверного `newlines` (правка не от этого текста).
        newlines: (newlines + added).saturating_sub(removed),
    }
}

/// Точка после текста `s`, начатого в точке `point`.
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

/// Точки tree-sitter для неубывающих байтовых смещений текста.
///
/// ropey делит строки не только по `\n`, но и по одинокому `\r`, VT, FF, NEL,
/// U+2028 и U+2029. Если таких «чужих» переводов строк в тексте нет (строк
/// ropey ровно на одну больше, чем `\n`), строка ropey и есть строка
/// tree-sitter, и точка находится за O(log n). Иначе — честный проход по
/// тексту от начала до последней правки: O(n), но такие файлы редки.
enum Points<'a> {
    Lines(&'a Rope),
    Scan {
        text: &'a Rope,
        /// Докуда просмотрен текст.
        byte: usize,
        /// Сколько `\n` встретилось до `byte`.
        row: usize,
        /// Байт сразу после последнего `\n` до `byte`.
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
            // Позиция между `\r` и `\n` у ropey остаётся в строке `\r`, как и у tree-sitter.
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

    /// Точка по определению tree-sitter — прямым подсчётом.
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
        // Канонический ChangeSet склеивает соседние правки.
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
