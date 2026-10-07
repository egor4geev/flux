//! Подсветка: capture запроса → области темы → спаны строк.
//!
//! Цветов здесь нет. Тема (во flux-app) — это список областей (`keyword`,
//! `function.method`…). [`HighlightMap`] один раз на язык и тему переводит
//! индекс capture в индекс области, так что на кадре нет строковых операций.
//!
//! Перекрытия разрешаются как в tree-sitter-highlight: capture, идущий позже
//! в порядке курсора (по началу, затем по номеру паттерна), закрашивает свой
//! участок поверх предыдущих. Вложенный capture перекрывает внешний на своём
//! участке, для одинакового диапазона побеждает поздний паттерн (у части
//! языков — ранний, см. `Precedence`).

use std::collections::HashMap;
use std::ops::Range;

use flux_core::Rope;
use flux_core::text::line_ending_len;
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator, Tree};

use crate::language::{Language, Precedence};
use crate::text::{CharCounter, byte_chunks};

/// Сколько незавершённых совпадений держит курсор запроса. Защита от
/// патологических файлов, как в Helix: лишние совпадения отбрасываются.
const MATCH_LIMIT: u32 = 256;

/// Индекс области темы — позиция имени в списке, переданном в [`HighlightMap::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Highlight(pub usize);

/// Подсвеченный участок строки: символьные колонки `start..end` (конец
/// не включается) внутри строки, без её перевода строки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub highlight: Highlight,
}

/// Отображение «индекс capture → область темы» для одного языка и одной темы.
#[derive(Debug, Clone)]
pub struct HighlightMap {
    language: &'static Language,
    by_capture: Box<[Option<Highlight>]>,
}

impl HighlightMap {
    /// `scopes` — области темы. Имя capture ищется целиком, затем с откатом
    /// по точкам: `function.method.builtin` → `function.method` → `function`.
    /// Capture без совпадения не подсвечивается и не участвует в разрешении
    /// перекрытий. Имена с `_` в начале — служебные, не подсвечиваются никогда.
    ///
    /// Нужен скомпилированный запрос языка: если его ещё нет, компилирует
    /// (до ~20 мс). Удобно строить после первого [`ParseResult`](crate::ParseResult):
    /// фоновый [`ParseJob::run`](crate::ParseJob::run) к тому времени запрос скомпилировал.
    pub fn new(language: &'static Language, scopes: &[impl AsRef<str>]) -> Self {
        Self::build(language, language.capture_names(), scopes)
    }

    /// Как [`HighlightMap::new`], но только если запрос языка уже скомпилирован
    /// (например, фоновым [`ParseJob::run`](crate::ParseJob::run)); иначе `None`.
    /// Никогда не компилирует запрос — для UI-потока.
    pub fn try_new(language: &'static Language, scopes: &[impl AsRef<str>]) -> Option<Self> {
        let query = language.compiled_query()?;
        Some(Self::build(language, query.capture_names(), scopes))
    }

    fn build(
        language: &'static Language,
        capture_names: &[&str],
        scopes: &[impl AsRef<str>],
    ) -> Self {
        let mut index = HashMap::with_capacity(scopes.len());
        for (i, scope) in scopes.iter().enumerate() {
            index.entry(scope.as_ref()).or_insert(Highlight(i));
        }
        let by_capture = capture_names
            .iter()
            .map(|name| resolve(name, &index))
            .collect();
        Self {
            language,
            by_capture,
        }
    }

    pub fn language(&self) -> &'static Language {
        self.language
    }

    /// Область для capture с индексом `capture`.
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

/// Спаны для строк `lines` (строки ropey, как у редактора). Результат —
/// по вектору на каждую существующую строку диапазона: строки за концом
/// текста отбрасываются.
///
/// Работает и с деревом, которое отредактировано, но ещё не разобрано:
/// смещения узлов обрезаются по длине текста и границам символов.
pub(crate) fn highlight_lines(
    tree: Option<&Tree>,
    text: &Rope,
    lines: Range<usize>,
    map: &HighlightMap,
) -> Vec<Vec<HighlightSpan>> {
    let last = lines.end.min(text.len_lines());
    let first = lines.start.min(last);
    let mut result = vec![Vec::new(); last - first];
    // Без дерева запрос не трогаем: его компиляция (до ~20 мс) не должна
    // случиться в UI-потоке до первого разбора — её делает фоновый ParseJob::run.
    let Some(tree) = tree else {
        return result;
    };
    let language = map.language;
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

/// Capture с уже разрешённой областью, в байтах.
#[derive(Debug, Clone, Copy)]
struct Capture {
    start: usize,
    end: usize,
    /// Узел дерева (`Node::id`).
    node: usize,
    highlight: Highlight,
}

/// Участок без перекрытий, в байтах.
type FlatSpan = (Range<usize>, Highlight);

/// Непересекающиеся отсортированные участки внутри `range`.
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

/// Разрешает перекрытия как tree-sitter-highlight. `captures` — в порядке
/// курсора запроса: по началу, при равном начале — по номеру паттерна.
/// Каждый capture «закрашивает» свой участок поверх всех предыдущих, поэтому:
/// - вложенный capture, начавшийся позже, перекрывает объемлющий на своём участке;
/// - из одинаковых диапазонов побеждает поздний паттерн;
/// - при общем начале объемлющий capture более позднего паттерна закрывает
///   вложенный: так в TOML `(pair (bare_key)) @property` перекрашивает ключ.
///
/// Для запросов «под ранний паттерн» ([`Precedence::FirstPattern`]) из идущих
/// подряд capture одного узла остаётся первый — как в tree-sitter-highlight до 0.21.
fn resolve_overlaps(
    mut captures: Vec<Capture>,
    precedence: Precedence,
    range: Range<usize>,
) -> Vec<FlatSpan> {
    if precedence == Precedence::FirstPattern {
        captures.dedup_by(|later, kept| later.node == kept.node);
    }
    // Курсор и так отдаёт capture по началу; устойчивая сортировка лишь
    // страхует это, не меняя порядок capture с общим началом.
    captures.sort_by_key(|capture| capture.start);

    let mut spans = Vec::with_capacity(captures.len());
    // Начатые участки в порядке начала: верхний — видимый. Кончившиеся
    // снимаются, когда оказываются сверху.
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
        // Следующая граница: начало следующего capture или конец видимого.
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

/// Добавляет участок, обрезанный по `range`; соседний с той же областью — сливает.
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

/// Режет байтовые участки по строкам и переводит в символьные колонки.
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

/// Байт конца содержимого строки — перед её переводом строки.
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

    /// Capture узла `node`; в тестах узел — просто номер.
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
        let rust = language_by_name("rust").unwrap();
        let map = HighlightMap::new(rust, &["keyword", "function", "function.method", "keyword"]);
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
        // В порядке курсора: по началу.
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
        // Один узел, два паттерна: курсор отдаёт их по номеру паттерна.
        let captures = vec![capture(3..8, 1, STRING), capture(3..8, 1, COMMENT)];
        let last = resolve_overlaps(captures.clone(), Precedence::LastPattern, 0..100);
        assert_eq!(last, vec![(3..8, COMMENT)]);
        let first = resolve_overlaps(captures, Precedence::FirstPattern, 0..100);
        assert_eq!(first, vec![(3..8, STRING)]);
    }

    #[test]
    fn common_start_goes_by_cursor_order() {
        // Вложенный узел раньше по паттерну — объемлющий закрашивает его целиком.
        let spans = resolve_overlaps(
            vec![capture(0..4, 1, STRING), capture(0..10, 2, KEYWORD)],
            Precedence::LastPattern,
            0..100,
        );
        assert_eq!(spans, vec![(0..10, KEYWORD)]);
        // Объемлющий раньше — вложенный виден на своём участке.
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
        // У разобранного дерева так не бывает, у устаревшего — может.
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
        // A снаружи, B внутри, C начинается внутри B и длится дольше A.
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
