//! Injections (plugin API 0.4) with real grammars: Markdown code blocks of Rust and Python, nested
//! and combined documents, and incremental edits that must end where a fresh parse does.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Rng, language_by_name};
use flux_core::{ChangeSet, Rope};
use flux_syntax::{
    GrammarSource, HighlightMap, Injection, Language, LanguageConfig, Precedence, Syntax,
};

const SCOPES: &[&str] = &["keyword", "string", "text.literal", "text.title", "number"];
const KEYWORD: usize = 0;
const LITERAL: usize = 2;

/// Code blocks: the fence's language names the embedded one.
const FENCES: &str = r#"
(fenced_code_block
  (info_string (language) @injection.language)
  (code_fence_content) @injection.content)
"#;

/// Every `pyall` block is one Python document.
const COMBINED: &str = r#"
((fenced_code_block
  (info_string (language) @_lang)
  (code_fence_content) @injection.content)
  (#eq? @_lang "pyall")
  (#set! injection.language "python")
  (#set! injection.combined))
"#;

/// A Markdown that embeds by `injections`, registered under its own name.
fn markdown(name: &str, injections: &str) -> Arc<Language> {
    let base = language_by_name("markdown").unwrap();
    flux_syntax::register(
        &format!("test.{name}"),
        vec![LanguageConfig {
            name: name.into(),
            display_name: name.into(),
            extensions: vec![name.into()],
            file_names: Vec::new(),
            file_patterns: Vec::new(),
            aliases: Vec::new(),
            grammar: GrammarSource::Builtin("markdown".into()),
            highlights: base.config().highlights.clone(),
            injections: injections.into(),
            precedence: Precedence::LastPattern,
        }],
    );
    flux_syntax::language_by_name(name).unwrap()
}

fn parse(syntax: &mut Syntax, text: &Rope) {
    while let Some(job) = syntax.parse_job(text) {
        assert!(syntax.finish(job.run()));
    }
}

fn maps(syntax: &Syntax) -> Vec<HighlightMap> {
    syntax
        .languages()
        .iter()
        .map(|language| HighlightMap::new(language, SCOPES))
        .collect()
}

/// The scopes of `line`, as (text, scope) pairs.
fn line_scopes(syntax: &Syntax, text: &Rope, line: usize) -> Vec<(String, usize)> {
    let spans = syntax.highlight_lines(text, line..line + 1, &maps(syntax));
    let chars: Vec<char> = text.line(line).chars().collect();
    spans[0]
        .iter()
        .map(|span| {
            let word: String = chars[span.start..span.end].iter().collect();
            (word, span.highlight.0)
        })
        .collect()
}

/// One byte range, as a list.
fn one(start: usize, end: usize) -> Vec<std::ops::Range<usize>> {
    std::iter::once(start..end).collect()
}

fn summary(injections: &[Injection]) -> Vec<(String, usize, Vec<std::ops::Range<usize>>)> {
    injections
        .iter()
        .map(|i| (i.language.name().to_string(), i.depth, i.ranges.clone()))
        .collect()
}

#[test]
fn a_code_block_gets_its_languages_highlighting() {
    let language = markdown("zz-md-fences", FENCES);
    let text = Rope::from_str("# Title\n\n```rust\nfn main() { let x = 1; }\n```\n\nplain\n");
    let mut syntax = Syntax::new(language);
    parse(&mut syntax, &text);
    let injections = syntax.injections();
    assert_eq!(injections.len(), 1);
    assert_eq!(injections[0].language.name(), "rust");
    assert_eq!(injections[0].depth, 1);
    assert_eq!(injections[0].ranges, one(17, 42));
    let scopes = line_scopes(&syntax, &text, 3);
    assert!(scopes.contains(&("fn".into(), KEYWORD)), "{scopes:?}");
    assert!(scopes.contains(&("let".into(), KEYWORD)), "{scopes:?}");
    // The host's color of the whole block stays under what Rust doesn't color.
    assert!(
        scopes.contains(&(" main() { ".into(), LITERAL)),
        "{scopes:?}"
    );
    assert!(
        line_scopes(&syntax, &text, 2)
            .iter()
            .any(|(word, scope)| word.starts_with("```") && *scope == LITERAL)
    );
}

#[test]
fn an_unknown_language_stays_the_hosts_text() {
    let language = markdown("zz-md-unknown", FENCES);
    let text = Rope::from_str("```cobol-zz\nMOVE A TO B\n```\n");
    let mut syntax = Syntax::new(language);
    parse(&mut syntax, &text);
    assert!(syntax.injections().is_empty());
    let scopes = line_scopes(&syntax, &text, 1);
    assert_eq!(scopes, vec![("MOVE A TO B".into(), LITERAL)]);
}

#[test]
fn injections_nest_and_combine() {
    let language = markdown("zz-md-nested", &format!("{FENCES}\n{COMBINED}"));
    let text = Rope::from_str(
        "````zz-md-nested\n```rust\nlet a = 1;\n```\n````\n\n```pyall\nx = 1\n```\n\n```pyall\ny = 2\n```\n",
    );
    let mut syntax = Syntax::new(language);
    parse(&mut syntax, &text);
    let found = summary(&syntax.injections());
    assert!(
        found.contains(&("zz-md-nested".into(), 1, one(17, 40))),
        "{found:?}"
    );
    assert!(
        found.contains(&("rust".into(), 2, one(25, 36))),
        "{found:?}"
    );
    let python: Vec<_> = found.iter().filter(|(name, ..)| name == "python").collect();
    assert_eq!(python.len(), 1, "one document of both blocks: {found:?}");
    assert_eq!(python[0].2, vec![55..61, 75..81]);
    let scopes = line_scopes(&syntax, &text, 2);
    assert!(scopes.contains(&("let".into(), KEYWORD)), "{scopes:?}");
}

#[test]
fn the_fences_language_can_change() {
    let language = markdown("zz-md-change", FENCES);
    let mut text = Rope::from_str("```rust\nlet x = 1;\n```\n");
    let mut syntax = Syntax::new(language);
    parse(&mut syntax, &text);
    assert_eq!(syntax.injections()[0].language.name(), "rust");
    // `rust` → `py`: outside the block's text, but in its match.
    let edit = |syntax: &mut Syntax, text: &mut Rope, from: usize, to: usize, insert: &str| {
        let changes = ChangeSet::from_changes(text.len_chars(), [(from, to, Some(insert.into()))]);
        syntax.edit(text, &changes);
        changes.apply(text);
    };
    edit(&mut syntax, &mut text, 3, 7, "py");
    parse(&mut syntax, &text);
    assert_eq!(syntax.injections()[0].language.name(), "python");
    edit(&mut syntax, &mut text, 3, 5, "");
    parse(&mut syntax, &text);
    assert!(syntax.injections().is_empty(), "no language, no injection");
}

#[test]
fn a_parse_within_a_budget_goes_on_in_the_background() {
    let language = markdown("zz-md-budget", FENCES);
    let text = Rope::from_str(&"```rust\nfn f() {}\n```\n\n".repeat(200));
    let mut syntax = Syntax::new(language);
    parse(&mut syntax, &text);
    assert_eq!(syntax.injections().len(), 200);
    // An edit in one block: within a generous budget, only it is parsed again.
    let mut text2 = text.clone();
    let changes = ChangeSet::from_changes(text.len_chars(), [(11, 11, Some("g".into()))]);
    syntax.edit(&text, &changes);
    changes.apply(&mut text2);
    let job = syntax.parse_job(&text2).unwrap();
    let result = job
        .run_with_budget(Duration::from_secs(5))
        .unwrap_or_else(|job| job.run());
    assert!(syntax.finish(result));
    let mut fresh = Syntax::new(syntax.language().clone());
    parse(&mut fresh, &text2);
    assert_eq!(summary(&syntax.injections()), summary(&fresh.injections()));
    // With no budget at all it comes back, and the background run finishes the same.
    let changes = ChangeSet::from_changes(text2.len_chars(), [(11, 12, None)]);
    syntax.edit(&text2, &changes);
    let job = syntax.parse_job(&text).unwrap();
    let job = match job.run_with_budget(Duration::ZERO) {
        Ok(result) => {
            assert!(syntax.finish(result));
            None
        }
        Err(job) => Some(job),
    };
    if let Some(job) = job {
        assert!(syntax.finish(job.run()));
    }
    let mut fresh = Syntax::new(syntax.language().clone());
    parse(&mut fresh, &text);
    assert_eq!(summary(&syntax.injections()), summary(&fresh.injections()));
}

/// Random edits, parsed now and then: the layers and the highlighting always end where a fresh
/// parse of the same text does.
#[test]
fn incremental_injections_match_a_fresh_parse() {
    let language = markdown("zz-md-random", &format!("{FENCES}\n{COMBINED}"));
    let snippets = [
        "x",
        "\n",
        "```",
        "```rust\n",
        "```py\n",
        "```pyall\n",
        "fn ",
        "let y = 2;",
        "> ",
        "~~~",
        "\n```\n",
        "# ",
        "ы",
        "`",
    ];
    let mut rng = Rng::new(7);
    let mut text = Rope::from_str(
        "# Doc\n\n```rust\nfn a() {}\n```\n\ntext\n\n```pyall\nx = 1\n```\n\n> ```py\n> y = 2\n> ```\n",
    );
    let mut syntax = Syntax::new(language.clone());
    parse(&mut syntax, &text);
    for step in 0..300 {
        let changes = common::random_changes(&mut rng, &text, &snippets);
        syntax.edit(&text, &changes);
        changes.apply(&mut text);
        if step % 3 != 0 {
            continue;
        }
        if let Some(job) = syntax.parse_job(&text) {
            let result = job
                .run_with_budget(Duration::from_micros(200))
                .unwrap_or_else(|job| job.run());
            assert!(syntax.finish(result));
        }
        let mut fresh = Syntax::new(language.clone());
        parse(&mut fresh, &text);
        assert_eq!(
            summary(&syntax.injections()),
            summary(&fresh.injections()),
            "step {step}: {text:?}"
        );
        let lines = 0..text.len_lines();
        assert_eq!(
            syntax.highlight_lines(&text, lines.clone(), &maps(&syntax)),
            fresh.highlight_lines(&text, lines, &maps(&fresh)),
            "step {step}: {text:?}"
        );
    }
}

#[test]
fn a_language_installed_later_is_picked_up() {
    let language = markdown("zz-md-later", FENCES);
    let text = Rope::from_str("```zz-later-lang\nfn x() {}\n```\n");
    let mut syntax = Syntax::new(language);
    parse(&mut syntax, &text);
    assert!(syntax.injections().is_empty());
    let mut rust = language_by_name("rust").unwrap().config().clone();
    rust.name = "zz-later-lang".into();
    rust.aliases = Vec::new();
    rust.extensions = Vec::new();
    flux_syntax::register("test.zz-later-lang", vec![rust]);
    syntax.languages_changed();
    parse(&mut syntax, &text);
    assert_eq!(syntax.injections()[0].language.name(), "zz-later-lang");
    flux_syntax::unregister("test.zz-later-lang");
    syntax.languages_changed();
    parse(&mut syntax, &text);
    assert!(syntax.injections().is_empty());
}

/// Typing in a large Markdown file with three hundred Rust code blocks, with the injections and
/// without them (as before API 0.4); run it manually:
///
/// ```sh
/// cargo test -p flux-syntax --release --test injection -- --ignored --nocapture
/// ```
#[test]
#[ignore = "speed measurements, run manually with --ignored --nocapture"]
fn bench_typing_in_markdown_with_code_blocks() {
    use std::time::Instant;

    let block = "Some text about the code below, with a [link](https://example.com).\n\n\
                 ```rust\nfn main() {\n    let x = vec![1, 2, 3];\n    println!(\"{x:?}\");\n}\n```\n\n";
    let source = block.repeat(300);
    let plain = language_by_name("markdown").unwrap();
    let injecting = markdown("zz-md-bench", FENCES);
    for language in [plain, injecting] {
        let mut text = Rope::from_str(&source);
        let mut syntax = Syntax::new(language.clone());
        let start = Instant::now();
        parse(&mut syntax, &text);
        let initial = start.elapsed();
        let mut rng = Rng::new(3);
        let mut attempts = Vec::new();
        let mut totals = Vec::new();
        let mut within = 0;
        let mut at = 0;
        for step in 0..200 {
            let changes = if step % 2 == 0 {
                at = rng.below(text.len_chars());
                ChangeSet::from_changes(text.len_chars(), [(at, at, Some("x".into()))])
            } else {
                ChangeSet::from_changes(text.len_chars(), [(at, at + 1, None)])
            };
            syntax.edit(&text, &changes);
            changes.apply(&mut text);
            let job = syntax.parse_job(&text).unwrap();
            let start = Instant::now();
            let attempt = job.run_with_budget(Duration::from_millis(1));
            attempts.push(start.elapsed());
            let result = match attempt {
                Ok(result) => {
                    within += 1;
                    result
                }
                Err(job) => job.run(),
            };
            totals.push(start.elapsed());
            assert!(syntax.finish(result));
        }
        attempts.sort();
        totals.sort();
        let maps = maps(&syntax);
        let mut windows = Vec::new();
        for first in (0..text.len_lines() - 60).step_by(97) {
            let start = Instant::now();
            syntax.highlight_lines(&text, first..first + 60, &maps);
            windows.push(start.elapsed());
        }
        windows.sort();
        eprintln!(
            "{:<12} {} lines, {} layers: first parse {initial:.2?}; run_with_budget(1 ms) median \
             {:.3?} max {:.3?}, {within}/200 within; whole reparse median {:.3?} p90 {:.3?} max \
             {:.3?}; highlight 60 lines median {:.3?} max {:.3?}",
            language.name(),
            text.len_lines(),
            syntax.injections().len(),
            attempts[100],
            attempts[199],
            totals[100],
            totals[180],
            totals[199],
            windows[windows.len() / 2],
            windows[windows.len() - 1],
        );
    }
}
