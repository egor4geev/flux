//! Замеры скорости — запускаются вручную:
//!
//! ```sh
//! cargo test -p flux-search --release --test bench -- --ignored --nocapture
//! cargo test -p flux-search --test bench -- --ignored --nocapture   # отладочная сборка
//! ```
//!
//! Проекты — `FLUX_SEARCH_BENCH_ROOTS` (через `:`), иначе сам репозиторий flux и каталог
//! над ним (`flux-dev`: репозиторий, вики, образцы). Файл для поиска в документе —
//! `FLUX_SEARCH_BENCH_FILE`, иначе `../playground/big.rs` (51 тыс. строк Rust).

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use flux_core::Rope;
use flux_search::{
    GrepOptions, PathMatcher, SearchQuery, find_all, replace_all, search_project, walk_files,
};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn roots() -> Vec<PathBuf> {
    match env::var_os("FLUX_SEARCH_BENCH_ROOTS") {
        Some(roots) => env::split_paths(&roots).collect(),
        None => {
            let repo = repo_root();
            let parent = repo.parent().unwrap().to_path_buf();
            vec![repo, parent]
        }
    }
}

fn bench_file() -> PathBuf {
    env::var_os("FLUX_SEARCH_BENCH_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("../playground/big.rs"))
}

/// Лучшее из нескольких прогонов: первый прогревает кеш файловой системы.
fn best<T>(runs: usize, mut f: impl FnMut() -> T) -> (Duration, T) {
    let mut best = Duration::MAX;
    let mut value = None;
    for _ in 0..runs {
        let start = Instant::now();
        let result = black_box(f());
        best = best.min(start.elapsed());
        value = Some(result);
    }
    (best, value.unwrap())
}

fn regex(text: &str) -> SearchQuery {
    SearchQuery {
        regex: true,
        ..SearchQuery::new(text)
    }
}

#[test]
#[ignore]
fn walk_and_grep() {
    for root in roots() {
        eprintln!("--- {}", root.display());
        let (time, summary) = best(5, || {
            walk_files(&root, &AtomicBool::new(false), |path| {
                black_box(path);
            })
        });
        eprintln!(
            "{:<44} {time:>10.3?}  files={}",
            "walk_files", summary.files
        );

        let queries = [
            ("literal «fn »", SearchQuery::new("fn ")),
            (
                "literal, case-sensitive «Selection»",
                SearchQuery {
                    case_sensitive: true,
                    ..SearchQuery::new("Selection")
                },
            ),
            (
                "whole word «text»",
                SearchQuery {
                    whole_word: true,
                    ..SearchQuery::new("text")
                },
            ),
            (r"regex «fn \w+\(»", regex(r"fn \w+\(")),
            // Склеено из частей, чтобы не найтись в этом же файле.
            (
                "literal, no hits",
                SearchQuery::new(["qqq", "zzz"].concat()),
            ),
        ];
        for (name, query) in queries {
            let (time, summary) = best(3, || {
                search_project(
                    &root,
                    &query,
                    &GrepOptions::default(),
                    &AtomicBool::new(false),
                    |file| {
                        black_box(file);
                    },
                )
                .unwrap()
            });
            eprintln!(
                "{name:<44} {time:>10.3?}  files={} matched={} matches={}{}",
                summary.files_searched,
                summary.files_matched,
                summary.matches,
                if summary.truncated {
                    " (truncated)"
                } else {
                    ""
                }
            );
        }
    }
}

#[test]
#[ignore]
fn find_in_document() {
    let path = bench_file();
    let text = Rope::from_str(&fs::read_to_string(&path).expect("bench file"));
    eprintln!(
        "--- {} ({} lines, {} KB)",
        path.display(),
        text.len_lines(),
        text.len_bytes() / 1024
    );
    let no_cancel = AtomicBool::new(false);
    let queries = [
        ("literal «let»", SearchQuery::new("let")),
        (
            "literal, rare «unreachable»",
            SearchQuery::new("unreachable"),
        ),
        (
            "whole word «self»",
            SearchQuery {
                whole_word: true,
                ..SearchQuery::new("self")
            },
        ),
        (r"regex «\bfn\s+\w+»", regex(r"\bfn\s+\w+")),
        (
            "literal, no hits",
            SearchQuery::new(["qqq", "zzz"].concat()),
        ),
    ];
    for (name, query) in &queries {
        let (time, matches) = best(5, || find_all(&text, query, &no_cancel).unwrap());
        eprintln!(
            "find_all {name:<38} {time:>10.3?}  matches={}{}",
            matches.ranges.len(),
            if matches.truncated {
                " (truncated)"
            } else {
                ""
            }
        );
    }
    let (time, edits) = best(3, || {
        replace_all(&text, &regex(r"let (\w+)"), "let ${1}_x", &no_cancel).unwrap()
    });
    eprintln!(
        "{:<47} {time:>10.3?}  edits={}",
        "replace_all regex «let (\\w+)»",
        edits.len()
    );
    let (time, _) = best(5, || {
        let mut string = String::with_capacity(text.len_bytes());
        for chunk in text.chunks() {
            string.push_str(chunk);
        }
        string
    });
    eprintln!("{:<47} {time:>10.3?}", "(rope → String alone)");
}

#[test]
#[ignore]
fn fuzzy_paths() {
    for root in roots() {
        eprintln!("--- {}", root.display());
        let notified = Arc::new(AtomicUsize::new(0));
        let counter = notified.clone();
        let mut matcher = PathMatcher::new(Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }));
        let injector = matcher.injector();
        let start = Instant::now();
        let summary = walk_files(&root, &AtomicBool::new(false), |path| injector.push(path));
        drop(injector);
        let walked = start.elapsed();
        settle(&mut matcher);
        eprintln!(
            "{:<44} {walked:>10.3?}, all matched after {:?}  files={}",
            "walk_files → PathMatcher",
            start.elapsed(),
            summary.files
        );
        // Набор запроса по букве: время до готового результата после каждой.
        let query = "srceditrs";
        for end in 1..=query.len() {
            let start = Instant::now();
            matcher.set_query(&query[..end]);
            settle(&mut matcher);
            let time = start.elapsed();
            let first = matcher
                .get(0)
                .map(|m| m.path.to_string())
                .unwrap_or_default();
            eprintln!(
                "  {:<42} {time:>10.3?}  matches={:<6} first={first}",
                format!("«{}»", &query[..end]),
                matcher.match_count()
            );
        }
        eprintln!("  notify calls: {}", notified.load(Ordering::Relaxed));
    }
}

fn settle(matcher: &mut PathMatcher) {
    loop {
        matcher.tick();
        if !matcher.is_running() {
            return;
        }
        thread::sleep(Duration::from_micros(200));
    }
}
