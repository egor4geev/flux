//! Character indices in a rope ↔ LSP positions (line + UTF-16 code unit offset), and file paths ↔
//! `file://` URIs.
//!
//! Lines are counted the LSP way: only `\n`, `\r\n`, and a lone `\r` end a line. The rope also
//! breaks lines at VT, FF, NEL, LS, and PS (U+000B, U+000C, U+0085, U+2028, U+2029); for a language
//! server these are ordinary characters, so in a document that contains them rope line numbers
//! would drift from the server's, and incremental sync would corrupt the server's copy. [`Lines`]
//! finds these characters once (a byte scan that normally finds none) and corrects rope line
//! numbers by them; without them, a conversion is plain rope arithmetic, O(log n).
//!
//! A position between the `\r` and `\n` of a CRLF can't be expressed in LSP terms; [`to_lsp`] gives
//! the end of the line there, past the `\r`, which servers clamp to the end of the line.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use flux_core::Rope;
use flux_core::text::line_end;
use lsp_types::{Position, Uri};
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str};

pub fn to_lsp(text: &Rope, pos: usize) -> Position {
    Lines::new(text).to_lsp(pos)
}

/// Clamped: a line past the end is the end of the document, a column past the end of the line
/// is the end of the line (before the line break).
pub fn from_lsp(text: &Rope, position: Position) -> usize {
    Lines::new(text).from_lsp(position)
}

pub fn range_to_lsp(text: &Rope, range: Range<usize>) -> lsp_types::Range {
    Lines::new(text).range_to_lsp(range)
}

pub fn range_from_lsp(text: &Rope, range: lsp_types::Range) -> Range<usize> {
    Lines::new(text).range_from_lsp(range)
}

/// A text prepared for many conversions: the rope-only line breaks are found once. The free
/// functions above build one per call; convert batches (diagnostics, edits) through one `Lines`.
pub struct Lines<'a> {
    text: &'a Rope,
    /// Character indices of the line breaks that only the rope counts, ascending.
    extra: Vec<usize>,
}

impl<'a> Lines<'a> {
    pub fn new(text: &'a Rope) -> Self {
        Self {
            text,
            extra: extra_breaks(text),
        }
    }

    pub fn to_lsp(&self, pos: usize) -> Position {
        let text = self.text;
        let pos = pos.min(text.len_chars());
        let line = text.char_to_line(pos);
        // Every rope-only break before `pos` ends a rope line above `line`.
        let extra_before = self.extra.partition_point(|&p| p < pos);
        let start = self.lsp_line_start(line);
        let column = text.char_to_utf16_cu(pos) - text.char_to_utf16_cu(start);
        Position::new((line - extra_before) as u32, column as u32)
    }

    pub fn from_lsp(&self, position: Position) -> usize {
        let text = self.text;
        let target = position.line as usize;
        // The first rope line of LSP line `target` is the least `r` with `r - X(r) == target`,
        // where `X(r)` counts the rope-only breaks before rope line `r`. `r - X(r)` never
        // decreases, so iterating `r = target + X(r)` from `r = target` reaches it.
        let mut first = target;
        loop {
            if first >= text.len_lines() {
                return text.len_chars();
            }
            let start = text.line_to_char(first);
            let next = target + self.extra.partition_point(|&p| p < start);
            if next == first {
                break;
            }
            first = next;
        }
        // The LSP line goes on through rope lines that end with a rope-only break.
        let mut last = first;
        while last + 1 < text.len_lines() && self.is_extra(text.line_to_char(last + 1) - 1) {
            last += 1;
        }
        let start = text.char_to_utf16_cu(text.line_to_char(first));
        let end = text.char_to_utf16_cu(line_end(text, last));
        // In the middle of a surrogate pair the rope rounds down to the character.
        text.utf16_cu_to_char((start + position.character as usize).min(end))
    }

    pub fn range_to_lsp(&self, range: Range<usize>) -> lsp_types::Range {
        lsp_types::Range::new(self.to_lsp(range.start), self.to_lsp(range.end))
    }

    pub fn range_from_lsp(&self, range: lsp_types::Range) -> Range<usize> {
        let start = self.from_lsp(range.start);
        start..self.from_lsp(range.end).max(start)
    }

    /// The first character of the LSP line that contains rope line `line`.
    fn lsp_line_start(&self, mut line: usize) -> usize {
        let mut start = self.text.line_to_char(line);
        while line > 0 && self.is_extra(start - 1) {
            line -= 1;
            start = self.text.line_to_char(line);
        }
        start
    }

    fn is_extra(&self, pos: usize) -> bool {
        !self.extra.is_empty() && self.extra.binary_search(&pos).is_ok()
    }
}

/// Character indices of VT, FF, NEL, LS, and PS, ascending. A rope chunk never splits a
/// character, so a multibyte sequence is always within one chunk.
fn extra_breaks(text: &Rope) -> Vec<usize> {
    let mut bytes_found = Vec::new();
    let mut chunk_start = 0;
    for chunk in text.chunks() {
        let bytes = chunk.as_bytes();
        let at = |i: usize| bytes.get(i).copied();
        for i in memchr::memchr3_iter(0x0B, 0x0C, 0xC2, bytes) {
            // NEL is C2 85.
            if bytes[i] != 0xC2 || at(i + 1) == Some(0x85) {
                bytes_found.push(chunk_start + i);
            }
        }
        // LS and PS are E2 80 A8 and E2 80 A9.
        for i in memchr::memchr_iter(0xE2, bytes) {
            if at(i + 1) == Some(0x80) && matches!(at(i + 2), Some(0xA8 | 0xA9)) {
                bytes_found.push(chunk_start + i);
            }
        }
        chunk_start += bytes.len();
    }
    bytes_found.sort_unstable();
    bytes_found
        .into_iter()
        .map(|byte| text.byte_to_char(byte))
        .collect()
}

/// What a path component may contain unescaped in a URI (RFC 3986 `pchar`) besides unreserved
/// characters: everything else is percent-encoded, non-ASCII bytes included.
const PATH: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}')
    .add(0x7F);

/// `file://` URI for a path; a relative path is made absolute against the current directory.
pub fn uri_from_path(path: &Path) -> Uri {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut uri = String::from("file://");
    for part in encode_path(&path) {
        uri.push_str(part);
    }
    // Everything outside `pchar` and `/` is escaped above, so the URI always parses.
    Uri::from_str(&uri).unwrap_or_else(|_| Uri::from_str("file:///").expect("a valid URI"))
}

#[cfg(unix)]
fn encode_path(path: &Path) -> percent_encoding::PercentEncode<'_> {
    use std::os::unix::ffi::OsStrExt;
    percent_encoding::percent_encode(path.as_os_str().as_bytes(), PATH)
}

#[cfg(not(unix))]
fn encode_path(path: &Path) -> percent_encoding::PercentEncode<'_> {
    // Non-UTF-8 paths are not representable here; they don't occur on the platforms we target.
    percent_encoding::utf8_percent_encode(path.to_str().unwrap_or_default(), PATH)
}

/// The path of a `file://` URI (`file:///…` or `file://localhost/…`); `None` for other schemes
/// and hosts. Compare documents by path, not by URI string: servers encode URIs differently.
pub fn path_from_uri(uri: &Uri) -> Option<PathBuf> {
    if !uri.scheme()?.as_str().eq_ignore_ascii_case("file") {
        return None;
    }
    let host = uri.authority().map_or("", |authority| authority.as_str());
    if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
        return None;
    }
    let bytes: Vec<u8> = percent_decode_str(uri.path().as_str()).collect();
    if bytes.first() != Some(&b'/') {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
    }
    #[cfg(not(unix))]
    {
        String::from_utf8(bytes).ok().map(PathBuf::from)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Reference conversion straight from the LSP rules, on a string.
    fn reference_to_lsp(text: &str, pos: usize) -> Position {
        let chars: Vec<char> = text.chars().collect();
        let (mut line, mut column) = (0, 0);
        let mut i = 0;
        while i < pos {
            match chars[i] {
                '\r' if chars.get(i + 1) == Some(&'\n') && i + 1 < pos => {
                    line += 1;
                    column = 0;
                    i += 1;
                }
                '\n' | '\r' => {
                    line += 1;
                    column = 0;
                }
                c => column += c.len_utf16() as u32,
            }
            i += 1;
        }
        Position::new(line, column)
    }

    /// Reference: LSP lines of `text` as `(start, end before the break)` character indices.
    fn reference_lines(text: &str) -> Vec<(usize, usize)> {
        let chars: Vec<char> = text.chars().collect();
        let mut lines = Vec::new();
        let mut start = 0;
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '\r' if chars.get(i + 1) == Some(&'\n') => {
                    lines.push((start, i));
                    i += 2;
                    start = i;
                }
                '\n' | '\r' => {
                    lines.push((start, i));
                    i += 1;
                    start = i;
                }
                _ => i += 1,
            }
        }
        lines.push((start, chars.len()));
        lines
    }

    fn reference_from_lsp(text: &str, position: Position) -> usize {
        let chars: Vec<char> = text.chars().collect();
        let lines = reference_lines(text);
        let Some(&(start, end)) = lines.get(position.line as usize) else {
            return chars.len();
        };
        let mut units = 0;
        let mut pos = start;
        while pos < end {
            let width = chars[pos].len_utf16();
            // In the middle of a surrogate pair: the character itself (rounded down).
            if units + width > position.character as usize {
                break;
            }
            units += width;
            pos += 1;
        }
        pos
    }

    /// A tiny deterministic generator: tests don't need a `rand` dependency.
    pub(crate) struct Rng(u64);

    impl Rng {
        pub(crate) fn new(seed: u64) -> Self {
            Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
        }

        pub(crate) fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }

        /// Random text over an alphabet heavy in line breaks and multi-unit characters.
        pub(crate) fn text(&mut self, max_len: usize) -> String {
            const PIECES: &[&str] = &[
                "a", "b", "z", " ", "\n", "\r\n", "\r", "é", "Я", "😀", "\u{2028}", "\u{c}",
                "\u{85}", "\u{b}", "\u{2029}", "\t",
            ];
            let len = self.below(max_len + 1);
            (0..len).map(|_| PIECES[self.below(PIECES.len())]).collect()
        }
    }

    /// A position between the `\r` and `\n` of a CRLF: not expressible in LSP terms.
    fn inside_crlf(chars: &[char], pos: usize) -> bool {
        pos > 0 && chars[pos - 1] == '\r' && chars.get(pos) == Some(&'\n')
    }

    #[test]
    fn plain_lines_and_utf16_columns() {
        let text = Rope::from("fn main() {\n    let 😀 = \"Я\";\n}\n");
        assert_eq!(to_lsp(&text, 0), Position::new(0, 0));
        // "    let " is 8 units; the emoji is 2.
        assert_eq!(to_lsp(&text, 12 + 8), Position::new(1, 8));
        assert_eq!(to_lsp(&text, 12 + 9), Position::new(1, 10));
        assert_eq!(from_lsp(&text, Position::new(1, 10)), 12 + 9);
        // The middle of the surrogate pair rounds down to the emoji.
        assert_eq!(from_lsp(&text, Position::new(1, 9)), 12 + 8);
        assert_eq!(to_lsp(&text, text.len_chars()), Position::new(3, 0));
    }

    #[test]
    fn positions_past_the_end_are_clamped() {
        let text = Rope::from("ab\r\ncd\nef");
        // Past the end of the line: before its line break (CRLF included).
        assert_eq!(from_lsp(&text, Position::new(0, 99)), 2);
        assert_eq!(from_lsp(&text, Position::new(1, 99)), 6);
        assert_eq!(from_lsp(&text, Position::new(2, 99)), 9);
        // Past the last line: the end of the document.
        assert_eq!(from_lsp(&text, Position::new(3, 0)), 9);
        assert_eq!(from_lsp(&text, Position::new(99, 5)), 9);
        assert_eq!(to_lsp(&text, 99), Position::new(2, 2));
    }

    #[test]
    fn lone_cr_ends_a_line_for_both_rope_and_lsp() {
        let text = Rope::from("a\rb\r\nc");
        assert_eq!(to_lsp(&text, 2), Position::new(1, 0));
        assert_eq!(to_lsp(&text, 5), Position::new(2, 0));
        assert_eq!(from_lsp(&text, Position::new(2, 0)), 5);
    }

    #[test]
    fn rope_only_breaks_are_ordinary_characters_for_lsp() {
        // The rope sees five lines here, LSP two: FF and LS are inside line 0, and NEL in line 1.
        let text = Rope::from("a\u{c}b\u{2028}c\nd\u{85}e");
        assert_eq!(text.len_lines(), 5);
        assert_eq!(to_lsp(&text, 2), Position::new(0, 2));
        assert_eq!(to_lsp(&text, 4), Position::new(0, 4));
        assert_eq!(to_lsp(&text, 6), Position::new(1, 0));
        assert_eq!(to_lsp(&text, 9), Position::new(1, 3));
        assert_eq!(from_lsp(&text, Position::new(0, 4)), 4);
        assert_eq!(from_lsp(&text, Position::new(0, 99)), 5);
        assert_eq!(from_lsp(&text, Position::new(1, 2)), 8);
        assert_eq!(from_lsp(&text, Position::new(1, 99)), 9);
        assert_eq!(from_lsp(&text, Position::new(2, 0)), 9);
    }

    #[test]
    fn conversions_match_the_lsp_rules_on_random_texts() {
        let mut rng = Rng::new(7);
        for _ in 0..400 {
            let source = rng.text(60);
            let text = Rope::from(source.as_str());
            let lines = Lines::new(&text);
            let chars: Vec<char> = source.chars().collect();
            for pos in 0..=chars.len() {
                if inside_crlf(&chars, pos) {
                    continue;
                }
                let expected = reference_to_lsp(&source, pos);
                assert_eq!(lines.to_lsp(pos), expected, "{source:?} at {pos}");
                assert_eq!(lines.from_lsp(expected), pos, "{source:?} at {expected:?}");
            }
            for line in 0..reference_lines(&source).len() as u32 + 2 {
                for character in 0..12 {
                    let position = Position::new(line, character);
                    assert_eq!(
                        lines.from_lsp(position),
                        reference_from_lsp(&source, position),
                        "{source:?} at {position:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn extra_breaks_are_found_across_chunks() {
        // Long enough for several rope chunks.
        let source = "ab\u{2028}cd\u{85}é—\u{c}".repeat(500);
        let text = Rope::from(source.as_str());
        assert!(text.chunks().count() > 1);
        let expected: Vec<usize> = source
            .chars()
            .enumerate()
            .filter(|(_, c)| matches!(c, '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}'))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(extra_breaks(&text), expected);
    }

    #[test]
    fn uris_round_trip_with_escapes() {
        let path = Path::new("/Users/me/my project/100% [draft] #1/Ёлка ?.rs");
        let uri = uri_from_path(path);
        assert_eq!(
            uri.as_str(),
            "file:///Users/me/my%20project/100%25%20%5Bdraft%5D%20%231/%D0%81%D0%BB%D0%BA%D0%B0%20%3F.rs"
        );
        assert_eq!(path_from_uri(&uri).as_deref(), Some(path));
    }

    #[test]
    fn foreign_uris_have_no_path() {
        let uri = |s: &str| Uri::from_str(s).unwrap();
        assert_eq!(
            path_from_uri(&uri("file://localhost/tmp/a%20b.rs")),
            Some(PathBuf::from("/tmp/a b.rs"))
        );
        assert_eq!(
            path_from_uri(&uri("FILE:///tmp/x")),
            Some(PathBuf::from("/tmp/x"))
        );
        assert_eq!(path_from_uri(&uri("https://example.com/a.rs")), None);
        assert_eq!(path_from_uri(&uri("file://server/share/a.rs")), None);
        assert_eq!(path_from_uri(&uri("untitled:Untitled-1")), None);
    }
}
