//! LSP snippets (`insertTextFormat: Snippet`) → plain text with tab stops.
//!
//! The grammar is VS Code's: `$1`, `${1}`, `${1:default}` (nested), `${1|one,two|}`, `$0`,
//! variables `$NAME`, `${NAME}`, `${NAME:default}`, and transforms `${1/regex/format/flags}`.
//! Variables have no values here (there is no selection or clipboard to take them from): they
//! become their default text or nothing. Transforms are dropped. Anything that doesn't parse is
//! kept as literal text.

use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    /// Text with placeholders replaced by their default text, `$0`/`$1` removed, escapes resolved.
    pub text: String,
    /// Tab stops in order (`$1`, `$2`, …, then `$0`), as character ranges in `text`: a placeholder's
    /// default text, or an empty range. The first one is where the cursor (selection) goes. A
    /// snippet with tab stops but without `$0` gets one at the end of the text; a snippet without
    /// tab stops has none (the cursor goes to the end).
    pub tabstops: Vec<Range<usize>>,
}

pub fn parse(snippet: &str) -> Snippet {
    let mut parser = Parser {
        chars: snippet.chars().collect(),
        pos: 0,
        text: String::new(),
        len: 0,
        stops: Vec::new(),
    };
    parser.sequence(false);

    // The first occurrence of each number is its tab stop; mirrors (later occurrences) are not
    // tracked.
    let mut stops: Vec<(u32, Range<usize>)> = Vec::new();
    for (number, range) in parser.stops {
        if !stops.iter().any(|(n, _)| *n == number) {
            stops.push((number, range));
        }
    }
    let end = parser.len;
    if !stops.is_empty() && !stops.iter().any(|(n, _)| *n == 0) {
        stops.push((0, end..end));
    }
    // `$0` is the last stop.
    stops.sort_by_key(|(number, _)| if *number == 0 { u32::MAX } else { *number });
    Snippet {
        text: parser.text,
        tabstops: stops.into_iter().map(|(_, range)| range).collect(),
    }
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
    text: String,
    /// Length of `text` in characters.
    len: usize,
    stops: Vec<(u32, Range<usize>)>,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn push(&mut self, c: char) {
        self.text.push(c);
        self.len += 1;
    }

    /// Text and constructs up to the end, or, inside a placeholder, up to its unescaped `}` (not
    /// consumed).
    fn sequence(&mut self, nested: bool) {
        while let Some(c) = self.peek() {
            match c {
                '\\' => match self.chars.get(self.pos + 1) {
                    Some(&escaped @ ('$' | '}' | '\\')) => {
                        self.push(escaped);
                        self.pos += 2;
                    }
                    _ => {
                        self.push('\\');
                        self.pos += 1;
                    }
                },
                '}' if nested => return,
                '$' => {
                    let saved = (self.pos, self.text.len(), self.len, self.stops.len());
                    if !self.construct() {
                        // Not a construct: a literal '$'.
                        self.pos = saved.0;
                        self.text.truncate(saved.1);
                        self.len = saved.2;
                        self.stops.truncate(saved.3);
                        self.push('$');
                        self.pos += 1;
                    }
                }
                c => {
                    self.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    /// A construct starting at `$`; `false` if there is none.
    fn construct(&mut self) -> bool {
        self.pos += 1;
        if let Some(number) = self.number() {
            self.stops.push((number, self.len..self.len));
            return true;
        }
        if self.name().is_some() {
            // A variable without a value.
            return true;
        }
        if self.peek() != Some('{') {
            return false;
        }
        self.pos += 1;
        if let Some(number) = self.number() {
            let start = self.len;
            match self.peek() {
                Some('}') => {}
                Some(':') => {
                    self.pos += 1;
                    self.sequence(true);
                    if self.peek() != Some('}') {
                        return false;
                    }
                }
                Some('|') => {
                    self.pos += 1;
                    if !self.choice() {
                        return false;
                    }
                }
                Some('/') => {
                    if !self.transform() {
                        return false;
                    }
                }
                _ => return false,
            }
            self.pos += 1;
            self.stops.push((number, start..self.len));
            return true;
        }
        if self.name().is_some() {
            match self.peek() {
                Some('}') => {}
                // The default text stays.
                Some(':') => {
                    self.pos += 1;
                    self.sequence(true);
                    if self.peek() != Some('}') {
                        return false;
                    }
                }
                Some('/') => {
                    if !self.transform() {
                        return false;
                    }
                }
                _ => return false,
            }
            self.pos += 1;
            return true;
        }
        false
    }

    fn number(&mut self) -> Option<u32> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        let digits: String = self.chars[start..self.pos].iter().collect();
        digits.parse().ok().or_else(|| {
            self.pos = start;
            None
        })
    }

    fn name(&mut self) -> Option<()> {
        if !self
            .peek()
            .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        {
            return None;
        }
        while self
            .peek()
            .is_some_and(|c| c == '_' || c.is_ascii_alphanumeric())
        {
            self.pos += 1;
        }
        Some(())
    }

    /// `one,two|}` after `${1|`: inserts the first option; stops before the `}`.
    fn choice(&mut self) -> bool {
        let mut first = true;
        while let Some(c) = self.peek() {
            match c {
                '\\' => {
                    let escaped = match self.chars.get(self.pos + 1) {
                        Some(&e @ ('$' | '}' | '\\' | ',' | '|')) => {
                            self.pos += 2;
                            e
                        }
                        _ => {
                            self.pos += 1;
                            '\\'
                        }
                    };
                    if first {
                        self.push(escaped);
                    }
                }
                ',' => {
                    first = false;
                    self.pos += 1;
                }
                '|' => {
                    self.pos += 1;
                    return self.peek() == Some('}');
                }
                c => {
                    if first {
                        self.push(c);
                    }
                    self.pos += 1;
                }
            }
        }
        false
    }

    /// `/regex/format/flags` up to (not including) the closing `}`: skipped. The format may hold
    /// its own `${1:/upcase}`.
    fn transform(&mut self) -> bool {
        let mut slashes = 0;
        let mut depth = 0;
        while let Some(c) = self.peek() {
            match c {
                '\\' => self.pos += 2,
                '$' if self.chars.get(self.pos + 1) == Some(&'{') => {
                    depth += 1;
                    self.pos += 2;
                }
                '}' if depth > 0 => {
                    depth -= 1;
                    self.pos += 1;
                }
                '/' if depth == 0 => {
                    slashes += 1;
                    self.pos += 1;
                }
                '}' if slashes == 3 => return true,
                _ => self.pos += 1,
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snippet(text: &str, tabstops: &[(usize, usize)]) -> Snippet {
        Snippet {
            text: text.to_string(),
            tabstops: tabstops.iter().map(|&(a, b)| a..b).collect(),
        }
    }

    #[test]
    fn plain_text_has_no_tabstops() {
        assert_eq!(parse("println!"), snippet("println!", &[]));
    }

    #[test]
    fn tabstops_in_order_with_final_last() {
        assert_eq!(
            parse("for ${1:x} in $2 {\n\t$0\n}"),
            snippet("for x in  {\n\t\n}", &[(4, 5), (9, 9), (13, 13)])
        );
        // Without `$0`: one at the end.
        assert_eq!(parse("foo($1)"), snippet("foo()", &[(4, 4), (5, 5)]));
        assert_eq!(
            parse("${2:b}${1:a}"),
            snippet("ba", &[(1, 2), (0, 1), (2, 2)])
        );
    }

    #[test]
    fn nested_placeholders() {
        assert_eq!(
            parse("${1:foo ${2:bar}}$0"),
            snippet("foo bar", &[(0, 7), (4, 7), (7, 7)])
        );
    }

    #[test]
    fn choices_take_the_first_option() {
        assert_eq!(parse("${1|one,two|}"), snippet("one", &[(0, 3), (3, 3)]));
        assert_eq!(parse(r"${1|a\,b,c|}"), snippet("a,b", &[(0, 3), (3, 3)]));
    }

    #[test]
    fn variables_become_their_default_or_nothing() {
        assert_eq!(parse("[$TM_SELECTED_TEXT]"), snippet("[]", &[]));
        assert_eq!(parse("${TM_FILENAME}.rs"), snippet(".rs", &[]));
        assert_eq!(parse("${NAME:default}"), snippet("default", &[]));
        assert_eq!(parse("${NAME:a $1 b}"), snippet("a  b", &[(2, 2), (4, 4)]));
    }

    #[test]
    fn transforms_are_dropped() {
        assert_eq!(
            parse("${1/(.*)/${1:/upcase}/g}x"),
            snippet("x", &[(0, 0), (1, 1)])
        );
    }

    #[test]
    fn escapes() {
        assert_eq!(parse(r"\$1 \} \\ \n"), snippet(r"$1 } \ \n", &[]));
        assert_eq!(parse(r"${1:a\}b}"), snippet("a}b", &[(0, 3), (3, 3)]));
    }

    #[test]
    fn malformed_constructs_are_literal() {
        assert_eq!(parse("cost: $"), snippet("cost: $", &[]));
        assert_eq!(parse("${1:open"), snippet("${1:open", &[]));
        assert_eq!(parse("${ x }"), snippet("${ x }", &[]));
        assert_eq!(parse("a $ b"), snippet("a $ b", &[]));
        assert_eq!(parse("${1|a,b}"), snippet("${1|a,b}", &[]));
    }

    #[test]
    fn mirrors_use_the_first_occurrence() {
        assert_eq!(parse("${1:x} = $1;"), snippet("x = ;", &[(0, 1), (5, 5)]));
    }

    #[test]
    fn multibyte_text_counts_characters() {
        assert_eq!(parse("Я😀${1:é}"), snippet("Я😀é", &[(2, 3), (3, 3)]));
    }
}
