//! Search in the terminal's scrollback and screen.
//!
//! alacritty's regex search runs a lazy DFA over the cells: a match never crosses a hard line break
//! but may continue through a line the terminal wrapped, and wide characters are covered whole.
//! The query semantics follow the file search (flux-search): literal unless `regex`, case ignored
//! unless `case_sensitive` (alacritty's own smart case is overridden either way), and a whole word
//! must not continue a word on either side (as with `rg -w`).

use std::fmt;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Direction, Point};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::search::{RegexIter, RegexSearch};

use crate::content::GridPoint;

/// At most this many matches are collected; the rest is reported as truncated.
pub(crate) const SEARCH_LIMIT: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

/// A match: its first and last cells (inclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    pub start: GridPoint,
    pub end: GridPoint,
}

/// Matches from the top of the scrollback to the bottom of the screen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchResults {
    pub matches: Vec<SearchMatch>,
    /// More than [`SEARCH_LIMIT`] matches: the list stops there.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchError {
    /// The regular expression doesn't compile; a single short line for the UI.
    InvalidRegex(String),
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SearchError::InvalidRegex(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for SearchError {}

/// All matches of `query` in the scrollback and on the screen, in order. An empty query finds
/// nothing.
pub(crate) fn search<T>(
    term: &Term<T>,
    query: &str,
    options: SearchOptions,
) -> Result<SearchResults, SearchError> {
    let mut results = SearchResults::default();
    if query.is_empty() {
        return Ok(results);
    }
    let mut regex = RegexSearch::new(&pattern(query, options))
        .map_err(|err| SearchError::InvalidRegex(short_message(&*err)))?;
    let start = Point::new(term.topmost_line(), Column(0));
    let end = Point::new(term.bottommost_line(), term.last_column());
    for found in RegexIter::new(start, end, Direction::Right, term, &mut regex) {
        let (first, last) = (*found.start(), *found.end());
        if options.whole_word && !is_whole_word(term, first, last) {
            continue;
        }
        if results.matches.len() == SEARCH_LIMIT {
            results.truncated = true;
            break;
        }
        results.matches.push(SearchMatch {
            start: GridPoint::new(first.line.0, first.column.0),
            end: GridPoint::new(last.line.0, last.column.0),
        });
    }
    Ok(results)
}

/// The regular expression for a query. alacritty ignores case when the pattern has no capitals;
/// the inline flag makes our choice explicit both ways.
fn pattern(query: &str, options: SearchOptions) -> String {
    let body = if options.regex {
        query.to_string()
    } else {
        escape(query)
    };
    let case = if options.case_sensitive {
        "(?-i)"
    } else {
        "(?i)"
    };
    format!("{case}(?:{body})")
}

/// A literal as a regular expression: the syntax characters are escaped.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// regex-syntax errors are several lines: the pattern, a `^` pointer, and an `error: …` line, deep
/// in the error's sources. The UI gets the gist.
fn short_message(err: &dyn std::error::Error) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(err) = source {
        text.push('\n');
        text.push_str(&err.to_string());
        source = err.source();
    }
    let reason = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("error:"))
        .next_back()
        .map(str::trim);
    match reason {
        Some(reason) => format!("regex parse error: {reason}"),
        None => text
            .lines()
            .next()
            .unwrap_or("invalid regular expression")
            .trim()
            .to_string(),
    }
}

/// The match doesn't continue a word: the characters right before its first cell and right after
/// its last one (across a wrapped line break) are not letters, digits, or `_`.
fn is_whole_word<T>(term: &Term<T>, first: Point, last: Point) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    !before(term, first).is_some_and(is_word) && !after(term, last).is_some_and(is_word)
}

/// The character in the cell before `point` on the same logical line.
fn before<T>(term: &Term<T>, point: Point) -> Option<char> {
    let grid = term.grid();
    let mut at = if point.column.0 > 0 {
        Point::new(point.line, point.column - 1)
    } else {
        let previous = point.line - 1;
        if previous < term.topmost_line() {
            return None;
        }
        let last = Point::new(previous, term.last_column());
        if !grid[last].flags.contains(Flags::WRAPLINE) {
            return None;
        }
        last
    };
    // The right half of a wide character: its character is in the cell to the left.
    if grid[at]
        .flags
        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        && at.column.0 > 0
    {
        at.column -= 1;
    }
    Some(grid[at].c)
}

/// The character in the cell after `point` on the same logical line.
fn after<T>(term: &Term<T>, point: Point) -> Option<char> {
    let grid = term.grid();
    let mut column = point.column.0 + 1;
    // The match ends on a wide character: skip its right half.
    if grid[point].flags.contains(Flags::WIDE_CHAR) {
        column += 1;
    }
    let at = if column <= term.last_column().0 {
        Point::new(point.line, Column(column))
    } else {
        let wrapped = grid[Point::new(point.line, term.last_column())]
            .flags
            .contains(Flags::WRAPLINE);
        let next = point.line + 1;
        if !wrapped || next > term.bottommost_line() {
            return None;
        }
        Point::new(next, Column(0))
    };
    let cell = &grid[at];
    // A wide character moved to the next line leaves a spacer at the end of this one.
    if cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
        return after(term, at);
    }
    Some(cell.c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::term::test::mock_term;

    fn find(content: &str, query: &str, options: SearchOptions) -> Vec<(i32, usize, i32, usize)> {
        let term = mock_term(content);
        search(&term, query, options)
            .unwrap()
            .matches
            .into_iter()
            .map(|m| (m.start.line, m.start.column, m.end.line, m.end.column))
            .collect()
    }

    fn literal() -> SearchOptions {
        SearchOptions::default()
    }

    #[test]
    fn literal_text_is_not_a_regex() {
        assert_eq!(find("a.b( axb(\r\n", "a.b(", literal()), [(0, 0, 0, 3)]);
    }

    #[test]
    fn case_is_ignored_unless_asked_including_cyrillic() {
        let text = "Привет привет Error error\r\n";
        assert_eq!(find(text, "привет", literal()).len(), 2);
        // Capitals don't turn on case sensitivity by themselves (alacritty's smart case).
        assert_eq!(find(text, "ERROR", literal()).len(), 2);
        let sensitive = SearchOptions {
            case_sensitive: true,
            ..literal()
        };
        assert_eq!(find(text, "error", sensitive), [(0, 20, 0, 24)]);
        assert_eq!(find(text, "Привет", sensitive), [(0, 0, 0, 5)]);
    }

    #[test]
    fn whole_word_needs_non_word_neighbours() {
        let word = SearchOptions {
            whole_word: true,
            ..literal()
        };
        assert_eq!(
            find("foo food foo_ (foo)\r\n", "foo", word),
            [(0, 0, 0, 2), (0, 15, 0, 17)]
        );
        // As with `rg -w`: non-word edges need non-word neighbours too.
        assert_eq!(find("a -> b a->b\r\n", "->", word), [(0, 2, 0, 3)]);
        assert_eq!(find("слово словом\r\n", "слово", word), [(0, 0, 0, 4)]);
    }

    #[test]
    fn whole_word_looks_across_wrapped_lines() {
        let word = SearchOptions {
            whole_word: true,
            ..literal()
        };
        // "abcfoo" is wrapped after "abc": "foo" continues a word.
        assert_eq!(find("abc\nfoo\r\n", "foo", word), []);
        assert_eq!(find("ab \nfoo\r\n", "foo", word), [(1, 0, 1, 2)]);
    }

    #[test]
    fn matches_cross_wrapped_lines_but_not_hard_breaks() {
        assert_eq!(find("hel\nlo\r\n", "hello", literal()), [(0, 0, 1, 1)]);
        assert_eq!(find("hel\r\nlo\r\n", "hello", literal()), []);
    }

    #[test]
    fn regex_and_its_errors() {
        let regex = SearchOptions {
            regex: true,
            ..literal()
        };
        assert_eq!(
            find("err42 err7 ok\r\n", r"err\d+", regex),
            [(0, 0, 0, 4), (0, 6, 0, 9)]
        );
        let term = mock_term("x\r\n");
        let err = search(&term, "(foo", regex).unwrap_err();
        assert_eq!(
            err,
            SearchError::InvalidRegex("regex parse error: unclosed group".into())
        );
        // Empty matches are skipped.
        assert_eq!(find("abc\r\n", "x*", regex), []);
    }

    #[test]
    fn wide_characters_are_covered_whole() {
        // 漢 and 字 take two columns each.
        assert_eq!(find("a漢字b\r\n", "漢字", literal()), [(0, 1, 0, 4)]);
        assert_eq!(find("a漢字b\r\n", "字b", literal()), [(0, 3, 0, 5)]);
    }

    #[test]
    fn empty_query_finds_nothing() {
        assert_eq!(find("abc\r\n", "", literal()), []);
    }

    #[test]
    fn matches_beyond_the_limit_are_truncated() {
        let line = format!("{}\r\n", "x".repeat(80));
        let content = line.repeat(130);
        let term = mock_term(&content);
        let results = search(&term, "x", literal()).unwrap();
        assert_eq!(results.matches.len(), SEARCH_LIMIT);
        assert!(results.truncated);
    }
}
