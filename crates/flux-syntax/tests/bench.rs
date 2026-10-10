//! Speed benchmarks; run them manually:
//!
//! ```sh
//! cargo test -p flux-syntax --release --test bench -- --ignored --nocapture
//! cargo test -p flux-syntax --test bench -- --ignored --nocapture   # debug build
//! ```
//!
//! The file comes from `FLUX_SYNTAX_BENCH_FILE`, otherwise `gpui-0.2.2/src/window.rs` from the
//! cargo registry (≈190 KB, 5k lines of Rust).

mod common;

use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use std::{env, fs};

use common::{Rng, language_for_path, languages, parse_now};
use flux_core::{ChangeSet, Rope};
use flux_syntax::tree_sitter::{Parser, QueryCursor};
use flux_syntax::{HighlightMap, Syntax};

const THEME: &[&str] = &[
    "attribute",
    "comment",
    "constant",
    "constructor",
    "function",
    "function.method",
    "function.macro",
    "keyword",
    "label",
    "operator",
    "property",
    "punctuation",
    "string",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

fn bench_file() -> PathBuf {
    if let Some(path) = env::var_os("FLUX_SYNTAX_BENCH_FILE") {
        return path.into();
    }
    let registry = PathBuf::from(env::var_os("HOME").expect("HOME")).join(".cargo/registry/src");
    fs::read_dir(&registry)
        .expect("cargo registry")
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("gpui-0.2.2/src/window.rs"))
        .find(|path| path.exists())
        .expect("gpui-0.2.2/src/window.rs not found; set FLUX_SYNTAX_BENCH_FILE")
}

struct Samples(Vec<Duration>);

impl Samples {
    fn new() -> Self {
        Self(Vec::new())
    }

    fn time<T>(&mut self, f: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let value = black_box(f());
        self.0.push(start.elapsed());
        value
    }

    fn report(&mut self, what: &str) {
        self.0.sort();
        let at = |q: f64| self.0[((self.0.len() - 1) as f64 * q).round() as usize];
        eprintln!(
            "{what:<46} median {:>10.3?}  p90 {:>10.3?}  max {:>10.3?}  (n={})",
            at(0.5),
            at(0.9),
            at(1.0),
            self.0.len()
        );
    }
}

#[test]
#[ignore = "speed measurements, run manually with --ignored --nocapture"]
fn bench() {
    let path = bench_file();
    let source = fs::read_to_string(&path).unwrap();
    let mut text = Rope::from_str(&source);
    let language = language_for_path(&path).expect("known language");
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    eprintln!(
        "\n[{profile}] {} — {} lines, {} KB, language {}",
        path.display(),
        text.len_lines(),
        text.len_bytes() / 1024,
        language.name()
    );

    eprintln!("highlight query compilation (once per language per process):");
    for language in languages() {
        let start = Instant::now();
        let query = language.query().unwrap();
        eprintln!(
            "  {:<12} {:>10.3?}  ({} patterns, {} captures)",
            language.name(),
            start.elapsed(),
            query.pattern_count(),
            query.capture_names().len()
        );
    }
    let map = HighlightMap::new(&language, THEME);

    let mut setup = Samples::new();
    for _ in 0..20 {
        setup.time(|| {
            let mut parser = Parser::new();
            parser.set_language(language.grammar().unwrap()).unwrap();
            parser
        });
    }
    setup.report("Parser::new + set_language");

    let rounds = if cfg!(debug_assertions) { 5 } else { 15 };
    let mut full = Samples::new();
    let mut syntax = Syntax::new(language.clone());
    for _ in 0..rounds {
        syntax = Syntax::new(language.clone());
        let job = syntax.parse_job(&text).unwrap();
        let result = full.time(|| job.run());
        assert!(syntax.finish(result));
    }
    full.report("initial parse of the whole file");

    // A single character: we insert "x" at a random place and delete it in the next step, so after
    // each pair the text is back to the original.
    let original = text.clone();
    let mut rng = Rng::new(1);
    let mut edits = Samples::new();
    let mut reparse = Samples::new();
    let mut within_budget = 0;
    let mut budget_attempt = Samples::new();
    let steps = 200;
    let mut at = 0;
    for step in 0..steps {
        let changes = if step % 2 == 0 {
            at = rng.below(text.len_chars());
            ChangeSet::from_changes(text.len_chars(), [(at, at, Some("x".into()))])
        } else {
            ChangeSet::from_changes(text.len_chars(), [(at, at + 1, None)])
        };
        edits.time(|| syntax.edit(&text, &changes));
        changes.apply(&mut text);
        let job = syntax.parse_job(&text).unwrap();
        if step % 2 == 0 {
            let result = reparse.time(|| job.run());
            assert!(syntax.finish(result));
        } else {
            // As in the app after a key press: synchronously, with a 1 ms budget.
            let attempt = budget_attempt.time(|| job.run_with_budget(Duration::from_millis(1)));
            let result = match attempt {
                Ok(result) => {
                    within_budget += 1;
                    result
                }
                Err(job) => job.run(),
            };
            assert!(syntax.finish(result));
        }
    }
    edits.report("Syntax::edit (one character, Tree::edit only)");
    reparse.report("reparse after a one-character edit");
    budget_attempt.report("run_with_budget(1 ms) after an edit");
    eprintln!(
        "{:<46} {within_budget}/{}",
        "  of them fit in the budget",
        steps / 2
    );

    let lines = text.len_lines();
    let mut highlight = Samples::new();
    let mut spans = 0;
    for first in (0..lines.saturating_sub(60)).step_by(lines / 40 + 1) {
        for _ in 0..10 {
            let result = highlight.time(|| syntax.highlight_lines(&text, first..first + 60, &map));
            spans = result.iter().map(Vec::len).sum::<usize>();
        }
    }
    highlight.report("highlight_lines, 60-line window");
    eprintln!("{:<46} {spans}", "  spans in the last window");

    let mut cursor = Samples::new();
    for _ in 0..100 {
        cursor.time(QueryCursor::new);
    }
    cursor.report("QueryCursor::new");

    // A check that the benchmark measured the real thing: the tree is correct after all the edits.
    assert_eq!(text, original);
    let mut fresh = Syntax::new(language.clone());
    parse_now(&mut fresh, &text);
    assert_eq!(
        syntax.tree().unwrap().root_node().to_sexp(),
        fresh.tree().unwrap().root_node().to_sexp()
    );
}
