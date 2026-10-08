//! The rope as tree-sitter sees it: byte chunks without copying the text, and counting of line
//! breaks and characters.

use std::ops::Range;

use flux_core::Rope;

/// The text from byte `byte` to the end of its chunk, for the parser callback. An empty slice means
/// the end of the text.
pub(crate) fn chunk_from(text: &Rope, byte: usize) -> &[u8] {
    if byte >= text.len_bytes() {
        return &[];
    }
    // The chunk that *contains* the byte: at a seam, ropey returns the next chunk, so the slice is
    // not empty.
    let (chunk, chunk_start, _, _) = text.chunk_at_byte(byte);
    &chunk.as_bytes()[byte - chunk_start..]
}

/// The bytes of a range as rope chunks: the node text for query predicates. The range is clamped to
/// the text length: nodes of an edited but not yet parsed tree may extend past the end.
pub(crate) fn byte_chunks(text: &Rope, range: Range<usize>) -> ByteChunks<'_> {
    let end = range.end.min(text.len_bytes());
    ByteChunks {
        text,
        pos: range.start.min(end),
        end,
    }
}

pub(crate) struct ByteChunks<'a> {
    text: &'a Rope,
    pos: usize,
    end: usize,
}

impl<'a> Iterator for ByteChunks<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        if self.pos >= self.end {
            return None;
        }
        let (chunk, chunk_start, _, _) = self.text.chunk_at_byte(self.pos);
        let to = (self.end - chunk_start).min(chunk.len());
        let bytes = &chunk.as_bytes()[self.pos - chunk_start..to];
        self.pos += bytes.len();
        Some(bytes)
    }
}

/// The number of `\n` in the text. tree-sitter splits lines only on them.
pub(crate) fn count_newlines(text: &Rope) -> usize {
    text.chunks()
        .map(|chunk| count_byte(chunk.as_bytes(), b'\n'))
        .sum()
}

pub(crate) fn count_byte(bytes: &[u8], needle: u8) -> usize {
    bytes.iter().filter(|&&b| b == needle).count()
}

/// Converts non-decreasing byte offsets into character counts from a starting point: one pass over
/// the visible text instead of `byte_to_char` (a descent through the rope tree) for every span
/// boundary.
pub(crate) struct CharCounter<'a> {
    text: &'a Rope,
    chunk: &'a [u8],
    chunk_start: usize,
    byte: usize,
    chars: usize,
}

impl<'a> CharCounter<'a> {
    pub fn new(text: &'a Rope, byte: usize) -> Self {
        let byte = byte.min(text.len_bytes());
        Self {
            text,
            chunk: &[],
            chunk_start: byte,
            byte,
            chars: 0,
        }
    }

    /// How many characters start between the starting point and `byte`. An offset inside a
    /// multi-byte character counts that character: this yields a character boundary instead of a
    /// panic.
    pub fn chars_to(&mut self, byte: usize) -> usize {
        let byte = byte.min(self.text.len_bytes());
        while self.byte < byte {
            let offset = self.byte - self.chunk_start;
            if offset >= self.chunk.len() {
                let (chunk, chunk_start, _, _) = self.text.chunk_at_byte(self.byte);
                self.chunk = chunk.as_bytes();
                self.chunk_start = chunk_start;
                continue;
            }
            let to = (byte - self.chunk_start).min(self.chunk.len());
            self.chars += count_char_starts(&self.chunk[offset..to]);
            self.byte = self.chunk_start + to;
        }
        self.chars
    }
}

/// The leading bytes of UTF-8 characters: all bytes except the continuation bytes `0b10xx_xxxx`.
fn count_char_starts(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| (b as i8) >= -0x40).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rope of many chunks: edits in the middle fragment it.
    fn chunky_rope() -> (Rope, String) {
        let mut text = Rope::new();
        let piece = "ab\nвг👍🏽\r\nx";
        for i in 0..400 {
            let at = (i * 7) % (text.len_chars() + 1);
            text.insert(at, piece);
        }
        let string = text.to_string();
        assert!(text.chunks().count() > 3);
        (text, string)
    }

    #[test]
    fn chunk_from_covers_whole_text() {
        let (text, string) = chunky_rope();
        let mut read = Vec::new();
        while read.len() < string.len() {
            let chunk = chunk_from(&text, read.len());
            assert!(!chunk.is_empty());
            read.extend_from_slice(chunk);
        }
        assert_eq!(read, string.as_bytes());
        assert!(chunk_from(&text, string.len()).is_empty());
        assert!(chunk_from(&text, string.len() + 10).is_empty());
    }

    #[test]
    fn byte_chunks_returns_exact_range() {
        let (text, string) = chunky_rope();
        let len = string.len();
        for (start, end) in [
            (0, len),
            (5, 900),
            (1000, 1001),
            (len - 3, len + 50),
            (len + 1, len + 2),
        ] {
            let got: Vec<u8> = byte_chunks(&text, start..end).flatten().copied().collect();
            let end = end.min(len);
            let start = start.min(end);
            assert_eq!(got, &string.as_bytes()[start..end]);
        }
    }

    #[test]
    fn counts_newlines_only() {
        let text = Rope::from_str("a\nb\r\nc\rd\u{2028}e\u{c}f\n");
        assert_eq!(count_newlines(&text), 3);
    }

    #[test]
    fn char_counter_matches_rope() {
        let (text, string) = chunky_rope();
        let mut counter = CharCounter::new(&text, 0);
        for (byte, _) in string.char_indices().step_by(13) {
            assert_eq!(counter.chars_to(byte), text.byte_to_char(byte));
        }
        assert_eq!(counter.chars_to(string.len() + 5), text.len_chars());
    }

    #[test]
    fn char_counter_rounds_up_inside_char() {
        let text = Rope::from_str("aж👍b");
        let mut counter = CharCounter::new(&text, 0);
        assert_eq!(counter.chars_to(2), 2, "middle of 'ж' counts it");
        assert_eq!(counter.chars_to(3), 2);
        assert_eq!(counter.chars_to(5), 3, "middle of the emoji");
        assert_eq!(counter.chars_to(7), 3);
        assert_eq!(counter.chars_to(8), 4);
    }
}
