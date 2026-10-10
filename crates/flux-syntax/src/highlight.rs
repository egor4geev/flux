//! Highlighting: query captures → theme scopes → line spans.
//!
//! There are no colors here. The theme (in flux-app) is a list of scopes (`keyword`,
//! `function.method`…). [`HighlightMap`] translates a capture index into a scope index once per
//! language and theme, so there are no string operations per frame.
//!
//! Overlaps are resolved as in tree-sitter-highlight: a capture that comes later in cursor order
//! (by start, then by pattern number) paints its span over the earlier ones. A nested capture
//! overrides the outer one on its own span; for an identical range the later pattern wins (for some
//! languages, the earlier one; see `Precedence`).

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use flux_core::Rope;
use flux_core::text::line_ending_len;
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator, Tree};

use crate::language::{Language, Precedence};
use crate::text::{CharCounter, byte_chunks};

/// How many unfinished matches the query cursor holds. A guard against pathological files, as in
/// Helix: excess matches are dropped.
const MATCH_LIMIT: u32 = 256;

/// A theme scope index: the position of the name in the list passed to [`HighlightMap::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Highlight(pub usize);

/// A highlighted span of a line: the character columns `start..end` (end exclusive) within the
/// line, excluding its line break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub highlight: Highlight,
}

/// The mapping "capture index → theme scope" for one language and one theme.
#[derive(Debug, Clone)]
pub struct HighlightMap {
    language: Arc<Language>,
    by_capture: Box<[Option<Highlight>]>,
}

impl HighlightMap {
    /// `scopes` are the theme scopes. A capture name is looked up in full first, then by falling
    /// back along the dots: `function.method.builtin` → `function.method` → `function`. A capture
    /// with no match is not highlighted and does not take part in overlap resolution. Names that
    /// start with `_` are internal and are never highlighted.
    ///
    /// Needs the language's compiled query: if there is none yet, compiles it (up to ~20 ms).
    /// Convenient to build after the first [`ParseResult`](crate::ParseResult): by then the
    /// background [`ParseJob::run`](crate::ParseJob::run) has compiled the query.
    pub fn new(language: &Arc<Language>, scopes: &[impl AsRef<str>]) -> Self {
        Self::build(language, language.capture_names(), scopes)
    }

    /// Like [`HighlightMap::new`], but only if the language's query is already compiled (for
    /// example, by a background [`ParseJob::run`](crate::ParseJob::run)); otherwise `None`. Never
    /// compiles the query; intended for the UI thread.
    pub fn try_new(language: &Arc<Language>, scopes: &[impl AsRef<str>]) -> Option<Self> {
        let query = language.compiled_query()?;
        Some(Self::build(language, query.capture_names(), scopes))
    }

    fn build(language: &Arc<Language>, capture_names: &[&str], scopes: &[impl AsRef<str>]) -> Self {
        let mut index = HashMap::with_capacity(scopes.len());
        for (i, scope) in scopes.iter().enumerate() {
            index.entry(scope.as_ref()).or_insert(Highlight(i));
        }
        let by_capture = capture_names
            .iter()
            .map(|name| resolve(name, &index))
            .collect();
        Self {
            language: language.clone(),
            by_capture,
        }
    }

    pub fn language(&self) -> &Arc<Language> {
        &self.language
    }

    /// The scope for the capture with index `capture`.
    pub fn get(&self, capture: u32) -> Option<Highlight> {
        self.by_capture.get(capture as usize).copied().flatten()
    }
}

fn resolve(capture: &str, scopes: &HashMap<&str, Highlight>) -> Option<Highlight> {
    if capture.starts_with('_') {
        return None;
    }
    let mut name = capture;
    loop {
        if let Some(&highlight) = scopes.get(name) {
            return Some(highlight);
        }
        name = name.rsplit_once('.')?.0;
    }
}

/// Spans for the lines `lines` (ropey lines, as in the editor). The result is one vector per
/// existing line of the range: lines past the end of the text are dropped.
///
/// Also works with a tree that has been edited but not yet parsed: node offsets are clamped to the
/// text length and to character boundaries.
pub(crate) fn highlight_lines(
    tree: Option<&Tree>,
    text: &Rope,
    lines: Range<usize>,
    map: &HighlightMap,
) -> Vec<Vec<HighlightSpan>> {
    let last = lines.end.min(text.len_lines());
    let first = lines.start.min(last);
    let mut result = vec![Vec::new(); last - first];
    // Without a tree we do not touch the query: compiling it (up to ~20 ms) must not happen on the
    // UI thread before the first parse; the background ParseJob::run does it.
    let Some(tree) = tree else {
        return result;
    };
    let language = &map.language;
    let Some(query) = language.query() else {
        return result;
    };
    let range = text.line_to_byte(first)..text.line_to_byte(last);
    if range.is_empty() {
        return result;
    }
    let spans = flat_spans(tree, query, language.precedence(), text, range, map);
    split_into_lines(text, first..last, &spans, &mut result);
    result
}

/// A capture with an already resolved scope, in bytes.
#[derive(Debug, Clone, Copy)]
struct Capture {
    start: usize,
    end: usize,
    /// Tree node (`Node::id`).
    node: usize,
    highlight: Highlight,
}

/// A span without overlaps, in bytes.
type FlatSpan = (Range<usize>, Highlight);

/// Non-overlapping sorted spans within `range`.
fn flat_spans(
    tree: &Tree,
    query: &Query,
    precedence: Precedence,
    text: &Rope,
    range: Range<usize>,
    map: &HighlightMap,
) -> Vec<FlatSpan> {
    let len = text.len_bytes();
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(MATCH_LIMIT);
    cursor.set_byte_range(range.clone());
    let text_provider = |node: Node| byte_chunks(text, node.byte_range());
    let mut matches = cursor.captures(query, tree.root_node(), text_provider);
    let mut captures = Vec::new();
    while let Some((m, index)) = matches.next() {
        let capture = m.captures()[*index];
        let Some(highlight) = map.get(capture.index) else {
            continue;
        };
        let node = capture.node.byte_range();
        let end = node.end.min(len);
        let start = node.start.min(end);
        if start < end && start < range.end && end > range.start {
            captures.push(Capture {
                start,
                end,
                node: capture.node.id(),
                highlight,
            });
        }
    }
    resolve_overlaps(captures, precedence, range)
}

/// Resolves overlaps as tree-sitter-highlight does. `captures` are in query cursor order: by start,
/// and for equal starts, by pattern number. Each capture "paints" its own span on top of all the
/// previous ones, so:
/// - a nested capture that starts later overrides the enclosing one on its own span;
/// - among identical ranges, the later pattern wins;
/// - with a shared start, an enclosing capture from a later pattern covers the nested one: this is
///   how `(pair (bare_key)) @property` recolors the key in TOML.
///
/// For queries written for "early pattern wins" ([`Precedence::FirstPattern`]), only the first of
/// consecutive captures of the same node is kept, as in tree-sitter-highlight before 0.21.
fn resolve_overlaps(
    mut captures: Vec<Capture>,
    precedence: Precedence,
    range: Range<usize>,
) -> Vec<FlatSpan> {
    if precedence == Precedence::FirstPattern {
        captures.dedup_by(|later, kept| later.node == kept.node);
    }
    // The cursor already yields captures by start; the stable sort merely serves as a safety net,
    // leaving the order of captures that share a start unchanged.
    captures.sort_by_key(|capture| capture.start);

    let mut spans = Vec::with_capacity(captures.len());
    // Started spans in start order: the top one is the visible one. Ended ones are popped once they
    // reach the top.
    let mut stack: Vec<(usize, Highlight)> = Vec::new();
    let mut pos = range.start;
    let mut next = 0;
    loop {
        while let Some(capture) = captures.get(next)
            && capture.start <= pos
        {
            stack.push((capture.end, capture.highlight));
            next += 1;
        }
        while stack.last().is_some_and(|&(end, _)| end <= pos) {
            stack.pop();
        }
        // The next boundary: the start of the next capture or the end of the visible one.
        let boundary = match (captures.get(next), stack.last()) {
            (None, None) => break,
            (Some(capture), None) => capture.start,
            (None, Some(&(end, _))) => end,
            (Some(capture), Some(&(end, _))) => capture.start.min(end),
        };
        if let Some(&(_, highlight)) = stack.last() {
            push_span(&mut spans, pos..boundary, highlight, &range);
        }
        pos = boundary;
    }
    spans
}

/// Adds a span clipped to `range`; merges it with an adjacent one that has the same scope.
fn push_span(
    spans: &mut Vec<FlatSpan>,
    span: Range<usize>,
    highlight: Highlight,
    range: &Range<usize>,
) {
    let start = span.start.max(range.start);
    let end = span.end.min(range.end);
    if start >= end {
        return;
    }
    if let Some((last, last_highlight)) = spans.last_mut()
        && last.end == start
        && *last_highlight == highlight
    {
        last.end = end;
        return;
    }
    spans.push((start..end, highlight));
}

/// Splits byte spans by lines and converts them to character columns.
fn split_into_lines(
    text: &Rope,
    lines: Range<usize>,
    spans: &[FlatSpan],
    result: &mut [Vec<HighlightSpan>],
) {
    let mut counter = CharCounter::new(text, text.line_to_byte(lines.start));
    let mut next = 0;
    for (line, line_spans) in lines.zip(result.iter_mut()) {
        let line_start = text.line_to_byte(line);
        let content_end = content_end_byte(text, line);
        let line_column = counter.chars_to(line_start);
        while next < spans.len() && spans[next].0.end <= line_start {
            next += 1;
        }
        for (span, highlight) in &spans[next..] {
            if span.start >= content_end {
                break;
            }
            let start = span.start.max(line_start);
            let end = span.end.min(content_end);
            if start >= end {
                continue;
            }
            let start = counter.chars_to(start) - line_column;
            let end = counter.chars_to(end) - line_column;
            if start < end {
                line_spans.push(HighlightSpan {
                    start,
                    end,
                    highlight: *highlight,
                });
            }
        }
    }
}

/// Byte offset of the end of the line's content, before its line break.
fn content_end_byte(text: &Rope, line: usize) -> usize {
    let slice = text.line(line);
    let len = slice.len_chars();
    let ending: usize = (len - line_ending_len(slice)..len)
        .map(|i| slice.char(i).len_utf8())
        .sum();
    text.line_to_byte(line + 1) - ending
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::language_by_name;

    const KEYWORD: Highlight = Highlight(0);
    const STRING: Highlight = Highlight(1);
    const COMMENT: Highlight = Highlight(2);

    /// A capture of node `node`; in the tests a node is just a number.
    fn capture(range: Range<usize>, node: usize, highlight: Highlight) -> Capture {
        Capture {
            start: range.start,
            end: range.end,
            node,
            highlight,
        }
    }

    #[test]
    fn falls_back_by_dots() {
        let scopes = HashMap::from([
            ("function", Highlight(0)),
            ("function.method", Highlight(1)),
        ]);
        assert_eq!(
            resolve("function.method.builtin", &scopes),
            Some(Highlight(1))
        );
        assert_eq!(resolve("function.builtin", &scopes), Some(Highlight(0)));
        assert_eq!(resolve("function", &scopes), Some(Highlight(0)));
        assert_eq!(resolve("keyword", &scopes), None);
        assert_eq!(resolve("_private", &scopes), None);
        assert_eq!(resolve("", &scopes), None);
    }

    #[test]
    fn map_follows_query_capture_order() {
        crate::standard::register();
        let rust = language_by_name("rust").unwrap();
        let map = HighlightMap::new(
            &rust,
            &["keyword", "function", "function.method", "keyword"],
        );
        let names = rust.capture_names();
        for (i, name) in names.iter().enumerate() {
            let expected = match *name {
                "keyword" => Some(Highlight(0)),
                "function.method" => Some(Highlight(2)),
                n if n.starts_with("function") => Some(Highlight(1)),
                _ => None,
            };
            assert_eq!(map.get(i as u32), expected, "{name}");
        }
        assert_eq!(map.get(names.len() as u32 + 5), None);
    }

    #[test]
    fn nested_capture_wins_on_its_part() {
        // In cursor order: by start.
        let spans = resolve_overlaps(
            vec![
                capture(0..10, 1, KEYWORD),
                capture(2..4, 2, STRING),
                capture(5..7, 3, COMMENT),
            ],
            Precedence::LastPattern,
            0..100,
        );
        assert_eq!(
            spans,
            vec![
                (0..2, KEYWORD),
                (2..4, STRING),
                (4..5, KEYWORD),
                (5..7, COMMENT),
                (7..10, KEYWORD),
            ]
        );
    }

    #[test]
    fn same_node_goes_by_precedence() {
        // One node, two patterns: the cursor yields them by pattern number.
        let captures = vec![capture(3..8, 1, STRING), capture(3..8, 1, COMMENT)];
        let last = resolve_overlaps(captures.clone(), Precedence::LastPattern, 0..100);
        assert_eq!(last, vec![(3..8, COMMENT)]);
        let first = resolve_overlaps(captures, Precedence::FirstPattern, 0..100);
        assert_eq!(first, vec![(3..8, STRING)]);
    }

    #[test]
    fn common_start_goes_by_cursor_order() {
        // The nested node has the earlier pattern: the enclosing one paints over it entirely.
        let spans = resolve_overlaps(
            vec![capture(0..4, 1, STRING), capture(0..10, 2, KEYWORD)],
            Precedence::LastPattern,
            0..100,
        );
        assert_eq!(spans, vec![(0..10, KEYWORD)]);
        // The enclosing one comes first: the nested one is visible on its own span.
        let spans = resolve_overlaps(
            vec![capture(0..10, 2, KEYWORD), capture(0..4, 1, STRING)],
            Precedence::LastPattern,
            0..100,
        );
        assert_eq!(spans, vec![(0..4, STRING), (4..10, KEYWORD)]);
    }

    #[test]
    fn spans_are_clipped_and_merged() {
        let spans = resolve_overlaps(
            vec![
                capture(0..5, 1, KEYWORD),
                capture(5..9, 2, KEYWORD),
                capture(9..30, 3, STRING),
                capture(12..14, 4, STRING),
            ],
            Precedence::LastPattern,
            3..20,
        );
        assert_eq!(spans, vec![(3..9, KEYWORD), (9..20, STRING)]);
    }

    #[test]
    fn partial_overlap_paints_in_order() {
        // This cannot happen in a parsed tree, but can in a stale one.
        let spans = resolve_overlaps(
            vec![
                capture(0..5, 1, KEYWORD),
                capture(3..8, 2, STRING),
                capture(6..9, 3, COMMENT),
            ],
            Precedence::LastPattern,
            0..100,
        );
        assert_eq!(
            spans,
            vec![(0..3, KEYWORD), (3..6, STRING), (6..9, COMMENT)]
        );
    }

    #[test]
    fn buried_capture_does_not_resurface_after_its_end() {
        // A is outside, B is inside, C starts inside B and extends past A.
        let spans = resolve_overlaps(
            vec![
                capture(0..10, 1, KEYWORD),
                capture(2..6, 2, STRING),
                capture(4..12, 3, COMMENT),
            ],
            Precedence::LastPattern,
            0..100,
        );
        assert_eq!(
            spans,
            vec![(0..2, KEYWORD), (2..4, STRING), (4..12, COMMENT)]
        );
    }

    #[test]
    fn line_content_excludes_any_line_ending() {
        let text = Rope::from_str("ab\r\nв\nc\u{2028}d\re\u{85}");
        let ends: Vec<usize> = (0..text.len_lines())
            .map(|l| content_end_byte(&text, l))
            .collect();
        assert_eq!(ends, vec![2, 6, 8, 12, 14, 16]);
    }
}
