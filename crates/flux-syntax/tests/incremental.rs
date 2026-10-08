//! Incremental parsing: after edits, the tree matches a fresh parse of the new text, and the points
//! of all nodes match the bytes according to tree-sitter's rules.

mod common;

use std::time::Duration;

use common::samples::{sample, snippets};
use common::{NaiveTree, Rng, check_points, dump, fresh_tree, parse_now, random_changes};
use flux_core::{ChangeSet, Rope};
use flux_syntax::{Syntax, language_by_name};

/// The outcome of random edits for one language.
#[derive(Debug, Default)]
struct Stats {
    /// Number of parses after edits.
    parses: usize,
    /// Of these, those where the fresh parse has no errors: the trees must match completely.
    clean: usize,
    /// Of these, those with errors where error recovery diverged from the fresh parse.
    error_recovery_diffs: usize,
}

impl Stats {
    fn add(&mut self, other: &Stats) {
        self.parses += other.parses;
        self.clean += other.clean;
        self.error_recovery_diffs += other.error_recovery_diffs;
    }
}

/// The document under edits in two forms at once: through [`Syntax`] and through the naive
/// reference [`NaiveTree`].
struct Session {
    language_name: &'static str,
    seed: u64,
    text: Rope,
    syntax: Syntax,
    naive: NaiveTree,
    stats: Stats,
}

impl Session {
    fn new(language_name: &'static str, seed: u64) -> Self {
        let language = language_by_name(language_name).unwrap();
        let source = sample(language_name);
        let text = Rope::from_str(&source);
        let mut syntax = Syntax::new(language);
        parse_now(&mut syntax, &text);
        let naive = NaiveTree::new(language, &source);
        Self {
            language_name,
            seed,
            text,
            syntax,
            naive,
            stats: Stats::default(),
        }
    }

    fn edit(&mut self, changes: &ChangeSet) {
        self.syntax.edit(&self.text, changes);
        self.naive.edit(&self.text.to_string(), changes);
        changes.apply(&mut self.text);
    }

    /// Parsing and checks:
    /// - the tree matches the reference with naive coordinates: always;
    /// - the points of all nodes are exact: always;
    /// - the tree matches a fresh parse if the text has no errors. With errors, tree-sitter is
    ///   entitled to recover differently than when parsing from scratch: its own tests compare such
    ///   trees only for consistency, so here the divergences are only counted.
    fn parse_and_check(&mut self, what: &str) {
        parse_now(&mut self.syntax, &self.text);
        let string = self.text.to_string();
        self.naive.reparse(&string);
        let context = format!(
            "{} seed {} {what}\ntext: {string:?}",
            self.language_name, self.seed
        );
        let tree = self.syntax.tree().unwrap();
        assert_eq!(dump(tree), dump(&self.naive.tree), "{context}");
        if let Err(message) = check_points(tree, &string) {
            panic!("{context}\n{message}");
        }
        let fresh = fresh_tree(self.syntax.language(), &self.text);
        self.stats.parses += 1;
        if fresh.root_node().has_error() {
            if dump(tree) != dump(&fresh) {
                self.stats.error_recovery_diffs += 1;
            }
        } else {
            self.stats.clean += 1;
            assert_eq!(
                tree.root_node().to_sexp(),
                fresh.root_node().to_sexp(),
                "{context}"
            );
            assert_eq!(dump(tree), dump(&fresh), "{context}");
        }
    }
}

/// Rounds of random edits: several edits with a parse after each one (sometimes in a batch without
/// a parse), then all the edits are undone one by one via [`ChangeSet::invert`]: at the end of the
/// round the text is the original again and has no errors.
fn random_edits(language_name: &'static str, seed: u64, rounds: usize) -> Stats {
    let mut session = Session::new(language_name, seed);
    let mut rng = Rng::new(seed);
    let original = session.text.clone();
    for round in 0..rounds {
        let mut undo = Vec::new();
        for step in 0..1 + rng.below(6) {
            let batch = if rng.chance(30) { 2 + rng.below(3) } else { 1 };
            for _ in 0..batch {
                let changes = random_changes(&mut rng, &session.text, snippets(language_name));
                undo.push(changes.invert(&session.text));
                session.edit(&changes);
            }
            session.parse_and_check(&format!("round {round} step {step}"));
        }
        while let Some(changes) = undo.pop() {
            session.edit(&changes);
            session.parse_and_check(&format!("round {round} undo {}", undo.len()));
        }
        assert_eq!(session.text, original);
    }
    session.stats
}

#[test]
fn samples_parse_without_errors() {
    for language in flux_syntax::languages() {
        let tree = fresh_tree(language, &Rope::from_str(&sample(language.name())));
        assert!(
            !tree.root_node().has_error(),
            "{}: {}",
            language.name(),
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn rust_incremental_matches_fresh_parse() {
    let mut total = Stats::default();
    for seed in 1..=4 {
        total.add(&random_edits("rust", seed, 60));
    }
    eprintln!("rust: {total:?}");
    assert!(total.clean >= 240, "{total:?}");
}

#[test]
fn every_language_incremental_matches_fresh_parse() {
    let mut total = Stats::default();
    for language in flux_syntax::languages() {
        let stats = random_edits(language.name(), 7, 30);
        eprintln!("{}: {stats:?}", language.name());
        total.add(&stats);
    }
    eprintln!("all: {total:?}");
}

/// Nothing but "foreign" line breaks: `\r`, U+2028, FF; the slow path for points.
#[test]
fn foreign_line_breaks_keep_points_exact() {
    let language = language_by_name("rust").unwrap();
    let mut rng = Rng::new(42);
    let mut text = Rope::from_str("fn a() {}\rfn b() {}\u{2028}fn c() {}\u{c}\n// x\u{85}y\r\n");
    let mut syntax = Syntax::new(language);
    parse_now(&mut syntax, &text);
    let pool = [
        "\r",
        "\u{2028}",
        "\u{c}",
        "\u{b}",
        "\u{85}",
        "\u{2029}",
        "\r\n",
        "\n",
        "fn z() {}",
    ];
    for step in 0..300 {
        let changes = random_changes(&mut rng, &text, &pool);
        syntax.edit(&text, &changes);
        changes.apply(&mut text);
        parse_now(&mut syntax, &text);
        let string = text.to_string();
        if let Err(message) = check_points(syntax.tree().unwrap(), &string) {
            panic!("step {step}: {message}\ntext: {string:?}");
        }
    }
}

/// Edits that arrive during a parse are replayed on top of its result. The reference repeats the
/// same sequence of parses with naive coordinates, so the trees must always match, even with syntax
/// errors. The second job runs while all the edits are being undone: at the end the text is the
/// original again, without errors, and the tree is compared with a fresh parse.
#[test]
fn edits_during_parse_are_replayed() {
    let language = language_by_name("rust").unwrap();
    for seed in 0..40 {
        let mut rng = Rng::new(seed);
        let source = sample("rust");
        let original = Rope::from_str(&source);
        let mut text = original.clone();
        let mut syntax = Syntax::new(language);
        let mut naive = NaiveTree::new(language, &source);
        let mut undo = Vec::new();
        let edit =
            |syntax: &mut Syntax, naive: &mut NaiveTree, text: &mut Rope, changes: &ChangeSet| {
                syntax.edit(text, changes);
                naive.edit(&text.to_string(), changes);
                changes.apply(text);
            };
        // Odd seeds start with a ready tree (edits are processed immediately); even seeds start
        // with the first parse (edits are deferred until the line count from the result).
        if seed % 2 == 1 {
            parse_now(&mut syntax, &text);
            let changes = random_changes(&mut rng, &text, snippets("rust"));
            undo.push(changes.invert(&text));
            edit(&mut syntax, &mut naive, &mut text, &changes);
        }
        let job = syntax.parse_job(&text).unwrap();
        if seed % 2 == 1 {
            naive.reparse(&text.to_string());
        }
        for _ in 0..1 + rng.below(4) {
            let changes = random_changes(&mut rng, &text, snippets("rust"));
            undo.push(changes.invert(&text));
            edit(&mut syntax, &mut naive, &mut text, &changes);
            assert!(
                syntax.parse_job(&text).is_none(),
                "previous job still running"
            );
        }
        assert!(syntax.finish(job.run()));
        assert!(
            syntax.needs_parse(),
            "edits after the snapshot need another parse"
        );
        check_points(syntax.tree().unwrap(), &text.to_string()).unwrap();

        // The second job; while it runs, all the edits are undone.
        let job = syntax.parse_job(&text).unwrap();
        naive.reparse(&text.to_string());
        while let Some(changes) = undo.pop() {
            edit(&mut syntax, &mut naive, &mut text, &changes);
        }
        assert_eq!(text, original);
        assert!(syntax.finish(job.run()));
        parse_now(&mut syntax, &text);
        naive.reparse(&source);
        assert!(!syntax.needs_parse());
        let tree = syntax.tree().unwrap();
        assert_eq!(dump(tree), dump(&naive.tree), "seed {seed}");
        assert_eq!(
            dump(tree),
            dump(&fresh_tree(language, &text)),
            "seed {seed}"
        );
    }
}

#[test]
fn edits_without_any_tree_or_job_need_nothing() {
    let language = language_by_name("go").unwrap();
    let mut text = Rope::from_str(&sample("go"));
    let mut syntax = Syntax::new(language);
    let mut rng = Rng::new(5);
    for _ in 0..10 {
        let changes = random_changes(&mut rng, &text, snippets("go"));
        syntax.edit(&text, &changes);
        changes.apply(&mut text);
    }
    assert!(syntax.tree().is_none());
    parse_now(&mut syntax, &text);
    assert_eq!(
        dump(syntax.tree().unwrap()),
        dump(&fresh_tree(language, &text))
    );
}

#[test]
fn abandoned_job_is_replaced() {
    let mut text = Rope::from_str(&sample("rust"));
    let mut syntax = Syntax::new(language_by_name("rust").unwrap());
    parse_now(&mut syntax, &text);
    let changes = ChangeSet::from_changes(text.len_chars(), [(0, 0, Some("fn x() {}\n".into()))]);
    syntax.edit(&text, &changes);
    changes.apply(&mut text);

    let job = syntax.parse_job(&text).unwrap();
    assert!(syntax.is_parsing());
    drop(job);
    assert!(!syntax.is_parsing());
    assert!(
        syntax.needs_parse(),
        "the dropped job never parsed its snapshot"
    );
    parse_now(&mut syntax, &text);
    assert_eq!(
        dump(syntax.tree().unwrap()),
        dump(&fresh_tree(syntax.language(), &text))
    );

    // A dropped result: the same thing.
    let changes = ChangeSet::from_changes(text.len_chars(), [(0, 2, None)]);
    syntax.edit(&text, &changes);
    changes.apply(&mut text);
    let result = syntax.parse_job(&text).unwrap().run();
    drop(result);
    assert!(syntax.needs_parse());
    parse_now(&mut syntax, &text);
    assert!(!syntax.needs_parse());
}

#[test]
fn foreign_and_stale_results_are_rejected() {
    let rust = language_by_name("rust").unwrap();
    let text = Rope::from_str("fn main() {}\n");
    let mut a = Syntax::new(rust);
    let mut b = Syntax::new(rust);
    let job_a = a.parse_job(&text).unwrap();
    let job_b = b.parse_job(&text).unwrap();
    assert!(!a.finish(job_b.run()), "result of another Syntax");
    assert!(a.tree().is_none());

    // After a reset, the result of the old job is stale.
    a.reset();
    assert!(!a.finish(job_a.run()));
    assert!(a.tree().is_none());
    parse_now(&mut a, &text);
    assert!(a.tree().is_some());
}

#[test]
fn mismatched_edit_resets_instead_of_panicking() {
    let mut text = Rope::from_str("fn main() {}\n");
    let mut syntax = Syntax::new(language_by_name("rust").unwrap());
    parse_now(&mut syntax, &text);
    let wrong = ChangeSet::from_changes(1000, [(500, 900, None)]);
    syntax.edit(&text, &wrong);
    assert!(syntax.tree().is_none());
    assert!(syntax.needs_parse());
    let changes = ChangeSet::from_changes(text.len_chars(), [(3, 7, Some("старт".into()))]);
    syntax.edit(&text, &changes);
    changes.apply(&mut text);
    parse_now(&mut syntax, &text);
    assert_eq!(
        dump(syntax.tree().unwrap()),
        dump(&fresh_tree(syntax.language(), &text))
    );
}

/// A large text: thousands of functions.
fn big_rust() -> Rope {
    let mut source = String::new();
    for i in 0..3000 {
        source.push_str(&format!(
            "/// Функция {i} 🦀\nfn f{i}(x: u32) -> u32 {{\n    let s = \"строка {i}\";\n    x + s.len() as u32 * {i}\n}}\n\n"
        ));
    }
    Rope::from_str(&source)
}

#[test]
fn tiny_budget_on_big_text_yields_job_that_can_finish() {
    let language = language_by_name("rust").unwrap();
    let mut text = big_rust();
    let mut syntax = Syntax::new(language);
    let job = syntax.parse_job(&text).unwrap();
    let job = match job.run_with_budget(Duration::ZERO) {
        Ok(_) => panic!("a big text cannot be parsed in zero time"),
        Err(job) => job,
    };
    assert!(syntax.is_parsing());

    // While the job is deferred, the document keeps being edited.
    let changes =
        ChangeSet::from_changes(text.len_chars(), [(0, 0, Some("fn новая() {}\n".into()))]);
    syntax.edit(&text, &changes);
    changes.apply(&mut text);

    // Run it to completion in small slices: each one continues the previous one.
    let mut job = job;
    let mut slices = 0;
    let result = loop {
        match job.run_with_budget(Duration::from_millis(2)) {
            Ok(result) => break result,
            Err(rest) => {
                slices += 1;
                job = rest;
            }
        }
    };
    assert!(slices > 0);
    assert!(syntax.finish(result));
    assert!(syntax.needs_parse());
    parse_now(&mut syntax, &text);
    assert_eq!(
        dump(syntax.tree().unwrap()),
        dump(&fresh_tree(language, &text))
    );
}

#[test]
fn small_edit_reparses_within_budget() {
    let language = language_by_name("rust").unwrap();
    let mut text = big_rust();
    let mut syntax = Syntax::new(language);
    parse_now(&mut syntax, &text);
    // A newline between functions: the text stays error-free.
    let at = text.line_to_char(text.len_lines() / 2 / 6 * 6);
    let changes = ChangeSet::from_changes(text.len_chars(), [(at, at, Some("\n".into()))]);
    syntax.edit(&text, &changes);
    changes.apply(&mut text);
    let job = syntax.parse_job(&text).unwrap();
    // In a debug build the C code of the grammars is not optimized, so the budget is generous.
    let result = job
        .run_with_budget(Duration::from_millis(500))
        .unwrap_or_else(|_| panic!("incremental reparse of one char should be fast"));
    assert!(syntax.finish(result));
    assert_eq!(
        dump(syntax.tree().unwrap()),
        dump(&fresh_tree(language, &text))
    );
}
