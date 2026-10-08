//! Search and replace in a document. Positions are `char` indices, as everywhere in flux.
//!
//! The functions are designed to run on a background thread: a snapshot of the rope (a clone is
//! O(1)) is gathered into a single `String`, because the matcher needs a contiguous byte slice.
//! This costs O(n) memory and time per search; it is noticeable for files of tens of megabytes
//! (tech debt).

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use flux_core::Rope;
use grep_matcher::{Captures, Matcher};
use grep_regex::RegexMatcher;

use crate::query::{QueryError, SearchQuery};

/// [`find_all`] does not collect more matches than this (`truncated`): highlighting and the "N of
/// M" counter are useless for a larger number. The limit does not apply to "replace all".
pub const MAX_BUFFER_MATCHES: usize = 100_000;

/// How often (counted in matches found) to check the cancellation flag.
const CANCEL_CHECK_EVERY: usize = 1024;

/// The matches of the query in the document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BufferMatches {
    /// Character ranges: ascending, non-overlapping, non-empty.
    pub ranges: Vec<Range<usize>>,
    /// There are more matches than [`MAX_BUFFER_MATCHES`]; only the first ones were collected.
    pub truncated: bool,
}

/// All matches of the query in the text. Empty matches (`^`, `a*`) are skipped. On cancellation
/// (`cancel`) it returns whatever was found so far; the result should be discarded.
pub fn find_all(
    text: &Rope,
    query: &SearchQuery,
    cancel: &AtomicBool,
) -> Result<BufferMatches, QueryError> {
    if query.is_empty() {
        return Ok(BufferMatches::default());
    }
    let matcher = query.buffer_matcher()?;
    let haystack = to_string(text);
    let mut chars = CharCounter::new(&haystack);
    let mut result = BufferMatches::default();
    if cancel.load(Ordering::Relaxed) {
        return Ok(result);
    }
    let _ = matcher.find_iter(haystack.as_bytes(), |m| {
        if m.is_empty() {
            return true;
        }
        if result.ranges.len() == MAX_BUFFER_MATCHES {
            result.truncated = true;
            return false;
        }
        if result.ranges.len() % CANCEL_CHECK_EVERY == 0 && cancel.load(Ordering::Relaxed) {
            return false;
        }
        result.ranges.push(chars.range(m.start()..m.end()));
        true
    });
    Ok(result)
}

/// The replacement text for the match `range` (from [`find_all`] for the same text and query). In
/// regular expression mode, groups are substituted: `$1`, `${1}`, `$name`, `${name}`, and `$$` for
/// a literal dollar sign; outside that mode, `replacement` is used as is. `None` means there is no
/// match in `range` anymore (the text has changed).
pub fn replacement_for(
    text: &Rope,
    query: &SearchQuery,
    range: Range<usize>,
    replacement: &str,
) -> Option<String> {
    if query.is_empty() || range.start >= range.end || range.end > text.len_chars() {
        return None;
    }
    let matcher = query.buffer_matcher().ok()?;
    let haystack = to_string(text);
    let bytes = text.char_to_byte(range.start)..text.char_to_byte(range.end);
    let mut caps = matcher.new_captures().ok()?;
    // Search starting at `bytes.start`, but with the full text around it: `^`, `$`, and word
    // boundaries look at the neighboring characters.
    if !matcher
        .captures_at(haystack.as_bytes(), bytes.start, &mut caps)
        .ok()?
    {
        return None;
    }
    let found = caps.get(0)?;
    if found.start() != bytes.start || found.end() != bytes.end {
        return None;
    }
    Some(expand(
        query,
        &matcher,
        &caps,
        haystack.as_bytes(),
        replacement,
    ))
}

/// All replacements at once, for a single "replace all" transaction: `(character range, text)` in
/// ascending order, without the [`MAX_BUFFER_MATCHES`] limit. Empty matches are skipped.
pub fn replace_all(
    text: &Rope,
    query: &SearchQuery,
    replacement: &str,
    cancel: &AtomicBool,
) -> Result<Vec<(Range<usize>, String)>, QueryError> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let matcher = query.buffer_matcher()?;
    let haystack = to_string(text);
    let mut chars = CharCounter::new(&haystack);
    let mut edits = Vec::new();
    if cancel.load(Ordering::Relaxed) {
        return Ok(edits);
    }
    let mut caps = matcher
        .new_captures()
        .expect("RegexMatcher always builds captures");
    let _ = matcher.captures_iter(haystack.as_bytes(), &mut caps, |caps| {
        let Some(m) = caps.get(0) else {
            return true;
        };
        if m.is_empty() {
            return true;
        }
        if edits.len() % CANCEL_CHECK_EVERY == 0 && cancel.load(Ordering::Relaxed) {
            return false;
        }
        let text = expand(query, &matcher, caps, haystack.as_bytes(), replacement);
        edits.push((chars.range(m.start()..m.end()), text));
        true
    });
    Ok(edits)
}

/// The replacement text for a found match: groups are substituted only in regex mode.
fn expand(
    query: &SearchQuery,
    matcher: &RegexMatcher,
    caps: &impl Captures,
    haystack: &[u8],
    replacement: &str,
) -> String {
    if !query.regex {
        return replacement.to_string();
    }
    let mut dst = Vec::with_capacity(replacement.len());
    caps.interpolate(
        |name| matcher.capture_index(name),
        haystack,
        replacement.as_bytes(),
        &mut dst,
    );
    String::from_utf8(dst).unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into())
}

fn to_string(text: &Rope) -> String {
    let mut string = String::with_capacity(text.len_bytes());
    for chunk in text.chunks() {
        string.push_str(chunk);
    }
    string
}

/// Converts byte offsets to character indices. The offsets must be in ascending order, so the whole
/// pass costs O(n).
pub(crate) struct CharCounter<'a> {
    text: &'a str,
    byte: usize,
    chars: usize,
}

impl<'a> CharCounter<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        Self {
            text,
            byte: 0,
            chars: 0,
        }
    }

    /// The index of the character that contains the byte `byte` (a byte in the middle of a
    /// character resolves to that character's start).
    pub(crate) fn char_at(&mut self, byte: usize) -> usize {
        let mut byte = byte.min(self.text.len());
        while !self.text.is_char_boundary(byte) {
            byte -= 1;
        }
        if byte < self.byte {
            // By construction we never go backwards; as a safeguard, recount from the start.
            self.byte = 0;
            self.chars = 0;
        }
        self.chars += self.text[self.byte..byte].chars().count();
        self.byte = byte;
        self.chars
    }

    /// Byte range → character range; an end in the middle of a character moves to the end of that
    /// character.
    pub(crate) fn range(&mut self, bytes: Range<usize>) -> Range<usize> {
        let start = self.char_at(bytes.start);
        let mut end = bytes.end.min(self.text.len());
        while !self.text.is_char_boundary(end) {
            end += 1;
        }
        start..self.char_at(end)
    }
}

#[cfg(test)]
// The ranges in the expectations are literally lists of ranges, not `(a..b).collect()`.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    fn rope(text: &str) -> Rope {
        Rope::from_str(text)
    }

    fn query(text: &str) -> SearchQuery {
        SearchQuery::new(text)
    }

    fn regex(text: &str) -> SearchQuery {
        SearchQuery {
            regex: true,
            ..SearchQuery::new(text)
        }
    }

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    fn found(text: &str, query: &SearchQuery) -> Vec<String> {
        let rope = rope(text);
        find_all(&rope, query, &no_cancel())
            .unwrap()
            .ranges
            .into_iter()
            .map(|r| rope.slice(r).to_string())
            .collect()
    }

    #[test]
    fn empty_query_finds_nothing() {
        let matches = find_all(&rope("abc"), &query(""), &no_cancel()).unwrap();
        assert_eq!(matches, BufferMatches::default());
    }

    #[test]
    fn positions_are_chars_not_bytes() {
        let text = rope("ы👍🏽 foo — ФОО foo");
        let matches = find_all(&text, &query("foo"), &no_cancel()).unwrap();
        assert_eq!(matches.ranges, [4..7, 14..17]);
        let cyrillic = find_all(&text, &query("фоо"), &no_cancel()).unwrap();
        assert_eq!(cyrillic.ranges, [10..13]);
    }

    #[test]
    fn multi_line_regex_crosses_newlines() {
        assert_eq!(found("a\nb\r\nc", &regex(r"a\nb")), ["a\nb"]);
        assert_eq!(found("a\nb\r\nc", &regex(r"b\r?\nc")), ["b\r\nc"]);
        // `^` and `$` are line boundaries, including with CRLF.
        assert_eq!(found("ab\r\nab\nxab", &regex("^ab$")), ["ab", "ab"]);
    }

    #[test]
    fn empty_matches_are_skipped() {
        assert!(found("abc\ndef", &regex("^")).is_empty());
        assert_eq!(found("baaac", &regex("a*")), ["aaa"]);
    }

    #[test]
    fn invalid_regex_is_an_error() {
        let err = find_all(&rope("x"), &regex("[a-"), &no_cancel()).unwrap_err();
        assert!(
            err.message.starts_with("regex parse error"),
            "{}",
            err.message
        );
    }

    #[test]
    fn matches_across_rope_chunk_boundaries() {
        // The rope splits text into chunks of ~1 KB; matches will also land on the seams.
        let unit = "абв needle где ";
        let text: String = unit.repeat(5_000);
        let rope = rope(&text);
        assert!(rope.chunks().count() > 10);
        let matches = find_all(&rope, &query("needle"), &no_cancel()).unwrap();
        assert_eq!(matches.ranges.len(), 5_000);
        let unit_chars = unit.chars().count();
        for (i, range) in matches.ranges.iter().enumerate() {
            assert_eq!(range.start, i * unit_chars + 4);
            assert_eq!(rope.slice(range.clone()), "needle");
        }
    }

    #[test]
    fn too_many_matches_are_truncated() {
        let text = rope(&"x".repeat(MAX_BUFFER_MATCHES + 5));
        let matches = find_all(&text, &query("x"), &no_cancel()).unwrap();
        assert!(matches.truncated);
        assert_eq!(matches.ranges.len(), MAX_BUFFER_MATCHES);
        let exact = rope(&"x".repeat(MAX_BUFFER_MATCHES));
        let matches = find_all(&exact, &query("x"), &no_cancel()).unwrap();
        assert!(!matches.truncated);
    }

    #[test]
    fn cancelled_search_stops_early() {
        let text = rope(&"x".repeat(10_000));
        let cancel = AtomicBool::new(true);
        let matches = find_all(&text, &query("x"), &cancel).unwrap();
        assert!(matches.ranges.len() < 10_000);
        let edits = replace_all(&text, &query("x"), "y", &cancel).unwrap();
        assert!(edits.len() < 10_000);
    }

    #[test]
    fn replacement_expands_groups_only_in_regex_mode() {
        let text = rope("let foo_bar = 1;");
        let q = regex(r"(?P<head>\w+)_(\w+)");
        let range = find_all(&text, &q, &no_cancel()).unwrap().ranges[0].clone();
        assert_eq!(range, 4..11);
        assert_eq!(
            replacement_for(&text, &q, range.clone(), "${2}_${head}").as_deref(),
            Some("bar_foo")
        );
        // The group name is the longest run of letters, digits, and `_`: `$2_` is the group "2_",
        // which does not exist.
        assert_eq!(
            replacement_for(&text, &q, range.clone(), "$2_$head").as_deref(),
            Some("foo")
        );
        assert_eq!(
            replacement_for(&text, &q, range.clone(), "${1}x $$1 $9").as_deref(),
            Some("foox $1 ")
        );
        let literal = query("foo_bar");
        assert_eq!(
            replacement_for(&text, &literal, range, "$1").as_deref(),
            Some("$1")
        );
    }

    #[test]
    fn stale_range_has_no_replacement() {
        let text = rope("foo bar foo");
        let q = query("foo");
        assert_eq!(replacement_for(&text, &q, 0..3, "x").as_deref(), Some("x"));
        assert_eq!(replacement_for(&text, &q, 1..4, "x"), None);
        assert_eq!(replacement_for(&text, &q, 4..7, "x"), None);
        assert_eq!(replacement_for(&text, &q, 8..12, "x"), None, "past the end");
        // Whole word: there is no match inside "food".
        let word = SearchQuery {
            whole_word: true,
            ..query("foo")
        };
        assert_eq!(replacement_for(&rope("food"), &word, 0..3, "x"), None);
    }

    #[test]
    fn replace_all_returns_every_edit_in_order() {
        let text = rope("a1 b22 c333");
        let edits = replace_all(&text, &regex(r"(\w)(\d+)"), "$2$1", &no_cancel()).unwrap();
        assert_eq!(
            edits,
            [
                (0..2, "1a".to_string()),
                (3..6, "22b".to_string()),
                (7..11, "333c".to_string())
            ]
        );
        let literal = replace_all(&text, &query("3"), "$", &no_cancel()).unwrap();
        assert_eq!(literal.len(), 3);
        assert!(literal.iter().all(|(_, text)| text == "$"));
        assert!(
            replace_all(&text, &query(""), "x", &no_cancel())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn replace_all_ignores_the_match_limit() {
        let text = rope(&"x".repeat(MAX_BUFFER_MATCHES + 3));
        let edits = replace_all(&text, &query("x"), "y", &no_cancel()).unwrap();
        assert_eq!(edits.len(), MAX_BUFFER_MATCHES + 3);
    }

    #[test]
    fn char_counter_handles_any_offsets() {
        let text = "aы👍b";
        let mut chars = CharCounter::new(text);
        assert_eq!(chars.char_at(0), 0);
        assert_eq!(chars.char_at(1), 1);
        assert_eq!(chars.char_at(2), 1, "middle of «ы»");
        assert_eq!(chars.char_at(3), 2);
        assert_eq!(chars.range(4..6), 2..3, "end inside 👍 goes to its end");
        assert_eq!(chars.char_at(1), 1, "going back recounts");
        assert_eq!(chars.char_at(100), 4);
    }
}
