//! Positions in a document's text, as Flux counts them: zero-based lines (ending at `\n`) and
//! columns in characters.

use crate::{Position, Range};

impl Position {
    /// A position: a zero-based line and a column in characters.
    pub fn new(line: u32, column: u32) -> Self {
        Position { line, column }
    }
}

impl Range {
    /// A span from `start` to `end`; a selection's cursor is at `end`.
    pub fn new(start: Position, end: Position) -> Self {
        Range { start, end }
    }

    /// A span within one line.
    pub fn on_line(line: u32, start: u32, end: u32) -> Self {
        Range {
            start: Position::new(line, start),
            end: Position::new(line, end),
        }
    }

    /// An empty range: a cursor.
    pub fn cursor(at: Position) -> Self {
        Range { start: at, end: at }
    }

    /// A cursor: nothing selected.
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// The range with `start` before `end` (a selection keeps its direction).
    pub fn ordered(self) -> Self {
        if (self.end.line, self.end.column) < (self.start.line, self.start.column) {
            Range {
                start: self.end,
                end: self.start,
            }
        } else {
            self
        }
    }
}

/// The byte offset of a position in `text`; a position past the end of its line or of the text is
/// clamped to it.
pub fn offset(text: &str, position: Position) -> usize {
    let mut line_start = 0;
    for _ in 0..position.line {
        match text[line_start..].find('\n') {
            Some(newline) => line_start += newline + 1,
            None => return text.len(),
        }
    }
    let line = &text[line_start..];
    let line = &line[..line.find('\n').unwrap_or(line.len())];
    let column = line
        .char_indices()
        .nth(position.column as usize)
        .map_or(line.len(), |(index, _)| index);
    line_start + column
}

/// The text of a range (in either direction).
pub fn slice(text: &str, range: Range) -> &str {
    let range = range.ordered();
    &text[offset(text, range.start)..offset(text, range.end)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_count_characters() {
        let text = "fn main() {\n    let ф = 1;\n}";
        assert_eq!(offset(text, Position::new(0, 3)), 3);
        assert_eq!(offset(text, Position::new(1, 8)), 12 + 8);
        // `ф` is two bytes: the column after it is one character further.
        assert_eq!(offset(text, Position::new(1, 9)), 12 + 10);
        // Past the end of a line, past the last line.
        assert_eq!(offset(text, Position::new(0, 99)), 11);
        assert_eq!(offset(text, Position::new(9, 0)), text.len());
    }

    #[test]
    fn slices_in_either_direction() {
        let text = "let ф = 1;\nlet y = 2;";
        let forward = Range::new(Position::new(0, 4), Position::new(1, 3));
        assert_eq!(slice(text, forward), "ф = 1;\nlet");
        let backward = Range::new(forward.end, forward.start);
        assert_eq!(slice(text, backward), "ф = 1;\nlet");
        assert_eq!(slice(text, Range::cursor(Position::new(1, 2))), "");
        assert!(Range::on_line(3, 1, 1).is_empty());
    }
}
