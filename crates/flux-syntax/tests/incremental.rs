//! Инкрементальный разбор: после правок дерево совпадает со свежим разбором
//! нового текста, а точки всех узлов — с байтами по правилам tree-sitter.

mod common;

use std::time::Duration;

use common::samples::{sample, snippets};
use common::{NaiveTree, Rng, check_points, dump, fresh_tree, parse_now, random_changes};
use flux_core::{ChangeSet, Rope};
use flux_syntax::{Syntax, language_by_name};

/// Итог случайных правок для одного языка.
#[derive(Debug, Default)]
struct Stats {
    /// Разборов после правок.
    parses: usize,
    /// Из них свежий разбор без ошибок: деревья обязаны совпасть целиком.
    clean: usize,
    /// Из них с ошибками, где восстановление после ошибки разошлось со свежим.
    error_recovery_diffs: usize,
}

impl Stats {
    fn add(&mut self, other: &Stats) {
        self.parses += other.parses;
        self.clean += other.clean;
        self.error_recovery_diffs += other.error_recovery_diffs;
    }
}

/// Документ под правками сразу в двух видах: через [`Syntax`] и через
/// наивный эталон [`NaiveTree`].
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

    /// Разбор и проверки:
    /// - дерево совпадает с эталоном с наивными координатами — всегда;
    /// - точки всех узлов точны — всегда;
    /// - дерево совпадает со свежим разбором, если в тексте нет ошибок. С
    ///   ошибками tree-sitter вправе восстановиться иначе, чем при разборе
    ///   с нуля: его собственные тесты такие деревья сравнивают только на
    ///   согласованность, поэтому здесь расхождения только считаются.
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

/// Раунды случайных правок: несколько правок с разбором после каждой (иногда
/// пачкой без разбора), затем отмена всех правок по одной через
/// [`ChangeSet::invert`] — в конце раунда текст снова исходный и без ошибок.
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

/// Сплошь «чужие» переводы строк: `\r`, U+2028, FF — медленный путь точек.
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

/// Правки, пришедшие во время разбора, доигрываются на его результате.
/// Эталон повторяет ту же последовательность разборов с наивными координатами,
/// так что деревья обязаны совпасть всегда, даже с синтаксическими ошибками.
/// Вторая работа идёт, пока все правки отменяются: в конце текст снова
/// исходный, без ошибок, и дерево сравнивается со свежим разбором.
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
        // Нечётные сиды — с готовым деревом (правки считаются сразу), чётные —
        // первый разбор (правки откладываются до числа строк из результата).
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

        // Вторая работа; пока она идёт, все правки отменяются.
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

    // Брошенный результат — то же самое.
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

    // После reset результат старой работы устарел.
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

/// Большой текст: тысячи функций.
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

    // Пока работа отложена, документ правят дальше.
    let changes =
        ChangeSet::from_changes(text.len_chars(), [(0, 0, Some("fn новая() {}\n".into()))]);
    syntax.edit(&text, &changes);
    changes.apply(&mut text);

    // Довести до конца маленькими порциями: каждая продолжает предыдущую.
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
    // Перевод строки между функциями: текст остаётся без ошибок.
    let at = text.line_to_char(text.len_lines() / 2 / 6 * 6);
    let changes = ChangeSet::from_changes(text.len_chars(), [(at, at, Some("\n".into()))]);
    syntax.edit(&text, &changes);
    changes.apply(&mut text);
    let job = syntax.parse_job(&text).unwrap();
    // В отладочной сборке C-код грамматик не оптимизирован: бюджет щедрый.
    let result = job
        .run_with_budget(Duration::from_millis(500))
        .unwrap_or_else(|_| panic!("incremental reparse of one char should be fast"));
    assert!(syntax.finish(result));
    assert_eq!(
        dump(syntax.tree().unwrap()),
        dump(&fresh_tree(language, &text))
    );
}
