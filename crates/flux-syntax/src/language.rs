//! Language registry: the tree-sitter grammar, the highlighting query, and language detection by
//! file path.
//!
//! The grammar and query are created lazily and only once (`OnceLock`): compiling a query costs
//! from a fraction of a millisecond up to ~20 ms (Rust in release), and most languages won't need
//! it during a session. [`Language`] lives in a static table and is `Sync`, so a `&'static
//! Language` can be handed to a background thread; [`ParseJob::run`](crate::ParseJob::run) compiles
//! the query there as well, so the UI thread doesn't pay for it.

use std::fmt;
use std::path::Path;
use std::sync::OnceLock;

use tree_sitter::Query;

/// Which pattern wins when the same span is captured by several patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Precedence {
    /// The later pattern in the query. This is how tree-sitter-highlight has worked since 0.21, and
    /// most queries are written for it (the generic `(identifier) @variable` comes first, special
    /// cases after).
    LastPattern,
    /// The earlier pattern: queries left over from the days of the old tree-sitter-highlight
    /// (special cases come before generic ones).
    FirstPattern,
}

pub struct Language {
    name: &'static str,
    /// Human-readable name: "Rust", "TypeScript".
    display_name: &'static str,
    /// Extensions without the dot, in lowercase.
    extensions: &'static [&'static str],
    /// Exact file names: `Cargo.lock`, `.bashrc`.
    file_names: &'static [&'static str],
    load_grammar: fn() -> tree_sitter::Language,
    /// Parts of the highlighting query, concatenated in order.
    queries: &'static [&'static str],
    precedence: Precedence,
    grammar: OnceLock<Option<tree_sitter::Language>>,
    query: OnceLock<Option<Query>>,
}

/// JavaScript function parameters: the file is in the grammar but not in its Rust binding.
const JAVASCRIPT_PARAMS_QUERY: &str = include_str!("../queries/javascript-params.scm");

static LANGUAGES: [Language; 11] = [
    Language::new(
        "rust",
        "Rust",
        &["rs"],
        &[],
        || tree_sitter_rust::LANGUAGE.into(),
        &[tree_sitter_rust::HIGHLIGHTS_QUERY],
        Precedence::LastPattern,
    ),
    Language::new(
        "toml",
        "TOML",
        &["toml"],
        &["Cargo.lock", "Pipfile", "poetry.lock", "uv.lock"],
        || tree_sitter_toml_ng::LANGUAGE.into(),
        &[tree_sitter_toml_ng::HIGHLIGHTS_QUERY],
        Precedence::LastPattern,
    ),
    // The JSON query puts the keys `(pair key: …)` before the generic `(string)`.
    Language::new(
        "json",
        "JSON",
        &["json", "jsonc"],
        &["flake.lock"],
        || tree_sitter_json::LANGUAGE.into(),
        &[tree_sitter_json::HIGHLIGHTS_QUERY],
        Precedence::FirstPattern,
    ),
    // Block grammar only: inline markup and code in blocks are injections.
    Language::new(
        "markdown",
        "Markdown",
        &["md", "markdown"],
        &[],
        || tree_sitter_md::LANGUAGE.into(),
        &[tree_sitter_md::HIGHLIGHT_QUERY_BLOCK],
        Precedence::LastPattern,
    ),
    Language::new(
        "yaml",
        "YAML",
        &["yaml", "yml"],
        &[".clang-format", ".clang-tidy"],
        || tree_sitter_yaml::LANGUAGE.into(),
        &[tree_sitter_yaml::HIGHLIGHTS_QUERY],
        Precedence::LastPattern,
    ),
    Language::new(
        "bash",
        "Shell",
        &["sh", "bash", "zsh"],
        &[
            ".bashrc",
            ".bash_profile",
            ".bash_aliases",
            ".bash_logout",
            ".profile",
            ".zshrc",
            ".zshenv",
            ".zprofile",
            ".zlogin",
            ".zlogout",
            "PKGBUILD",
        ],
        || tree_sitter_bash::LANGUAGE.into(),
        &[tree_sitter_bash::HIGHLIGHT_QUERY],
        Precedence::LastPattern,
    ),
    Language::new(
        "python",
        "Python",
        &["py", "pyi", "pyw"],
        &[],
        || tree_sitter_python::LANGUAGE.into(),
        &[tree_sitter_python::HIGHLIGHTS_QUERY],
        Precedence::LastPattern,
    ),
    // The Go query puts `(identifier) @variable` and `(field_identifier) @property` after function
    // and method calls.
    Language::new(
        "go",
        "Go",
        &["go"],
        &[],
        || tree_sitter_go::LANGUAGE.into(),
        &[tree_sitter_go::HIGHLIGHTS_QUERY],
        Precedence::FirstPattern,
    ),
    // The order of the parts follows the grammar's tree-sitter.json.
    Language::new(
        "javascript",
        "JavaScript",
        &["js", "mjs", "cjs", "jsx"],
        &[],
        || tree_sitter_javascript::LANGUAGE.into(),
        &[
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
            JAVASCRIPT_PARAMS_QUERY,
        ],
        Precedence::LastPattern,
    ),
    // The TypeScript query supplements the JavaScript query. The grammar's tree-sitter.json lists
    // it first, which is the order from the "earlier pattern wins" days. With the current "later
    // pattern wins" rule, the TypeScript parts must come after JavaScript, otherwise the generic
    // `(identifier) @variable` from JS would override them.
    Language::new(
        "typescript",
        "TypeScript",
        &["ts", "mts", "cts"],
        &[],
        || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        &[
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ],
        Precedence::LastPattern,
    ),
    Language::new(
        "tsx",
        "TSX",
        &["tsx"],
        &[],
        || tree_sitter_typescript::LANGUAGE_TSX.into(),
        &[
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ],
        Precedence::LastPattern,
    ),
];

/// All known languages.
pub fn languages() -> &'static [Language] {
    &LANGUAGES
}

pub fn language_by_name(name: &str) -> Option<&'static Language> {
    LANGUAGES.iter().find(|language| language.name == name)
}

/// The language by file name (`Cargo.lock`, `.zshrc`), otherwise by extension.
pub fn language_for_path(path: &Path) -> Option<&'static Language> {
    let file_name = path.file_name()?.to_str()?;
    if let Some(language) = LANGUAGES
        .iter()
        .find(|language| language.file_names.contains(&file_name))
    {
        return Some(language);
    }
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|language| language.extensions.contains(&extension.as_str()))
}

impl Language {
    const fn new(
        name: &'static str,
        display_name: &'static str,
        extensions: &'static [&'static str],
        file_names: &'static [&'static str],
        load_grammar: fn() -> tree_sitter::Language,
        queries: &'static [&'static str],
        precedence: Precedence,
    ) -> Self {
        Self {
            name,
            display_name,
            extensions,
            file_names,
            load_grammar,
            queries,
            precedence,
            grammar: OnceLock::new(),
            query: OnceLock::new(),
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn display_name(&self) -> &'static str {
        self.display_name
    }

    /// The tree-sitter grammar. `None` if its ABI is not supported by the linked version of
    /// tree-sitter.
    pub fn grammar(&self) -> Option<&tree_sitter::Language> {
        self.grammar
            .get_or_init(|| {
                let grammar = (self.load_grammar)();
                let abi = grammar.abi_version();
                (tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION..=tree_sitter::LANGUAGE_VERSION)
                    .contains(&abi)
                    .then_some(grammar)
            })
            .as_ref()
    }

    /// The highlighting query; it is compiled on first access, which takes up to ~20 ms, so it is
    /// better not to do that on the UI thread. `None` if the grammar failed to load or the query
    /// failed to compile.
    pub fn query(&self) -> Option<&Query> {
        self.query
            .get_or_init(|| {
                let grammar = self.grammar()?;
                let mut query = Query::new(grammar, &self.queries.join("\n")).ok()?;
                disable_property_patterns(&mut query);
                Some(query)
            })
            .as_ref()
    }

    /// The query, if it has already been compiled. Doesn't compile or wait for another caller's
    /// compilation, so it can be called from the UI thread.
    pub fn compiled_query(&self) -> Option<&Query> {
        self.query.get().and_then(Option::as_ref)
    }

    /// The capture names of the highlighting query; an index in this list is the capture index.
    pub fn capture_names(&self) -> &[&str] {
        self.query().map_or(&[], Query::capture_names)
    }

    pub(crate) fn precedence(&self) -> Precedence {
        self.precedence
    }
}

/// Predicates that [`tree_sitter::QueryCursor`] checks itself: `#eq?`, `#match?`, `#any-of?` and
/// their `not-`/`any-` variants. It skips the others, and so do we:
/// - `#is-not? local` (and any `#is-not?`): we don't track local variables, so no node is "local":
///   the condition is satisfied and the pattern applies;
/// - `#set!`: pattern properties are needed by injections and locals, neither of which we have
///   here;
/// - unknown general predicates (`#lua-match?`…): like tree-sitter-highlight, we don't filter the
///   match.
///
/// The only exception is `#is?`: we have no way to assert a property, so such patterns are disabled
/// rather than firing on everything.
fn disable_property_patterns(query: &mut Query) {
    for pattern in 0..query.pattern_count() {
        if query
            .property_predicates(pattern)
            .iter()
            .any(|(_, is_positive)| *is_positive)
        {
            query.disable_pattern(pattern);
        }
    }
}

impl fmt::Debug for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Language").field(&self.name).finish()
    }
}

impl PartialEq for Language {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Eq for Language {}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_for(path: &str) -> Option<&'static str> {
        language_for_path(Path::new(path)).map(Language::name)
    }

    #[test]
    fn detects_language_by_extension_and_file_name() {
        assert_eq!(name_for("src/main.rs"), Some("rust"));
        assert_eq!(name_for("/abs/path/LIB.RS"), Some("rust"));
        assert_eq!(name_for("Cargo.toml"), Some("toml"));
        assert_eq!(name_for("Cargo.lock"), Some("toml"));
        assert_eq!(name_for("package.json"), Some("json"));
        assert_eq!(name_for(".vscode/settings.jsonc"), Some("json"));
        assert_eq!(name_for("README.md"), Some("markdown"));
        assert_eq!(name_for("ci.yml"), Some("yaml"));
        assert_eq!(name_for("ci.yaml"), Some("yaml"));
        assert_eq!(name_for("/home/me/.bashrc"), Some("bash"));
        assert_eq!(name_for(".zshrc"), Some("bash"));
        assert_eq!(name_for("build.sh"), Some("bash"));
        assert_eq!(name_for("setup.py"), Some("python"));
        assert_eq!(name_for("stubs.pyi"), Some("python"));
        assert_eq!(name_for("main.go"), Some("go"));
        assert_eq!(name_for("index.js"), Some("javascript"));
        assert_eq!(name_for("index.mjs"), Some("javascript"));
        assert_eq!(name_for("index.cjs"), Some("javascript"));
        assert_eq!(name_for("App.jsx"), Some("javascript"));
        assert_eq!(name_for("index.ts"), Some("typescript"));
        assert_eq!(name_for("index.d.mts"), Some("typescript"));
        assert_eq!(name_for("App.tsx"), Some("tsx"));
    }

    #[test]
    fn unknown_files_have_no_language() {
        assert_eq!(name_for("Makefile"), None);
        assert_eq!(name_for("notes.txt"), None);
        assert_eq!(name_for("no_extension"), None);
        assert_eq!(name_for(".gitignore"), None);
        assert_eq!(name_for(""), None);
        assert_eq!(name_for("/"), None);
    }

    #[test]
    fn finds_language_by_name() {
        assert_eq!(language_by_name("tsx").map(Language::name), Some("tsx"));
        assert!(language_by_name("cobol").is_none());
        for language in languages() {
            assert_eq!(language_by_name(language.name()), Some(language));
        }
    }

    #[test]
    fn every_grammar_loads_and_every_query_compiles() {
        for language in languages() {
            assert!(
                language.grammar().is_some(),
                "{}: grammar ABI",
                language.name()
            );
            assert!(language.query().is_some(), "{}: query", language.name());
            assert!(!language.capture_names().is_empty(), "{}", language.name());
        }
    }

    #[test]
    fn compiled_query_never_compiles() {
        // Its own instance: other tests compile the queries from the static table.
        let language: &'static Language = Box::leak(Box::new(Language::new(
            "rust",
            "Rust",
            &["rs"],
            &[],
            || tree_sitter_rust::LANGUAGE.into(),
            &[tree_sitter_rust::HIGHLIGHTS_QUERY],
            Precedence::LastPattern,
        )));
        assert!(language.compiled_query().is_none());
        assert!(crate::HighlightMap::try_new(language, &["keyword"]).is_none());
        assert!(
            language.compiled_query().is_none(),
            "try_new must not compile"
        );
        assert!(language.query().is_some());
        let map = crate::HighlightMap::try_new(language, &["keyword"]).unwrap();
        let keyword = language
            .capture_names()
            .iter()
            .position(|name| *name == "keyword");
        assert_eq!(map.get(keyword.unwrap() as u32), Some(crate::Highlight(0)));
    }

    #[test]
    fn display_names_are_set() {
        for language in languages() {
            assert!(!language.display_name().is_empty(), "{}", language.name());
        }
        assert_eq!(
            language_by_name("typescript").unwrap().display_name(),
            "TypeScript"
        );
    }

    #[test]
    fn language_is_shareable_between_threads() {
        fn assert_sync<T: Sync + Send>() {}
        assert_sync::<Language>();
        let language = language_by_name("rust").unwrap();
        let compiled = std::thread::spawn(move || language.query().is_some());
        assert!(compiled.join().unwrap());
    }
}
