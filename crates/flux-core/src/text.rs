//! Low-level text helpers: graphemes, lines, character classes.

use ropey::{Rope, RopeSlice};
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

/// The boundary of the next grapheme after `char_idx`. A grapheme is what the user sees as a single
/// character: `é` made of two code points, an emoji with a ZWJ, `\r\n`.
pub fn next_grapheme_boundary(slice: RopeSlice, char_idx: usize) -> usize {
    if char_idx >= slice.len_chars() {
        return slice.len_chars();
    }
    let byte_idx = slice.char_to_byte(char_idx);
    let (mut chunk, mut chunk_byte_idx, _, _) = slice.chunk_at_byte(byte_idx);
    let mut cursor = GraphemeCursor::new(byte_idx, slice.len_bytes(), true);
    loop {
        match cursor.next_boundary(chunk, chunk_byte_idx) {
            Ok(None) => return slice.len_chars(),
            Ok(Some(n)) => return slice.byte_to_char(n),
            Err(GraphemeIncomplete::NextChunk) => {
                chunk_byte_idx += chunk.len();
                chunk = slice.chunk_at_byte(chunk_byte_idx).0;
            }
            Err(GraphemeIncomplete::PreContext(n)) => {
                let context = slice.chunk_at_byte(n - 1).0;
                cursor.provide_context(context, n - context.len());
            }
            Err(_) => unreachable!("grapheme cursor asked for an unexpected chunk"),
        }
    }
}

/// The boundary of the previous grapheme before `char_idx`.
pub fn prev_grapheme_boundary(slice: RopeSlice, char_idx: usize) -> usize {
    if char_idx == 0 {
        return 0;
    }
    let char_idx = char_idx.min(slice.len_chars());
    let byte_idx = slice.char_to_byte(char_idx);
    let (mut chunk, mut chunk_byte_idx, _, _) = slice.chunk_at_byte(byte_idx);
    let mut cursor = GraphemeCursor::new(byte_idx, slice.len_bytes(), true);
    loop {
        match cursor.prev_boundary(chunk, chunk_byte_idx) {
            Ok(None) => return 0,
            Ok(Some(n)) => return slice.byte_to_char(n),
            Err(GraphemeIncomplete::PrevChunk) => {
                let (prev, prev_byte_idx, _, _) = slice.chunk_at_byte(chunk_byte_idx - 1);
                chunk = prev;
                chunk_byte_idx = prev_byte_idx;
            }
            Err(GraphemeIncomplete::PreContext(n)) => {
                let context = slice.chunk_at_byte(n - 1).0;
                cursor.provide_context(context, n - context.len());
            }
            Err(_) => unreachable!("grapheme cursor asked for an unexpected chunk"),
        }
    }
}

/// Length of the line break at the end of `line`, in characters (0, 1, or 2 for `\r\n`).
pub fn line_ending_len(line: RopeSlice) -> usize {
    let len = line.len_chars();
    if len == 0 {
        return 0;
    }
    match line.char(len - 1) {
        '\n' if len >= 2 && line.char(len - 2) == '\r' => 2,
        '\n' | '\r' | '\u{000B}' | '\u{000C}' | '\u{0085}' | '\u{2028}' | '\u{2029}' => 1,
        _ => 0,
    }
}

/// The index of the first character of line `line`.
pub fn line_start(text: &Rope, line: usize) -> usize {
    text.line_to_char(line)
}

/// The index of the end of line `line`, before the line break.
pub fn line_end(text: &Rope, line: usize) -> usize {
    let slice = text.line(line);
    text.line_to_char(line) + slice.len_chars() - line_ending_len(slice)
}

/// Length of the line without its line break.
pub fn line_len(text: &Rope, line: usize) -> usize {
    line_end(text, line) - line_start(text, line)
}

/// The leading spaces and tabs of a line.
pub fn indentation(text: &Rope, line: usize) -> String {
    text.line(line)
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// The line break used in the document (determined from the first line).
pub fn detect_line_ending(text: &Rope) -> &'static str {
    for line in text.lines().take(100) {
        match line_ending_len(line) {
            2 => return "\r\n",
            1 => return "\n",
            _ => {}
        }
    }
    "\n"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharClass {
    Whitespace,
    Punctuation,
    Word,
}

pub fn char_class(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Whitespace
    } else if c.is_ascii_punctuation() && c != '_' {
        CharClass::Punctuation
    } else {
        CharClass::Word
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphemes_walk_over_combined_symbols() {
        // "e" + combining acute, family emoji (ZWJ sequence), CRLF.
        let text = Rope::from_str("e\u{301}👨‍👩‍👧x\r\ny");
        let s = text.slice(..);
        let mut stops = vec![0];
        let mut pos = 0;
        while pos < s.len_chars() {
            pos = next_grapheme_boundary(s, pos);
            stops.push(pos);
        }
        assert_eq!(stops, vec![0, 2, 7, 8, 10, 11]);

        let mut back = vec![pos];
        while pos > 0 {
            pos = prev_grapheme_boundary(s, pos);
            back.push(pos);
        }
        back.reverse();
        assert_eq!(back, stops);
    }

    #[test]
    fn graphemes_across_rope_chunks() {
        let text = Rope::from_str(&"ab👍🏽".repeat(2000));
        let s = text.slice(..);
        let mut pos = 0;
        let mut count = 0;
        while pos < s.len_chars() {
            pos = next_grapheme_boundary(s, pos);
            count += 1;
        }
        assert_eq!(count, 3 * 2000);
    }

    #[test]
    fn line_ends() {
        let text = Rope::from_str("ab\r\ncd\nef");
        assert_eq!(line_end(&text, 0), 2);
        assert_eq!(line_end(&text, 1), 6);
        assert_eq!(line_end(&text, 2), 9);
        assert_eq!(detect_line_ending(&text), "\r\n");
    }
}
