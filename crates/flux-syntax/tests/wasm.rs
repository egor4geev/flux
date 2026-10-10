//! WebAssembly grammars (stage 8.3): a plugin's grammar loads, parses and highlights the same as
//! the native one, off the UI thread only; a broken module says why. The fixtures are small grammars
//! built with tree-sitter CLI 0.27.1 (`tests/fixtures/README.md`).

mod common;

use std::sync::{Arc, mpsc};
use std::time::Duration;

use common::{dump, fresh_tree, language_by_name, parse_now};
use flux_core::{ChangeSet, Rope};
use flux_syntax::{
    GrammarSource, HighlightMap, Language, LanguageConfig, Precedence, Syntax, register,
};

const JSON_WASM: &[u8] = include_bytes!("fixtures/tree-sitter-json.wasm");
const TOML_WASM: &[u8] = include_bytes!("fixtures/tree-sitter-toml.wasm");

const SCOPES: &[&str] = &["string", "number", "constant", "punctuation", "property"];

/// One cache folder for every test of this binary: the engine is made once, by whichever test
/// loads a grammar first. `FLUX_WASM_BENCH_CACHE` names another (the benchmark's second run).
fn cache_dir() -> std::path::PathBuf {
    let dir = std::env::var_os("FLUX_WASM_BENCH_CACHE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("flux-syntax-wasm-cache-{}", std::process::id()))
        });
    flux_syntax::set_wasm_cache_dir(dir.clone());
    dir
}

/// The standard (native) language `name` as a WebAssembly one, registered under `owner`.
fn wasm_language(owner: &str, name: &str, bytes: &[u8]) -> Arc<Language> {
    cache_dir();
    let native = language_by_name(name).unwrap();
    let mut config = native.config().clone();
    config.name = format!("{owner}-{name}");
    config.aliases.clear();
    config.extensions = vec![format!("{owner}-{name}")];
    config.file_names.clear();
    config.grammar = GrammarSource::Wasm {
        name: name.into(),
        bytes: Arc::from(bytes),
    };
    register(owner, vec![config.clone()]);
    flux_syntax::language_by_name(&config.name).unwrap()
}

/// Parses on a thread of its own, as the application does for WebAssembly grammars.
fn parse_spawned(syntax: &mut Syntax, text: &Rope) {
    let job = syntax.parse_job(text).expect("a parse is needed");
    let Err(job) = job.run_with_budget(Duration::from_secs(10)) else {
        panic!("a WebAssembly grammar is never parsed on the calling (UI) thread");
    };
    let (sender, receiver) = mpsc::channel();
    job.spawn(move |result| sender.send(result).unwrap());
    let result = receiver.recv_timeout(Duration::from_secs(30)).unwrap();
    assert!(syntax.finish(result));
}

const JSON: &str = r#"{
  "name": "flux",
  "version": 3,
  "tags": ["ide", "rust", null, true],
  "nested": { "pi": 3.14, "empty": {} }
}
"#;

#[test]
fn a_wasm_grammar_parses_and_highlights_like_the_native_one() {
    let wasm = wasm_language("test.wasm-json", "json", JSON_WASM);
    assert!(wasm.is_wasm());
    assert!(wasm.loaded_grammar().is_none(), "loaded on first use");
    let text = Rope::from_str(JSON);
    let mut syntax = Syntax::new(wasm.clone());
    parse_spawned(&mut syntax, &text);
    assert!(wasm.loaded_grammar().is_some());
    assert!(wasm.grammar_error().is_none());
    let native = language_by_name("json").unwrap();
    let tree = syntax.tree().unwrap();
    assert!(!tree.root_node().has_error());
    assert_eq!(dump(tree), dump(&fresh_tree(&native, &text)));

    let map = HighlightMap::new(&wasm, SCOPES);
    let spans = syntax.highlight_lines(&text, 0..text.len_lines(), &map);
    let mut native_syntax = Syntax::new(native.clone());
    parse_now(&mut native_syntax, &text);
    let native_spans = native_syntax.highlight_lines(
        &text,
        0..text.len_lines(),
        &HighlightMap::new(&native, SCOPES),
    );
    assert_eq!(spans, native_spans);
    assert!(spans.iter().map(Vec::len).sum::<usize>() > 10);

    // An edit: the incremental parse on another thread matches a fresh one.
    let changes = ChangeSet::from_changes(text.len_chars(), [(16, 16, Some("-ide".into()))]);
    let mut edited = text.clone();
    syntax.edit(&text, &changes);
    changes.apply(&mut edited);
    parse_spawned(&mut syntax, &edited);
    assert_eq!(
        dump(syntax.tree().unwrap()),
        dump(&fresh_tree(&native, &edited))
    );
    flux_syntax::unregister("test.wasm-json");
}

#[test]
fn a_grammar_with_an_external_scanner_works() {
    let wasm = wasm_language("test.wasm-toml", "toml", TOML_WASM);
    let text = Rope::from_str(
        "[package]\nname = \"flux\"\ndescription = \"\"\"\nA multi-line\nstring\n\"\"\"\n\
         [[bin]]\npath = 'src/main.rs'\nnumbers = [1, 2.5, 0x1f]\n",
    );
    let mut syntax = Syntax::new(wasm);
    parse_spawned(&mut syntax, &text);
    let native = language_by_name("toml").unwrap();
    assert!(!syntax.tree().unwrap().root_node().has_error());
    assert_eq!(
        dump(syntax.tree().unwrap()),
        dump(&fresh_tree(&native, &text))
    );
    flux_syntax::unregister("test.wasm-toml");
}

#[test]
fn a_parser_moves_between_threads_with_its_store() {
    let wasm = wasm_language("test.wasm-threads", "json", JSON_WASM);
    let text = Rope::from_str(JSON);
    let mut syntax = Syntax::new(wasm);
    // The first job makes the parser and its store on one thread; the next ones take them along to
    // other threads.
    for _ in 0..4 {
        let job = syntax.parse_job(&text).unwrap();
        let result = std::thread::spawn(move || job.run()).join().unwrap();
        assert!(syntax.finish(result));
        assert!(!syntax.tree().unwrap().root_node().has_error());
        syntax.reset();
    }
    flux_syntax::unregister("test.wasm-threads");
}

#[test]
fn a_broken_module_says_why_and_is_not_parsed_again() {
    cache_dir();
    let config = LanguageConfig {
        name: "zz-broken-wasm".into(),
        display_name: "Broken".into(),
        extensions: vec!["zz-broken-wasm".into()],
        file_names: Vec::new(),
        aliases: Vec::new(),
        grammar: GrammarSource::Wasm {
            name: "json".into(),
            bytes: Arc::from(&b"\0asm not really"[..]),
        },
        highlights: "(string) @string".into(),
        precedence: Precedence::LastPattern,
    };
    register("test.wasm-broken", vec![config]);
    let language = flux_syntax::language_by_name("zz-broken-wasm").unwrap();
    let text = Rope::from_str("{}\n");
    let mut syntax = Syntax::new(language.clone());
    assert!(syntax.needs_parse(), "not tried yet");
    let job = syntax.parse_job(&text).unwrap();
    assert!(syntax.finish(job.run()));
    assert!(syntax.tree().is_none());
    assert!(language.grammar_failed());
    assert!(!language.grammar_error().unwrap().is_empty());
    assert!(!syntax.needs_parse());
    let changes = ChangeSet::from_changes(text.len_chars(), [(1, 1, Some(" ".into()))]);
    syntax.edit(&text, &changes);
    assert!(!syntax.needs_parse(), "a failed grammar is not parsed again");

    // The wrong name: the module has no `tree_sitter_yaml`.
    let mut wrong = flux_syntax::language_by_name("zz-broken-wasm")
        .unwrap()
        .config()
        .clone();
    wrong.grammar = GrammarSource::Wasm {
        name: "yaml".into(),
        bytes: Arc::from(JSON_WASM),
    };
    register("test.wasm-broken", vec![wrong]);
    let language = flux_syntax::language_by_name("zz-broken-wasm").unwrap();
    assert!(language.grammar().is_none());
    assert!(language.grammar_error().is_some());
    flux_syntax::unregister("test.wasm-broken");
}

#[test]
fn reregistering_keeps_a_loaded_grammar_until_the_config_changes() {
    let wasm = wasm_language("test.wasm-reload", "json", JSON_WASM);
    assert!(wasm.grammar().is_some());
    let config = wasm.config().clone();
    register("test.wasm-reload", vec![config.clone()]);
    let same = flux_syntax::language_by_name(&config.name).unwrap();
    assert!(Arc::ptr_eq(&same, &wasm));
    assert!(same.loaded_grammar().is_some());
    let mut changed = config.clone();
    changed.highlights.push_str("\n(number) @number");
    register("test.wasm-reload", vec![changed]);
    let reloaded = flux_syntax::language_by_name(&config.name).unwrap();
    assert!(!Arc::ptr_eq(&reloaded, &wasm));
    assert!(reloaded.loaded_grammar().is_none(), "a new language, a new load");
    assert!(reloaded.grammar().is_some());
    flux_syntax::unregister("test.wasm-reload");
}

#[test]
fn compiled_grammars_are_cached_on_disk() {
    let dir = cache_dir();
    let wasm = wasm_language("test.wasm-cache", "toml", TOML_WASM);
    assert!(wasm.grammar().is_some());
    let files = walk(&dir);
    assert!(files > 0, "{}: no cached modules", dir.display());
    flux_syntax::unregister("test.wasm-cache");
}

fn walk(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() { walk(&path) } else { 1 }
        })
        .sum()
}

/// Speed of a WebAssembly grammar against the native one, run by hand:
/// `FLUX_WASM_BENCH_DIR=<folder with tree-sitter-rust.wasm> cargo test -p flux-syntax --test wasm
/// -- --ignored --nocapture` (add `--release` for release numbers). `FLUX_WASM_BENCH_CACHE` — the
/// cache folder (a second run loads from it); `FLUX_SYNTAX_BENCH_FILE` — the Rust file to parse.
#[test]
#[ignore = "speed measurements, run manually with --ignored --nocapture"]
fn bench_wasm_against_native() {
    use std::time::Instant;
    let Some(dir) = std::env::var_os("FLUX_WASM_BENCH_DIR") else {
        eprintln!("FLUX_WASM_BENCH_DIR is not set");
        return;
    };
    let file = std::env::var("FLUX_SYNTAX_BENCH_FILE").unwrap_or_else(|_| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../flux-app/src/workspace.rs").into()
    });
    let text = Rope::from_str(&std::fs::read_to_string(&file).unwrap());
    let bytes = std::fs::read(std::path::Path::new(&dir).join("tree-sitter-rust.wasm")).unwrap();
    let wasm = wasm_language("bench.wasm", "rust", &bytes);
    let started = Instant::now();
    assert!(wasm.grammar().is_some(), "{:?}", wasm.grammar_error());
    eprintln!("load rust.wasm: {:.1} ms", started.elapsed().as_secs_f64() * 1e3);
    let native = language_by_name("rust").unwrap();
    for language in [native, wasm] {
        let mut syntax = Syntax::new(language.clone());
        let started = Instant::now();
        parse_now(&mut syntax, &text);
        let full = started.elapsed();
        let middle = text.len_chars() / 2;
        let mut times = Vec::new();
        let mut current = text.clone();
        for _ in 0..21 {
            let changes =
                ChangeSet::from_changes(current.len_chars(), [(middle, middle, Some("x".into()))]);
            syntax.edit(&current, &changes);
            changes.apply(&mut current);
            let started = Instant::now();
            parse_now(&mut syntax, &current);
            times.push(started.elapsed());
        }
        times.sort();
        eprintln!(
            "{} ({}): full {:.2} ms, incremental median {:.3} ms, {} lines",
            language.name(),
            if language.is_wasm() { "wasm" } else { "native" },
            full.as_secs_f64() * 1e3,
            times[times.len() / 2].as_secs_f64() * 1e3,
            text.len_lines()
        );
    }
}
