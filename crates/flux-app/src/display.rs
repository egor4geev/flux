//! Отображаемая строка: табы развёрнуты в пробелы, подсветка переведена в
//! `TextRun`. Чистые функции — без окна и контекста gpui.

use flux_core::Rope;
use flux_core::text::{line_end, line_start};
use flux_syntax::{Highlight, HighlightSpan};
use gpui::{FontStyle, FontWeight, TextRun};

use crate::theme::{self, SyntaxStyle};

/// Строка для отрисовки: без перевода строки, табы развёрнуты в пробелы.
/// Второе значение — байтовое смещение в отображаемой строке для каждой
/// колонки исходной строки и ещё одно, последнее, — длина отображаемой строки.
pub fn display_line(text: &Rope, line: usize) -> (String, Vec<usize>) {
    let slice = text.slice(line_start(text, line)..line_end(text, line));
    let mut display = String::with_capacity(slice.len_bytes());
    let mut char_to_byte = Vec::with_capacity(slice.len_chars() + 1);
    let mut column = 0;
    for c in slice.chars() {
        char_to_byte.push(display.len());
        if c == '\t' {
            let spaces = theme::TAB_WIDTH - column % theme::TAB_WIDTH;
            display.extend(std::iter::repeat_n(' ', spaces));
            column += spaces;
        } else {
            display.push(c);
            column += 1;
        }
    }
    char_to_byte.push(display.len());
    (display, char_to_byte)
}

/// `TextRun` отображаемой строки по спанам подсветки. Колонки спанов — символы
/// исходной строки; `char_to_byte` (из [`display_line`]) переводит их в байты
/// отображаемой, так что таб под спаном красится целиком. Промежутки — шрифтом и
/// цветом `base`; всё за концом строки отбрасывается. Длины прогонов в сумме
/// равны длине отображаемой строки, как того требует `shape_line`.
pub fn text_runs(
    spans: &[HighlightSpan],
    char_to_byte: &[usize],
    base: &TextRun,
    style: impl Fn(Highlight) -> Option<SyntaxStyle>,
) -> Vec<TextRun> {
    let columns = char_to_byte.len().saturating_sub(1);
    let line_len = char_to_byte.last().copied().unwrap_or(0);
    let mut runs: Vec<TextRun> = Vec::with_capacity(spans.len() * 2 + 1);
    let mut push = |len: usize, style: Option<SyntaxStyle>| {
        if len == 0 {
            return;
        }
        let mut run = base.clone();
        run.len = len;
        if let Some(style) = style {
            run.color = style.color;
            if style.bold {
                run.font.weight = FontWeight::BOLD;
            }
            if style.italic {
                run.font.style = FontStyle::Italic;
            }
        }
        // Соседние прогоны одного вида сливаются: меньше работы раскладке.
        match runs.last_mut() {
            Some(last) if last.color == run.color && last.font == run.font => last.len += len,
            _ => runs.push(run),
        }
    };
    let mut pos = 0;
    for span in spans {
        let start = char_to_byte[span.start.min(columns)];
        let end = char_to_byte[span.end.min(columns)];
        if start < pos || start >= end {
            continue;
        }
        push(start - pos, None);
        push(end - start, style(span.highlight));
        pos = end;
    }
    push(line_len - pos, None);
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Hsla, font, rgb};

    const KEYWORD: Highlight = Highlight(0);
    const STRING: Highlight = Highlight(1);
    const TITLE: Highlight = Highlight(2);

    fn color(hex: u32) -> Hsla {
        rgb(hex).into()
    }

    fn base() -> TextRun {
        TextRun {
            len: 0,
            font: font("Menlo"),
            color: color(0xffffff),
            background_color: None,
            underline: None,
            strikethrough: None,
        }
    }

    fn style(highlight: Highlight) -> Option<SyntaxStyle> {
        let plain = |hex| SyntaxStyle {
            color: color(hex),
            bold: false,
            italic: false,
        };
        match highlight {
            KEYWORD => Some(plain(0xff0000)),
            STRING => Some(plain(0x00ff00)),
            TITLE => Some(SyntaxStyle {
                bold: true,
                italic: true,
                ..plain(0x0000ff)
            }),
            _ => None,
        }
    }

    fn span(start: usize, end: usize, highlight: Highlight) -> HighlightSpan {
        HighlightSpan {
            start,
            end,
            highlight,
        }
    }

    /// Прогоны как (длина, цвет) — для сравнения.
    fn runs(source: &str, spans: &[HighlightSpan]) -> (String, Vec<(usize, Hsla)>) {
        let text = Rope::from_str(source);
        let (display, char_to_byte) = display_line(&text, 0);
        let runs = text_runs(spans, &char_to_byte, &base(), style);
        assert_eq!(runs.iter().map(|r| r.len).sum::<usize>(), display.len());
        let runs = runs.iter().map(|r| (r.len, r.color)).collect();
        (display, runs)
    }

    #[test]
    fn display_line_expands_tabs_to_tab_stops() {
        let text = Rope::from_str("a\tb\t\tc\r\nnext");
        let (display, char_to_byte) = display_line(&text, 0);
        assert_eq!(display, "a   b       c");
        assert_eq!(char_to_byte, vec![0, 1, 4, 5, 8, 12, 13]);
    }

    #[test]
    fn plain_line_is_one_base_run() {
        assert_eq!(
            runs("let x;", &[]),
            ("let x;".into(), vec![(6, color(0xffffff))])
        );
    }

    #[test]
    fn spans_become_runs_with_gaps_in_base_color() {
        let (_, got) = runs("fn main() {}", &[span(0, 2, KEYWORD), span(3, 7, STRING)]);
        assert_eq!(
            got,
            vec![
                (2, color(0xff0000)),
                (1, color(0xffffff)),
                (4, color(0x00ff00)),
                (5, color(0xffffff)),
            ]
        );
    }

    #[test]
    fn tab_under_a_span_is_painted_whole() {
        // «\t» в колонке 0 — четыре пробела; спан на табе и на «fn».
        let (display, got) = runs("\tfn x", &[span(0, 1, STRING), span(1, 3, KEYWORD)]);
        assert_eq!(display, "    fn x");
        assert_eq!(
            got,
            vec![
                (4, color(0x00ff00)),
                (2, color(0xff0000)),
                (2, color(0xffffff))
            ]
        );
    }

    #[test]
    fn columns_are_chars_and_runs_are_bytes() {
        // «ы» — 2 байта, «👍🏽» — 2 символа по 4 байта.
        let (_, got) = runs("ы = \"п👍🏽\";", &[span(4, 9, STRING)]);
        assert_eq!(
            got,
            vec![
                (5, color(0xffffff)),
                (12, color(0x00ff00)),
                (1, color(0xffffff))
            ]
        );
    }

    #[test]
    fn empty_line_has_no_runs() {
        let (display, got) = runs("", &[span(0, 3, KEYWORD)]);
        assert_eq!(display, "");
        assert_eq!(got, vec![]);
    }

    #[test]
    fn spans_past_the_end_are_clipped_or_dropped() {
        let (_, got) = runs("abc", &[span(1, 99, KEYWORD), span(50, 60, STRING)]);
        assert_eq!(got, vec![(1, color(0xffffff)), (2, color(0xff0000))]);
    }

    #[test]
    fn unknown_highlight_and_neighbours_merge_into_base() {
        let (_, got) = runs(
            "abcdef",
            &[
                span(1, 2, Highlight(7)),
                span(2, 3, KEYWORD),
                span(3, 4, KEYWORD),
            ],
        );
        assert_eq!(
            got,
            vec![
                (2, color(0xffffff)),
                (2, color(0xff0000)),
                (2, color(0xffffff))
            ]
        );
    }

    #[test]
    fn bold_and_italic_change_the_font() {
        let text = Rope::from_str("# Title");
        let (_, char_to_byte) = display_line(&text, 0);
        let runs = text_runs(&[span(2, 7, TITLE)], &char_to_byte, &base(), style);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].font.weight, FontWeight::NORMAL);
        assert_eq!(runs[1].font.weight, FontWeight::BOLD);
        assert_eq!(runs[1].font.style, FontStyle::Italic);
    }
}
