//! A search query: text and flags. The semantics are the same for in-document search and project
//! search: both build a `grep_regex` matcher from the same [`SearchQuery`].

use std::fmt;

use grep_regex::{RegexMatcher, RegexMatcherBuilder};

/// What to search for. Without `regex` the text is matched literally; with `whole_word`, a match
/// must not continue a word on either the left or the right (as with `rg -w`: the pattern `->` is
/// also searched as a "whole word").
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct SearchQuery {
    pub text: String,
    /// Without it, case is ignored, including for Cyrillic.
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// A regular expression in the syntax of the `regex` crate (as in ripgrep).
    pub regex: bool,
}

/// The query could not be built, usually because of an error in the regular expression. `message`
/// is a single short line for the status in the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryError {
    pub message: String,
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for QueryError {}

impl From<grep_regex::Error> for QueryError {
    /// regex-syntax parse errors are multi-line: the pattern, a `^` pointer and an `error: …` line.
    /// For the status we keep only the gist.
    fn from(err: grep_regex::Error) -> Self {
        let text = err.to_string();
        let reason = text
            .lines()
            .filter_map(|line| line.trim().strip_prefix("error:"))
            .next_back()
            .map(str::trim);
        let message = match reason {
            Some(reason) => format!("regex parse error: {reason}"),
            None => text
                .lines()
                .next()
                .unwrap_or("invalid query")
                .trim()
                .to_string(),
        };
        Self { message }
    }
}

impl SearchQuery {
    /// Literal, case-insensitive search.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Common settings. `multi_line` is the `(?m)` flag: `^` and `$` match at line boundaries
    /// (ripgrep always enables it). With `crlf`, `$` also matches before `\r\n`.
    fn builder(&self) -> RegexMatcherBuilder {
        let mut builder = RegexMatcherBuilder::new();
        builder
            .case_insensitive(!self.case_sensitive)
            .word(self.whole_word)
            .fixed_strings(!self.regex)
            .multi_line(true)
            .crlf(true);
        builder
    }

    /// Matcher for a document: a match may cross a line break (`foo\nbar`). `crlf(true)` also sets
    /// the line terminator, so we clear it; otherwise `\n` in the pattern would be forbidden.
    pub(crate) fn buffer_matcher(&self) -> Result<RegexMatcher, QueryError> {
        let mut builder = self.builder();
        builder.line_terminator(None);
        Ok(builder.build(&self.text)?)
    }

    /// Line-by-line matcher for project search: a match never contains `\r` or `\n` (CRLF
    /// terminator, as with `rg --crlf`), and a literal `\n` in the pattern is an error. The search
    /// must use the same terminator (`LineTerminator::crlf()`).
    pub(crate) fn line_matcher(&self) -> Result<RegexMatcher, QueryError> {
        Ok(self.builder().build(&self.text)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grep_matcher::Matcher;

    fn regex(text: &str) -> SearchQuery {
        SearchQuery {
            regex: true,
            ..SearchQuery::new(text)
        }
    }

    fn find_all(matcher: &RegexMatcher, haystack: &str) -> Vec<String> {
        let mut found = Vec::new();
        matcher
            .find_iter(haystack.as_bytes(), |m| {
                found.push(haystack[m.start()..m.end()].to_string());
                true
            })
            .unwrap();
        found
    }

    #[test]
    fn literal_text_is_not_a_regex() {
        let matcher = SearchQuery::new("a.b(").buffer_matcher().unwrap();
        assert_eq!(find_all(&matcher, "axb( a.b("), ["a.b("]);
    }

    #[test]
    fn case_is_ignored_unless_asked_including_cyrillic() {
        let query = SearchQuery::new("Привет");
        let matcher = query.buffer_matcher().unwrap();
        assert_eq!(find_all(&matcher, "привет ПРИВЕТ"), ["привет", "ПРИВЕТ"]);
        let sensitive = SearchQuery {
            case_sensitive: true,
            ..query
        };
        let matcher = sensitive.buffer_matcher().unwrap();
        assert!(find_all(&matcher, "привет ПРИВЕТ").is_empty());
    }

    #[test]
    fn whole_word_needs_non_word_neighbours_only() {
        let word = |text: &str| SearchQuery {
            whole_word: true,
            ..SearchQuery::new(text)
        };
        let matcher = word("foo").buffer_matcher().unwrap();
        assert_eq!(find_all(&matcher, "foo food foo_ (foo)"), ["foo", "foo"]);
        // A pattern with non-word characters at its edges: `\b->\b` wouldn't find a standalone
        // arrow. Right next to letters it is not a "whole word" (as with `rg -w`).
        let matcher = word("->").buffer_matcher().unwrap();
        assert_eq!(find_all(&matcher, "a -> b, (->), a->b"), ["->", "->"]);
    }

    #[test]
    fn bad_regex_is_a_short_error() {
        let err = regex("(foo").buffer_matcher().unwrap_err();
        assert_eq!(err.message, "regex parse error: unclosed group");
        assert!(!err.to_string().contains('\n'));
    }

    #[test]
    fn dollar_matches_before_crlf() {
        let matcher = regex("b$").buffer_matcher().unwrap();
        assert_eq!(find_all(&matcher, "ab\r\nab\nab"), ["b", "b", "b"]);
        let matcher = regex("b$").line_matcher().unwrap();
        assert_eq!(find_all(&matcher, "ab\r\n"), ["b"]);
    }

    #[test]
    fn newline_is_allowed_only_in_buffer_search() {
        assert!(regex(r"a\nb").buffer_matcher().is_ok());
        let err = regex(r"a\nb").line_matcher().unwrap_err();
        assert!(err.message.contains("not allowed"), "{}", err.message);
        // In line-by-line search, `\s` simply doesn't capture a newline.
        let matcher = regex(r"a\s+").line_matcher().unwrap();
        assert_eq!(find_all(&matcher, "a  \r\nb"), ["a  "]);
    }
}
