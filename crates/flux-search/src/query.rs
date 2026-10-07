//! Поисковый запрос: текст и флаги. Одна семантика для поиска в документе и по проекту —
//! оба строят матчер `grep_regex` из одного [`SearchQuery`].

use std::fmt;

use grep_regex::{RegexMatcher, RegexMatcherBuilder};

/// Что искать. Без `regex` текст ищется буквально; `whole_word` — вхождение не продолжает
/// слово ни слева, ни справа (как `rg -w`: шаблон `->` тоже ищется «целым словом»).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct SearchQuery {
    pub text: String,
    /// Без него регистр не учитывается, в том числе для кириллицы.
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// Регулярное выражение в синтаксисе крейта `regex` (как в ripgrep).
    pub regex: bool,
}

/// Запрос не собрался — обычно ошибка в регулярном выражении. `message` — одна короткая
/// строка для статуса в UI.
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
    /// Ошибки разбора у regex-syntax многострочные: шаблон, указатель `^` и строка
    /// `error: …`. Для статуса оставляем только суть.
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
    /// Буквальный поиск без учёта регистра.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Общие настройки. `multi_line` — это флаг `(?m)`: `^` и `$` — границы строк
    /// (ripgrep включает его всегда). `crlf` — `$` срабатывает и перед `\r\n`.
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

    /// Матчер для документа: вхождение может переходить через перевод строки
    /// (`foo\nbar`). `crlf(true)` ставит и терминатор строки — снимаем его, иначе
    /// `\n` в шаблоне был бы запрещён.
    pub(crate) fn buffer_matcher(&self) -> Result<RegexMatcher, QueryError> {
        let mut builder = self.builder();
        builder.line_terminator(None);
        Ok(builder.build(&self.text)?)
    }

    /// Построчный матчер для поиска по проекту: вхождение никогда не содержит `\r` и
    /// `\n` (терминатор CRLF — как у `rg --crlf`), литерал `\n` в шаблоне — ошибка.
    /// Поиск должен идти с тем же терминатором (`LineTerminator::crlf()`).
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
        // Шаблон с не-словесными символами по краям: `\b->\b` не нашёл бы отдельно
        // стоящую стрелку. Вплотную к буквам — не «целое слово» (как у `rg -w`).
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
        // `\s` в построчном поиске просто не захватывает перевод строки.
        let matcher = regex(r"a\s+").line_matcher().unwrap();
        assert_eq!(find_all(&matcher, "a  \r\nb"), ["a  "]);
    }
}
