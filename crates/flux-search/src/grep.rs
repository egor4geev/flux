//! Поиск по проекту: параллельный обход ([`crate::files`]) и построчный поиск
//! `grep-searcher` в каждом файле. Результаты уходят потоком, по файлу.

use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use grep_matcher::{LineTerminator, Matcher};
use grep_regex::RegexMatcher;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::WalkState;

use crate::files::{file_entry, relative, walker};
use crate::query::{QueryError, SearchQuery};

/// Ограничения поиска по проекту.
#[derive(Debug, Clone)]
pub struct GrepOptions {
    /// Всего вхождений: дальше поиск останавливается (`truncated`).
    pub max_matches: usize,
    /// Длинная строка (минифицированный код) показывается окном такой ширины в символах
    /// вокруг первого вхождения. `0` — без ограничения.
    pub max_line_chars: usize,
    /// Файлы крупнее пропускаются.
    pub max_file_bytes: u64,
}

impl Default for GrepOptions {
    fn default() -> Self {
        Self {
            max_matches: 10_000,
            max_line_chars: 300,
            max_file_bytes: 10 * 1024 * 1024,
        }
    }
}

/// Вхождения в одном файле.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMatches {
    /// Путь относительно корня проекта.
    pub path: PathBuf,
    /// Строки с вхождениями по возрастанию номера.
    pub lines: Vec<LineMatch>,
}

/// Строка с вхождениями.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineMatch {
    /// Номер строки с нуля (строки разделяются `\n`).
    pub line: usize,
    /// Текст строки без перевода строки. Длинная строка — окно
    /// [`GrepOptions::max_line_chars`] символов вокруг первого вхождения.
    pub text: String,
    /// Колонка (в символах) начала `text` в настоящей строке: больше нуля, если начало
    /// строки обрезано окном.
    pub column_offset: usize,
    /// Вхождения — колонки в символах внутри `text`, по возрастанию. Вышедшие за окно
    /// обрезаны по его краю или отброшены.
    pub ranges: Vec<Range<usize>>,
}

/// Итог поиска.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GrepSummary {
    /// Сколько файлов просмотрено (включая бинарные).
    pub files_searched: usize,
    /// В скольких файлах нашлись вхождения.
    pub files_matched: usize,
    /// Сколько вхождений отдано (не больше [`GrepOptions::max_matches`]).
    pub matches: usize,
    /// Остановились на [`GrepOptions::max_matches`].
    pub truncated: bool,
    /// Остановились по флагу отмены.
    pub cancelled: bool,
}

/// Ищет запрос в файлах проекта — по тем же правилам обхода, что
/// [`walk_files`](crate::files::walk_files). Бинарные файлы (с байтом NUL) и файлы крупнее
/// [`GrepOptions::max_file_bytes`] пропускаются. Блокирует до конца поиска — вызывать в
/// фоне. `sink` вызывается из рабочих потоков, по файлу, в произвольном порядке.
/// Ошибка в запросе возвращается сразу, до обхода; пустой запрос ничего не ищет.
pub fn search_project(
    root: &Path,
    query: &SearchQuery,
    options: &GrepOptions,
    cancel: &AtomicBool,
    sink: impl Fn(FileMatches) + Sync,
) -> Result<GrepSummary, QueryError> {
    if query.is_empty() {
        return Ok(GrepSummary::default());
    }
    let matcher = query.line_matcher()?;
    let counters = Counters::default();
    walker(root, Some(options.max_file_bytes))
        .build_parallel()
        .run(|| {
            let mut searcher = SearcherBuilder::new()
                .line_terminator(LineTerminator::crlf())
                .binary_detection(BinaryDetection::quit(b'\0'))
                .line_number(true)
                .build();
            let (matcher, counters, sink) = (&matcher, &counters, &sink);
            Box::new(move |entry| {
                if cancel.load(Ordering::Relaxed) {
                    counters.cancelled.store(true, Ordering::Relaxed);
                    return WalkState::Quit;
                }
                if counters.truncated.load(Ordering::Relaxed) {
                    return WalkState::Quit;
                }
                let Some(entry) = file_entry(entry) else {
                    return WalkState::Continue;
                };
                let lines = search_file(&mut searcher, matcher, entry.path(), options, cancel);
                counters.files_searched.fetch_add(1, Ordering::Relaxed);
                let Some(lines) = counters.take(lines, options.max_matches) else {
                    return WalkState::Continue;
                };
                counters.files_matched.fetch_add(1, Ordering::Relaxed);
                sink(FileMatches {
                    path: relative(root, entry.path()).to_path_buf(),
                    lines,
                });
                if counters.truncated.load(Ordering::Relaxed) {
                    WalkState::Quit
                } else {
                    WalkState::Continue
                }
            })
        });
    Ok(counters.summary(options.max_matches))
}

#[derive(Default)]
struct Counters {
    files_searched: AtomicUsize,
    files_matched: AtomicUsize,
    matches: AtomicUsize,
    truncated: AtomicBool,
    cancelled: AtomicBool,
}

impl Counters {
    /// Учитывает вхождения файла в общем лимите. Файл, не влезший целиком, обрезается;
    /// `None` — отдавать нечего.
    fn take(&self, lines: Vec<FoundLine>, max_matches: usize) -> Option<Vec<LineMatch>> {
        let count: usize = lines.iter().map(|line| line.count).sum();
        if count == 0 {
            return None;
        }
        let before = self.matches.fetch_add(count, Ordering::Relaxed);
        let mut left = max_matches.saturating_sub(before);
        if left < count {
            self.truncated.store(true, Ordering::Relaxed);
        }
        let mut taken = Vec::new();
        for FoundLine { mut line, count } in lines {
            if left == 0 {
                break;
            }
            if count > left {
                line.ranges.truncate(left);
            }
            left = left.saturating_sub(count);
            taken.push(line);
        }
        (!taken.is_empty()).then_some(taken)
    }

    fn summary(self, max_matches: usize) -> GrepSummary {
        GrepSummary {
            files_searched: self.files_searched.into_inner(),
            files_matched: self.files_matched.into_inner(),
            matches: self.matches.into_inner().min(max_matches),
            truncated: self.truncated.into_inner(),
            cancelled: self.cancelled.into_inner(),
        }
    }
}

/// Строка с вхождениями и их настоящее число (часть могла не попасть в окно).
struct FoundLine {
    line: LineMatch,
    count: usize,
}

/// Вхождения в одном файле; бинарный или нечитаемый файл — пусто.
fn search_file(
    searcher: &mut Searcher,
    matcher: &RegexMatcher,
    path: &Path,
    options: &GrepOptions,
    cancel: &AtomicBool,
) -> Vec<FoundLine> {
    let mut collector = Collector {
        matcher,
        max_line_chars: options.max_line_chars,
        cancel,
        lines: Vec::new(),
        binary: false,
    };
    let searched = searcher.search_path(matcher, path, &mut collector);
    if searched.is_err() || collector.binary {
        return Vec::new();
    }
    collector.lines
}

struct Collector<'a> {
    matcher: &'a RegexMatcher,
    max_line_chars: usize,
    cancel: &'a AtomicBool,
    lines: Vec<FoundLine>,
    /// Нашёлся байт NUL: файл бинарный, найденное в нём выбрасывается.
    binary: bool,
}

impl Sink for Collector<'_> {
    type Error = io::Error;

    fn matched(&mut self, _: &Searcher, found: &SinkMatch<'_>) -> Result<bool, io::Error> {
        if self.cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let bytes = trim_line_terminator(found.bytes());
        let mut matches = Vec::new();
        let _ = self.matcher.find_iter(bytes, |m| {
            if !m.is_empty() {
                matches.push(m.start()..m.end());
            }
            true
        });
        // Строка «совпала» только пустым вхождением (`^`, `a*`) — пропускаем.
        if matches.is_empty() {
            return Ok(true);
        }
        let line = found.line_number().map_or(0, |n| n.saturating_sub(1)) as usize;
        let count = matches.len();
        self.lines.push(FoundLine {
            line: line_match(line, bytes, &matches, self.max_line_chars),
            count,
        });
        Ok(true)
    }

    fn binary_data(&mut self, _: &Searcher, _: u64) -> Result<bool, io::Error> {
        self.binary = true;
        Ok(false)
    }
}

fn trim_line_terminator(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}

/// Строка для показа: текст (невалидный UTF-8 — через замену на U+FFFD), колонки
/// вхождений в символах, окно для длинной строки.
fn line_match(
    line: usize,
    bytes: &[u8],
    matches: &[Range<usize>],
    max_line_chars: usize,
) -> LineMatch {
    let text = String::from_utf8_lossy(bytes);
    // Колонки считаются по тексту после замены: смещения байтов — по исходным байтам,
    // поэтому для каждого берём длину в символах префикса до него.
    let column = |byte: usize| String::from_utf8_lossy(&bytes[..byte]).chars().count();
    let ranges: Vec<Range<usize>> = match std::str::from_utf8(bytes) {
        Ok(valid) => {
            let mut chars = crate::buffer::CharCounter::new(valid);
            matches.iter().map(|m| chars.range(m.clone())).collect()
        }
        Err(_) => matches
            .iter()
            .map(|m| column(m.start)..column(m.end))
            .collect(),
    };
    let len = text.chars().count();
    if max_line_chars == 0 || len <= max_line_chars {
        return LineMatch {
            line,
            text: text.into_owned(),
            column_offset: 0,
            ranges,
        };
    }
    // Окно: первое вхождение — на пятой части ширины от левого края, но не дальше конца.
    let first = ranges[0].start;
    let start = first
        .saturating_sub(max_line_chars / 5)
        .min(len - max_line_chars);
    let end = start + max_line_chars;
    let window: String = text.chars().skip(start).take(max_line_chars).collect();
    let ranges = ranges
        .iter()
        .filter(|range| range.end > start && range.start < end)
        .map(|range| range.start.max(start) - start..range.end.min(end) - start)
        .collect();
    LineMatch {
        line,
        text: window,
        column_offset: start,
        ranges,
    }
}

#[cfg(test)]
// Диапазоны в ожиданиях — именно списки диапазонов, а не `(a..b).collect()`.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;
    use crate::files::tests::tree;
    use std::fs;
    use std::sync::Mutex;

    fn search(
        root: &Path,
        query: &SearchQuery,
        options: &GrepOptions,
    ) -> (Vec<FileMatches>, GrepSummary) {
        let found = Mutex::new(Vec::new());
        let summary = search_project(root, query, options, &AtomicBool::new(false), |file| {
            found.lock().unwrap().push(file)
        })
        .unwrap();
        let mut found = found.into_inner().unwrap();
        found.sort_by(|a, b| a.path.cmp(&b.path));
        (found, summary)
    }

    fn line(line: usize, text: &str, ranges: &[Range<usize>]) -> LineMatch {
        LineMatch {
            line,
            text: text.into(),
            column_offset: 0,
            ranges: ranges.to_vec(),
        }
    }

    #[test]
    fn finds_lines_with_char_columns() {
        let dir = tree(&[
            (".git/HEAD", ""),
            (".gitignore", "ignored/\n"),
            ("a.rs", "fn main() {\n    let привет = \"мир\"; // мир\n}\n"),
            ("b.txt", "no match here\r\nМИР 👍🏽 мир\r\n"),
            ("ignored/c.txt", "мир"),
        ]);
        let (files, summary) = search(
            dir.path(),
            &SearchQuery::new("мир"),
            &GrepOptions::default(),
        );
        assert_eq!(
            files,
            [
                FileMatches {
                    path: "a.rs".into(),
                    lines: vec![line(
                        1,
                        "    let привет = \"мир\"; // мир",
                        &[18..21, 27..30]
                    )],
                },
                FileMatches {
                    path: "b.txt".into(),
                    lines: vec![line(1, "МИР 👍🏽 мир", &[0..3, 7..10])],
                },
            ]
        );
        assert_eq!(summary.files_matched, 2);
        assert_eq!(summary.matches, 4);
        assert_eq!(summary.files_searched, 3, "a.rs, b.txt, .gitignore");
        assert!(!summary.truncated && !summary.cancelled);
    }

    #[test]
    fn regex_word_and_anchors_work_per_line() {
        let dir = tree(&[("x.txt", "foo food\r\nbar foo\r\n")]);
        let word = SearchQuery {
            whole_word: true,
            ..SearchQuery::new("foo")
        };
        let (files, _) = search(dir.path(), &word, &GrepOptions::default());
        let lines: Vec<_> = files[0]
            .lines
            .iter()
            .map(|l| (l.line, l.ranges.clone()))
            .collect();
        assert_eq!(lines, [(0, vec![0..3]), (1, vec![4..7])]);

        let at_end = SearchQuery {
            regex: true,
            ..SearchQuery::new("foo$")
        };
        let (files, _) = search(dir.path(), &at_end, &GrepOptions::default());
        assert_eq!(files[0].lines, [line(1, "bar foo", &[4..7])]);
    }

    #[test]
    fn bad_regex_fails_before_walking() {
        let dir = tree(&[("x.txt", "x")]);
        let query = SearchQuery {
            regex: true,
            ..SearchQuery::new("(x")
        };
        let result = search_project(
            dir.path(),
            &query,
            &GrepOptions::default(),
            &AtomicBool::new(false),
            |_| panic!("no results expected"),
        );
        assert_eq!(
            result.unwrap_err().message,
            "regex parse error: unclosed group"
        );
    }

    #[test]
    fn binary_and_large_files_are_skipped() {
        let dir = tree(&[("text.txt", "needle\n"), ("small.txt", "needle")]);
        let mut binary = b"needle\n".to_vec();
        binary.extend(std::iter::repeat_n(0u8, 10));
        binary.extend(b"needle\n");
        fs::write(dir.path().join("bin.dat"), binary).unwrap();
        fs::write(dir.path().join("big.txt"), "needle\n".repeat(1000)).unwrap();
        let options = GrepOptions {
            max_file_bytes: 1000,
            ..GrepOptions::default()
        };
        let (files, _) = search(dir.path(), &SearchQuery::new("needle"), &options);
        let paths: Vec<_> = files.iter().map(|f| f.path.to_str().unwrap()).collect();
        assert_eq!(paths, ["small.txt", "text.txt"]);
    }

    #[test]
    fn long_lines_are_windowed_around_the_first_match() {
        let long = format!(
            "{}needle{}needle{}",
            "ы".repeat(1000),
            "x".repeat(50),
            "z".repeat(500)
        );
        let dir = tree(&[("min.js", &long)]);
        let options = GrepOptions {
            max_line_chars: 100,
            ..GrepOptions::default()
        };
        let (files, summary) = search(dir.path(), &SearchQuery::new("needle"), &options);
        let found = &files[0].lines[0];
        assert_eq!(found.column_offset, 1000 - 20);
        assert_eq!(found.text.chars().count(), 100);
        assert_eq!(found.ranges, [20..26, 76..82]);
        assert_eq!(
            &found.text[found.text.char_indices().nth(20).unwrap().0..][..6],
            "needle"
        );
        assert_eq!(summary.matches, 2);

        // Вхождение у самого конца строки: окно прижимается к концу.
        let tail = format!("{}needle", "x".repeat(500));
        let dir = tree(&[("tail.js", &tail)]);
        let (files, _) = search(dir.path(), &SearchQuery::new("needle"), &options);
        let found = &files[0].lines[0];
        assert_eq!(found.column_offset, 506 - 100);
        assert_eq!(found.ranges, [94..100]);
    }

    #[test]
    fn match_limit_truncates_and_stops() {
        let files: Vec<(String, String)> = (0..20)
            .map(|i| (format!("f{i}.txt"), "hit hit hit\n".repeat(5)))
            .collect();
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, c)| (p.as_str(), c.as_str()))
            .collect();
        let dir = tree(&files);
        let options = GrepOptions {
            max_matches: 40,
            ..GrepOptions::default()
        };
        let (found, summary) = search(dir.path(), &SearchQuery::new("hit"), &options);
        let given: usize = found
            .iter()
            .flat_map(|f| &f.lines)
            .map(|l| l.ranges.len())
            .sum();
        assert_eq!(given, 40);
        assert_eq!(summary.matches, 40);
        assert!(summary.truncated);
        assert!(summary.files_matched < 20);
    }

    #[test]
    fn cancelled_search_reports_nothing() {
        let dir = tree(&[("x.txt", "needle")]);
        let summary = search_project(
            dir.path(),
            &SearchQuery::new("needle"),
            &GrepOptions::default(),
            &AtomicBool::new(true),
            |_| panic!("cancelled search must not report"),
        )
        .unwrap();
        assert!(summary.cancelled);
        assert_eq!(summary.matches, 0);
    }

    #[test]
    fn empty_matches_and_empty_query_find_nothing() {
        let dir = tree(&[("x.txt", "abc\n")]);
        let (files, summary) = search(dir.path(), &SearchQuery::new(""), &GrepOptions::default());
        assert!(files.is_empty());
        assert_eq!(summary, GrepSummary::default());
        let empty = SearchQuery {
            regex: true,
            ..SearchQuery::new("z*")
        };
        let (files, _) = search(dir.path(), &empty, &GrepOptions::default());
        assert!(files.is_empty());
    }

    #[test]
    fn invalid_utf8_is_shown_lossy() {
        let dir = tree(&[]);
        fs::write(dir.path().join("latin1.txt"), b"caf\xe9 needle\n").unwrap();
        let (files, _) = search(
            dir.path(),
            &SearchQuery::new("needle"),
            &GrepOptions::default(),
        );
        assert_eq!(files[0].lines, [line(0, "caf\u{fffd} needle", &[5..11])]);
    }
}
