//! Line highlighting: scopes, columns in characters, span invariants, robustness against a stale
//! tree.

mod common;

use common::samples::{sample, snippets};
use common::{Rng, language_by_name, languages, parse_now, random_changes};
use flux_core::text::line_len;
use flux_core::{ChangeSet, Rope};
use flux_syntax::{Highlight, HighlightMap, HighlightSpan, Syntax};

/// The theme for the tests is the union of the capture names of all queries, with fallbacks.
const SCOPES: &[&str] = &[
    "attribute",
    "boolean",
    "comment",
    "comment.documentation",
    "constant",
    "constant.builtin",
    "constructor",
    "embedded",
    "escape",
    "function",
    "function.builtin",
    "function.macro",
    "function.method",
    "keyword",
    "label",
    "number",
    "operator",
    "property",
    "punctuation.bracket",
    "punctuation.delimiter",
    "punctuation.special",
    "string",
    "string.escape",
    "string.special",
    "string.special.key",
    "tag",
    "text.literal",
    "text.reference",
    "text.title",
    "text.uri",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

struct Doc {
    text: Rope,
    syntax: Syntax,
    map: HighlightMap,
    scopes: &'static [&'static str],
}

impl Doc {
    fn new(language: &str, source: &str, scopes: &'static [&'static str]) -> Self {
        let language = language_by_name(language).unwrap();
        let text = Rope::from_str(source);
        let mut syntax = Syntax::new(language.clone());
        parse_now(&mut syntax, &text);
        Self {
            text,
            syntax,
            map: HighlightMap::new(&language, scopes),
            scopes,
        }
    }

    fn lines(&self) -> Vec<Vec<HighlightSpan>> {
        self.syntax
            .highlight_lines(&self.text, 0..self.text.len_lines(), &self.map)
    }

    /// A line's spans as (start, end, scope).
    fn line(&self, line: usize) -> Vec<(usize, usize, &'static str)> {
        let spans = self
            .syntax
            .highlight_lines(&self.text, line..line + 1, &self.map);
        spans[0]
            .iter()
            .map(|span| (span.start, span.end, self.scopes[span.highlight.0]))
            .collect()
    }

    /// The scope of the first character of the first occurrence of `needle`.
    fn scope_of(&self, needle: &str) -> Option<&'static str> {
        let byte = self
            .text
            .to_string()
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} not found"));
        let char = self.text.byte_to_char(byte);
        let line = self.text.char_to_line(char);
        let column = char - self.text.line_to_char(line);
        self.line(line)
            .into_iter()
            .find(|(start, end, _)| (*start..*end).contains(&column))
            .map(|(_, _, scope)| scope)
    }
}

/// Spans are sorted, non-overlapping, non-empty, and do not extend beyond the line content.
fn check_invariants(text: &Rope, first_line: usize, lines: &[Vec<HighlightSpan>]) {
    for (i, spans) in lines.iter().enumerate() {
        let line = first_line + i;
        let len = line_len(text, line);
        let mut prev_end = 0;
        for span in spans {
            assert!(span.start < span.end, "line {line}: empty span {span:?}");
            assert!(
                span.start >= prev_end,
                "line {line}: overlap or disorder at {span:?}"
            );
            assert!(span.end <= len, "line {line} ({len} chars): {span:?}");
            prev_end = span.end;
        }
    }
}

#[test]
fn rust_keywords_strings_comments() {
    const THEME: &[&str] = &["keyword", "string", "comment", "function"];
    let doc = Doc::new(
        "rust",
        "fn main() {\n    let s = \"строка 👍🏽\"; // комментарий\n}\n",
        THEME,
    );
    assert_eq!(doc.line(0), vec![(0, 2, "keyword"), (3, 7, "function")]);
    // Columns are characters: Cyrillic and an emoji made of two code points.
    assert_eq!(
        doc.line(1),
        vec![(4, 7, "keyword"), (12, 23, "string"), (25, 39, "comment")]
    );
    assert_eq!(doc.line(2), vec![]);
    assert_eq!(doc.lines().len(), 4);
}

#[test]
fn spans_stop_before_line_ending() {
    let doc = Doc::new("rust", "let a = 1; // ы\r\n/* x\r\ny */\r\n", SCOPES);
    assert_eq!(
        doc.line(0).last(),
        Some(&(11, 15, "comment")),
        "comment without \\r"
    );
    assert_eq!(doc.line(1), vec![(0, 4, "comment")]);
    assert_eq!(doc.line(2), vec![(0, 4, "comment")]);
    check_invariants(&doc.text, 0, &doc.lines());
}

#[test]
fn every_sample_is_highlighted_consistently() {
    for language in languages() {
        let doc = Doc::new(language.name(), &sample(language.name()), SCOPES);
        let lines = doc.lines();
        assert_eq!(lines.len(), doc.text.len_lines());
        check_invariants(&doc.text, 0, &lines);
        let spans: usize = lines.iter().map(Vec::len).sum();
        assert!(spans > 10, "{}: only {spans} spans", language.name());

        // A window in the middle matches the same section of the full highlighting.
        let middle = 3..lines.len() - 2;
        let window = doc
            .syntax
            .highlight_lines(&doc.text, middle.clone(), &doc.map);
        assert_eq!(window, lines[middle], "{}", language.name());
    }
}

#[test]
fn empty_document_and_out_of_range_lines() {
    let rust = language_by_name("rust").unwrap();
    let map = HighlightMap::new(&rust, SCOPES);
    let empty = Rope::new();
    let mut syntax = Syntax::new(rust.clone());
    assert_eq!(
        syntax.highlight_lines(&empty, 0..10, &map),
        vec![Vec::new()]
    );
    parse_now(&mut syntax, &empty);
    assert!(syntax.tree().is_some());
    assert_eq!(
        syntax.highlight_lines(&empty, 0..10, &map),
        vec![Vec::new()]
    );
    assert_eq!(
        syntax.highlight_lines(&empty, 5..10, &map),
        Vec::<Vec<_>>::new()
    );

    let doc = Doc::new("rust", "fn a() {}\nfn b() {}\n", SCOPES);
    assert_eq!(
        doc.syntax.highlight_lines(&doc.text, 5..10, &doc.map).len(),
        0
    );
    assert_eq!(
        doc.syntax
            .highlight_lines(&doc.text, 1..100, &doc.map)
            .len(),
        2
    );
    let reversed = std::ops::Range { start: 2, end: 1 };
    assert_eq!(
        doc.syntax
            .highlight_lines(&doc.text, reversed, &doc.map)
            .len(),
        0
    );
    assert_eq!(
        doc.syntax.highlight_lines(&doc.text, 0..0, &doc.map).len(),
        0
    );
}

#[test]
fn no_highlight_before_first_parse() {
    let rust = language_by_name("rust").unwrap();
    let text = Rope::from_str("fn main() {}\n");
    let syntax = Syntax::new(rust.clone());
    let lines = syntax.highlight_lines(&text, 0..2, &HighlightMap::new(&rust, SCOPES));
    assert_eq!(lines, vec![Vec::new(), Vec::new()]);
}

/// The tree is edited but not parsed: highlighting does not panic and upholds the invariants,
/// however far the tree and the text diverge.
#[test]
fn stale_tree_never_panics() {
    for language in languages() {
        let name = language.name();
        let mut doc = Doc::new(name, &sample(name), SCOPES);
        let mut rng = Rng::new(11);
        for step in 0..60 {
            let changes = if step % 20 == 19 {
                // Delete almost everything: the tree nodes are far past the end of the text.
                ChangeSet::from_changes(doc.text.len_chars(), [(1, doc.text.len_chars(), None)])
            } else {
                random_changes(&mut rng, &doc.text, snippets(name))
            };
            doc.syntax.edit(&doc.text, &changes);
            changes.apply(&mut doc.text);
            check_invariants(&doc.text, 0, &doc.lines());
            if step % 7 == 0 {
                parse_now(&mut doc.syntax, &doc.text);
            }
        }
    }
}

/// A stale tree with nodes whose offsets fall inside multi-byte characters of the new text.
#[test]
fn stale_offsets_inside_multibyte_chars() {
    let mut doc = Doc::new("rust", "let s = \"абвгд\"; // эюя\n", SCOPES);
    // We swap out the text without editing the tree: the offsets now cut through characters.
    doc.text = Rope::from_str("ы👍🏽ы👍🏽ы👍🏽ы👍🏽ы👍🏽ы👍🏽\n");
    check_invariants(&doc.text, 0, &doc.lines());
    doc.text = Rope::from_str("ы");
    check_invariants(&doc.text, 0, &doc.lines());
}

#[test]
fn highlight_map_falls_back_by_dots() {
    const THEME: &[&str] = &["function", "keyword"];
    let doc = Doc::new("rust", "fn f() { x.go(); m!(); }\n", THEME);
    assert_eq!(
        doc.scope_of("go"),
        Some("function"),
        "function.method → function"
    );
    assert_eq!(
        doc.scope_of("m!"),
        Some("function"),
        "function.macro → function"
    );
    assert_eq!(doc.scope_of("fn"), Some("keyword"));
    assert_eq!(doc.scope_of("("), None, "punctuation is not in the theme");
}

/// For one and the same node the later pattern wins; this is how tree-sitter-highlight ≥ 0.21
/// works, and the JavaScript query is written for it: `(identifier) @variable` comes first in it.
#[test]
fn javascript_later_pattern_wins() {
    let doc = Doc::new(
        "javascript",
        "function foo(a) {}\nvar b = function() {};\nlet c = 1;\nx.bar();\nnew Foo();\nrequire('m');\n",
        SCOPES,
    );
    assert_eq!(doc.scope_of("foo"), Some("function"));
    assert_eq!(doc.scope_of("a)"), Some("variable.parameter"));
    assert_eq!(doc.scope_of("b ="), Some("function"));
    assert_eq!(doc.scope_of("c ="), Some("variable"));
    assert_eq!(doc.scope_of("bar"), Some("function.method"));
    assert_eq!(doc.scope_of("Foo"), Some("constructor"));
    assert_eq!(doc.scope_of("require"), Some("function.builtin"));
    assert_eq!(doc.scope_of("var"), Some("keyword"));
}

/// A capture with no scope in the theme takes no part in overlap resolution and does not shadow
/// other captures of the same node.
#[test]
fn unmapped_capture_does_not_hide_others() {
    const THEME: &[&str] = &["variable"];
    let doc = Doc::new("javascript", "function foo() {}\n", THEME);
    assert_eq!(doc.scope_of("foo"), Some("variable"));
}

/// The Go and JSON queries are written for "early pattern wins".
#[test]
fn go_and_json_earlier_pattern_wins() {
    let go = Doc::new(
        "go",
        "package main\n\nfunc main() {\n\tfmt.Println(x)\n\tfoo(1)\n\ty := p.Field\n}\n",
        SCOPES,
    );
    assert_eq!(go.scope_of("main()"), Some("function"));
    assert_eq!(go.scope_of("Println"), Some("function.method"));
    assert_eq!(go.scope_of("foo"), Some("function"));
    assert_eq!(go.scope_of("x)"), Some("variable"));
    assert_eq!(go.scope_of("Field"), Some("property"));

    let json = Doc::new("json", "{\"key\": \"value\", \"n\": [1, true]}\n", SCOPES);
    assert_eq!(json.scope_of("\"key\""), Some("string.special.key"));
    assert_eq!(json.scope_of("\"value\""), Some("string"));
    assert_eq!(json.scope_of("1"), Some("number"));
    assert_eq!(json.scope_of("true"), Some("constant.builtin"));
}

/// The TypeScript query extends JavaScript and must override it.
#[test]
fn typescript_extends_javascript() {
    let doc = Doc::new(
        "typescript",
        "function f(x: number): Array<string> { return []; }\ninterface I { a?: T }\nconst s = 'ы';\n",
        SCOPES,
    );
    assert_eq!(doc.scope_of("function"), Some("keyword"));
    assert_eq!(doc.scope_of("f("), Some("function"));
    assert_eq!(doc.scope_of("x:"), Some("variable.parameter"));
    assert_eq!(doc.scope_of("number"), Some("type.builtin"));
    assert_eq!(doc.scope_of("Array"), Some("type"));
    assert_eq!(doc.scope_of("<string"), Some("punctuation.bracket"));
    assert_eq!(doc.scope_of("interface"), Some("keyword"));
    assert_eq!(doc.scope_of("'ы'"), Some("string"));

    let tsx = Doc::new("tsx", "const a = <div className=\"x\">{n}</div>;\n", SCOPES);
    assert_eq!(tsx.scope_of("div"), Some("tag"));
    assert_eq!(tsx.scope_of("className"), Some("attribute"));
    assert_eq!(tsx.scope_of("const"), Some("keyword"));
}

#[test]
fn other_languages_basic_scopes() {
    let python = Doc::new("python", &sample("python"), SCOPES);
    assert_eq!(python.scope_of("def"), Some("keyword"));
    assert_eq!(python.scope_of("greet"), Some("function"));
    assert_eq!(python.scope_of("print"), Some("function.builtin"));
    assert_eq!(python.scope_of("\"\"\"Док"), Some("string"));
    assert_eq!(python.scope_of("# -*-"), Some("comment"));

    let bash = Doc::new("bash", &sample("bash"), SCOPES);
    assert_eq!(bash.scope_of("echo"), Some("function"));
    assert_eq!(bash.scope_of("heredoc строка"), Some("string"));
    assert_eq!(bash.scope_of("for f"), Some("keyword"));
    assert_eq!(bash.scope_of("#!/usr"), Some("comment"));

    let yaml = Doc::new("yaml", &sample("yaml"), SCOPES);
    assert_eq!(yaml.scope_of("name:"), Some("property"));
    assert_eq!(yaml.scope_of("flux"), Some("string"));
    assert_eq!(yaml.scope_of("null"), Some("constant.builtin"));
    assert_eq!(yaml.scope_of("# конф"), Some("comment"));

    let toml = Doc::new("toml", &sample("toml"), SCOPES);
    assert_eq!(toml.scope_of("dependencies"), Some("type"));
    // `(pair (bare_key)) @property` comes after `(bare_key) @type` and starts at the same place as
    // the key, so, as in tree-sitter-highlight, the key of the pair becomes a property.
    assert_eq!(toml.scope_of("name = \"flux\""), Some("property"));
    assert_eq!(toml.scope_of("= \"flux\""), Some("operator"));
    assert_eq!(toml.scope_of("\"flux\""), Some("string"));
    assert_eq!(toml.scope_of("c = 1979"), Some("property"));
    assert_eq!(toml.scope_of("1979"), Some("string.special"));

    let markdown = Doc::new("markdown", &sample("markdown"), SCOPES);
    assert_eq!(markdown.scope_of("# Заг"), Some("punctuation.special"));
    assert_eq!(markdown.scope_of("Заголовок"), Some("text.title"));
    assert_eq!(markdown.scope_of("- пункт"), Some("punctuation.special"));
    assert_eq!(markdown.scope_of("fn main"), Some("text.literal"));
}

#[test]
fn highlight_is_a_plain_index() {
    let doc = Doc::new("rust", "fn x() {}\n", &["comment", "keyword"]);
    let lines = doc.lines();
    assert_eq!(lines[0][0].highlight, Highlight(1));
}
