//! Language registry (stage 8.3): languages come from plugins. A plugin's `[[languages]]` entry,
//! resolved by the application into a [`LanguageConfig`] (the query read from the plugin's files,
//! the grammar's bytes), is registered under the plugin's id ([`register`]); detection by file path
//! ([`language_for_path`]) and by name ([`language_by_name`]) look through what is registered.
//!
//! A grammar is either compiled into Flux ([`GrammarSource::Builtin`], [`builtin_grammar`]) or a
//! tree-sitter grammar compiled to WebAssembly that a plugin ships ([`GrammarSource::Wasm`]).
//!
//! The grammar and query are created lazily and only once (`OnceLock`): compiling a query costs
//! from a fraction of a millisecond up to ~20 ms (Rust in release), loading a WebAssembly grammar
//! compiles its module (1–50 ms, [`crate::wasm`]), and most languages won't need either during a
//! session. A [`Language`] is shared as an `Arc` and is `Sync`, so it can be handed to a background
//! thread; [`ParseJob::run`](crate::ParseJob::run) loads the grammar and compiles the query there,
//! so the UI thread doesn't pay for it.

use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use tree_sitter::Query;

/// Which pattern wins when the same span is captured by several patterns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Precedence {
    /// The later pattern in the query. This is how tree-sitter-highlight has worked since 0.21, and
    /// most queries are written for it (the generic `(identifier) @variable` comes first, special
    /// cases after).
    #[default]
    LastPattern,
    /// The earlier pattern: queries left over from the days of the old tree-sitter-highlight
    /// (special cases come before generic ones).
    FirstPattern,
}

/// Where a language's grammar comes from.
#[derive(Clone, PartialEq)]
pub enum GrammarSource {
    /// A grammar compiled into Flux, by name ([`builtin_grammar`]).
    Builtin(String),
    /// A tree-sitter grammar compiled to WebAssembly, shipped by a plugin. `name` is the grammar's
    /// own name: the module exports `tree_sitter_<name>`.
    Wasm { name: String, bytes: Arc<[u8]> },
}

impl fmt::Debug for GrammarSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GrammarSource::Builtin(name) => f.debug_tuple("Builtin").field(name).finish(),
            GrammarSource::Wasm { name, bytes } => f
                .debug_struct("Wasm")
                .field("name", name)
                .field("bytes", &bytes.len())
                .finish(),
        }
    }
}

/// A language as a plugin describes it, resolved: the highlighting query is its text.
#[derive(Debug, Clone, PartialEq)]
pub struct LanguageConfig {
    /// The key: "rust", "typescript", "tsx". Also a name for code fences.
    pub name: String,
    /// Human-readable name: "Rust", "TypeScript".
    pub display_name: String,
    /// Extensions without the dot; matched in any case.
    pub extensions: Vec<String>,
    /// Exact file names: `Cargo.lock`, `.bashrc`.
    pub file_names: Vec<String>,
    /// More names for code fences and lookups by name: "rs", "py", "sh".
    pub aliases: Vec<String>,
    pub grammar: GrammarSource,
    /// The highlighting query (the plugin's query files, concatenated).
    pub highlights: String,
    pub precedence: Precedence,
}

pub struct Language {
    config: LanguageConfig,
    /// The plugin that registered it.
    owner: String,
    grammar: OnceLock<Result<tree_sitter::Language, String>>,
    query: OnceLock<Option<Query>>,
    /// A parse of its WebAssembly grammar outlived [`crate::wasm::PARSE_LIMIT`]: the grammar is
    /// not parsed any more.
    hung: AtomicBool,
}

/// Why a language stopped being parsed: its grammar's parse never returned.
const HUNG: &str = "the grammar stopped responding";

/// Loads a grammar compiled into Flux.
type LoadGrammar = fn() -> tree_sitter::Language;

/// The grammars compiled into Flux, by name: JavaScript and TypeScript come with Flux (the bundled
/// plugin `flux.javascript`); every other language is a plugin with a WebAssembly grammar.
static BUILTIN_GRAMMARS: &[(&str, LoadGrammar)] = &[
    ("javascript", || tree_sitter_javascript::LANGUAGE.into()),
    ("typescript", || {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }),
    ("tsx", || tree_sitter_typescript::LANGUAGE_TSX.into()),
];

/// The native grammars of the test languages ([`crate::standard`]): only in builds of the tests.
#[cfg(feature = "standard-languages")]
static STANDARD_GRAMMARS: &[(&str, LoadGrammar)] = &[
    ("rust", || tree_sitter_rust::LANGUAGE.into()),
    ("toml", || tree_sitter_toml_ng::LANGUAGE.into()),
    ("json", || tree_sitter_json::LANGUAGE.into()),
    ("markdown", || tree_sitter_md::LANGUAGE.into()),
    ("yaml", || tree_sitter_yaml::LANGUAGE.into()),
    ("bash", || tree_sitter_bash::LANGUAGE.into()),
    ("python", || tree_sitter_python::LANGUAGE.into()),
    ("go", || tree_sitter_go::LANGUAGE.into()),
];

#[cfg(not(feature = "standard-languages"))]
static STANDARD_GRAMMARS: &[(&str, LoadGrammar)] = &[];

/// A grammar compiled into Flux.
pub fn builtin_grammar(name: &str) -> Option<tree_sitter::Language> {
    BUILTIN_GRAMMARS
        .iter()
        .chain(STANDARD_GRAMMARS)
        .find(|(builtin, _)| *builtin == name)
        .map(|(_, load)| load())
}

/// The names of the grammars compiled into Flux.
pub fn builtin_grammar_names() -> Vec<&'static str> {
    BUILTIN_GRAMMARS
        .iter()
        .chain(STANDARD_GRAMMARS)
        .map(|(name, _)| *name)
        .collect()
}

/// The registered languages by owner, in the order owners were first registered.
struct Registry {
    owners: Vec<(String, Vec<Arc<Language>>)>,
    generation: u64,
}

static REGISTRY: RwLock<Registry> = RwLock::new(Registry {
    owners: Vec::new(),
    generation: 0,
});

/// Registers the languages of `owner` (a plugin id), replacing what it registered before. A
/// language whose config is unchanged keeps its loaded grammar and compiled query. An empty list
/// is the same as [`unregister`]. Returns whether anything changed ([`generation`] bumps then).
pub fn register(owner: &str, configs: Vec<LanguageConfig>) -> bool {
    let mut registry = REGISTRY.write().unwrap();
    let previous = registry.owners.iter().position(|(known, _)| known == owner);
    if configs.is_empty() {
        let Some(index) = previous else {
            return false;
        };
        registry.owners.remove(index);
        registry.generation += 1;
        return true;
    }
    let old = previous.map(|index| std::mem::take(&mut registry.owners[index].1));
    let unchanged = old
        .as_ref()
        .is_some_and(|old| old.iter().map(|l| &l.config).eq(configs.iter()));
    let languages = configs
        .into_iter()
        .map(|config| {
            old.iter()
                .flatten()
                .find(|language| language.config == config)
                .cloned()
                .unwrap_or_else(|| Arc::new(Language::new(owner, config)))
        })
        .collect();
    match previous {
        Some(index) => registry.owners[index].1 = languages,
        None => registry.owners.push((owner.to_string(), languages)),
    }
    if !unchanged {
        registry.generation += 1;
    }
    !unchanged
}

/// Removes the languages of `owner`.
pub fn unregister(owner: &str) {
    register(owner, Vec::new());
}

/// Bumps every time the registered languages change: documents look their language up again.
pub fn generation() -> u64 {
    REGISTRY.read().unwrap().generation
}

/// All registered languages, in registration order.
pub fn languages() -> Vec<Arc<Language>> {
    let registry = REGISTRY.read().unwrap();
    registry
        .owners
        .iter()
        .flat_map(|(_, languages)| languages.iter().cloned())
        .collect()
}

/// The language by its name or one of its aliases, in any case. A later registration wins.
pub fn language_by_name(name: &str) -> Option<Arc<Language>> {
    let registry = REGISTRY.read().unwrap();
    let all = registry.owners.iter().flat_map(|(_, languages)| languages);
    let found: Vec<&Arc<Language>> = all
        .filter(|language| {
            language.config.name.eq_ignore_ascii_case(name)
                || language
                    .config
                    .aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(name))
        })
        .collect();
    found.last().map(|language| (*language).clone())
}

/// The language by file name (`Cargo.lock`, `.zshrc`), otherwise by extension (in any case). A
/// later registration wins: a plugin installed over a bundled one takes its files.
pub fn language_for_path(path: &Path) -> Option<Arc<Language>> {
    let file_name = path.file_name()?.to_str()?;
    let registry = REGISTRY.read().unwrap();
    let all: Vec<&Arc<Language>> = registry
        .owners
        .iter()
        .flat_map(|(_, languages)| languages)
        .collect();
    if let Some(language) = all
        .iter()
        .rev()
        .find(|language| language.config.file_names.iter().any(|n| n == file_name))
    {
        return Some((*language).clone());
    }
    let extension = path.extension()?.to_str()?;
    all.iter()
        .rev()
        .find(|language| {
            language
                .config
                .extensions
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
        .map(|language| (*language).clone())
}

impl Language {
    pub(crate) fn new(owner: &str, config: LanguageConfig) -> Self {
        Self {
            config,
            owner: owner.to_string(),
            grammar: OnceLock::new(),
            query: OnceLock::new(),
            hung: AtomicBool::new(false),
        }
    }

    /// The key: "rust", "tsx".
    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// Human-readable name: "Rust", "TypeScript".
    pub fn display_name(&self) -> &str {
        &self.config.display_name
    }

    /// The plugin that registered the language.
    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn config(&self) -> &LanguageConfig {
        &self.config
    }

    /// The grammar is a plugin's WebAssembly module (not compiled into Flux): it is never parsed
    /// on the UI thread, and each parse runs on a thread of its own ([`crate::wasm`]).
    pub fn is_wasm(&self) -> bool {
        matches!(self.config.grammar, GrammarSource::Wasm { .. })
    }

    /// The tree-sitter grammar, loaded on first access: a WebAssembly grammar is compiled then,
    /// which takes milliseconds — not on the UI thread ([`Language::loaded_grammar`] is for it).
    /// `None` if it can't be loaded: an ABI the linked tree-sitter doesn't support, a broken module
    /// ([`Language::grammar_error`] says why).
    pub fn grammar(&self) -> Option<&tree_sitter::Language> {
        self.grammar
            .get_or_init(|| load_grammar(&self.config.grammar))
            .as_ref()
            .ok()
    }

    /// The grammar if it is already loaded. Never loads it, so it can be called from the UI thread.
    pub fn loaded_grammar(&self) -> Option<&tree_sitter::Language> {
        self.grammar.get().and_then(|grammar| grammar.as_ref().ok())
    }

    /// Why the grammar can't be used: it couldn't be loaded, or a parse of it never returned;
    /// `None` while it works or hasn't been tried.
    pub fn grammar_error(&self) -> Option<&str> {
        if self.hung.load(Ordering::Relaxed) {
            return Some(HUNG);
        }
        self.grammar
            .get()
            .and_then(|grammar| grammar.as_ref().err())
            .map(String::as_str)
    }

    /// Whether the grammar is known to be unusable (tried and failed, or stuck). A language that
    /// hasn't been loaded yet is not: it may still load.
    pub fn grammar_failed(&self) -> bool {
        self.grammar_error().is_some()
    }

    /// A parse of the grammar outlived its limit: it is not parsed any more.
    pub(crate) fn mark_hung(&self) {
        self.hung.store(true, Ordering::Relaxed);
    }

    /// The highlighting query; it is compiled on first access, which takes up to ~20 ms, so it is
    /// better not to do that on the UI thread. `None` if the grammar failed to load or the query
    /// failed to compile.
    pub fn query(&self) -> Option<&Query> {
        self.query
            .get_or_init(|| {
                let grammar = self.grammar()?;
                let mut query = Query::new(grammar, &self.config.highlights).ok()?;
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

    pub fn precedence(&self) -> Precedence {
        self.config.precedence
    }
}

/// Loads a grammar and checks its ABI against the linked tree-sitter.
fn load_grammar(source: &GrammarSource) -> Result<tree_sitter::Language, String> {
    let grammar = match source {
        GrammarSource::Builtin(name) => {
            builtin_grammar(name).ok_or_else(|| format!("no grammar \"{name}\" in Flux"))?
        }
        GrammarSource::Wasm { name, bytes } => crate::wasm::load(name, bytes)?,
    };
    let abi = grammar.abi_version();
    let supported = tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION..=tree_sitter::LANGUAGE_VERSION;
    if !supported.contains(&abi) {
        return Err(format!(
            "the grammar's ABI {abi} is not supported (Flux reads {}–{})",
            supported.start(),
            supported.end()
        ));
    }
    Ok(grammar)
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
        f.debug_tuple("Language").field(&self.config.name).finish()
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
    use crate::standard;

    fn name_for(path: &str) -> Option<String> {
        standard::register();
        language_for_path(Path::new(path)).map(|language| language.name().to_string())
    }

    #[test]
    fn detects_language_by_extension_and_file_name() {
        let name = |path: &str| name_for(path);
        assert_eq!(name("src/main.rs").as_deref(), Some("rust"));
        assert_eq!(name("/abs/path/LIB.RS").as_deref(), Some("rust"));
        assert_eq!(name("Cargo.toml").as_deref(), Some("toml"));
        assert_eq!(name("Cargo.lock").as_deref(), Some("toml"));
        assert_eq!(name("package.json").as_deref(), Some("json"));
        assert_eq!(name(".vscode/settings.jsonc").as_deref(), Some("json"));
        assert_eq!(name("README.md").as_deref(), Some("markdown"));
        assert_eq!(name("ci.yml").as_deref(), Some("yaml"));
        assert_eq!(name("ci.yaml").as_deref(), Some("yaml"));
        assert_eq!(name("/home/me/.bashrc").as_deref(), Some("bash"));
        assert_eq!(name(".zshrc").as_deref(), Some("bash"));
        assert_eq!(name("build.sh").as_deref(), Some("bash"));
        assert_eq!(name("setup.py").as_deref(), Some("python"));
        assert_eq!(name("stubs.pyi").as_deref(), Some("python"));
        assert_eq!(name("main.go").as_deref(), Some("go"));
        assert_eq!(name("index.js").as_deref(), Some("javascript"));
        assert_eq!(name("index.mjs").as_deref(), Some("javascript"));
        assert_eq!(name("index.cjs").as_deref(), Some("javascript"));
        assert_eq!(name("App.jsx").as_deref(), Some("javascript"));
        assert_eq!(name("index.ts").as_deref(), Some("typescript"));
        assert_eq!(name("index.d.mts").as_deref(), Some("typescript"));
        assert_eq!(name("App.tsx").as_deref(), Some("tsx"));
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
    fn finds_language_by_name_and_alias() {
        standard::register();
        assert_eq!(
            language_by_name("tsx").map(|l| l.name().to_string()),
            Some("tsx".into())
        );
        assert_eq!(
            language_by_name("RS").map(|l| l.name().to_string()),
            Some("rust".into())
        );
        assert!(language_by_name("cobol").is_none());
        // Other tests register and remove languages of their own meanwhile.
        for language in languages()
            .into_iter()
            .filter(|language| language.owner() == standard::OWNER)
        {
            assert_eq!(language_by_name(language.name()).as_ref(), Some(&language));
        }
    }

    #[test]
    fn every_grammar_loads_and_every_query_compiles() {
        standard::register();
        for language in languages()
            .into_iter()
            .filter(|language| language.owner() == standard::OWNER)
        {
            assert!(
                language.grammar().is_some(),
                "{}: {:?}",
                language.name(),
                language.grammar_error()
            );
            assert!(language.query().is_some(), "{}: query", language.name());
            assert!(!language.capture_names().is_empty(), "{}", language.name());
        }
    }

    fn config(name: &str, extension: &str) -> LanguageConfig {
        LanguageConfig {
            name: name.into(),
            display_name: name.to_uppercase(),
            extensions: vec![extension.into()],
            file_names: Vec::new(),
            aliases: Vec::new(),
            grammar: GrammarSource::Builtin("json".into()),
            highlights: "(string) @string".into(),
            precedence: Precedence::LastPattern,
        }
    }

    #[test]
    fn registering_replaces_an_owners_languages_and_keeps_unchanged_ones() {
        register(
            "test.replace",
            vec![config("zzreplace-a", "zza"), config("zzreplace-b", "zzb")],
        );
        let a = language_by_name("zzreplace-a").unwrap();
        let before = generation();
        assert!(register("test.replace", vec![config("zzreplace-a", "zza")]));
        assert!(generation() > before);
        assert!(Arc::ptr_eq(&language_by_name("zzreplace-a").unwrap(), &a));
        assert!(language_by_name("zzreplace-b").is_none());
        // Other tests register languages meanwhile: the global generation is theirs too.
        assert!(
            !register("test.replace", vec![config("zzreplace-a", "zza")]),
            "nothing changed"
        );
        unregister("test.replace");
        assert!(language_by_name("zzreplace-a").is_none());
    }

    #[test]
    fn a_later_owner_takes_the_files() {
        register("test.first", vec![config("zzfirst", "zzx")]);
        register("test.second", vec![config("zzsecond", "zzx")]);
        let found = language_for_path(Path::new("a.ZZX")).unwrap();
        assert_eq!(found.name(), "zzsecond");
        assert_eq!(found.owner(), "test.second");
        unregister("test.second");
        assert_eq!(
            language_for_path(Path::new("a.zzx")).unwrap().name(),
            "zzfirst"
        );
        unregister("test.first");
    }

    #[test]
    fn a_broken_grammar_says_why() {
        let mut broken = config("zzbroken", "zzbroken");
        broken.grammar = GrammarSource::Builtin("no-such-grammar".into());
        register("test.broken", vec![broken]);
        let language = language_by_name("zzbroken").unwrap();
        assert!(language.loaded_grammar().is_none());
        assert!(language.grammar().is_none());
        assert!(language.grammar_failed());
        assert!(
            language
                .grammar_error()
                .unwrap()
                .contains("no-such-grammar")
        );
        unregister("test.broken");
    }

    #[test]
    fn compiled_query_never_compiles() {
        // Its own language: other tests compile the queries of the registered ones.
        let mut own = config("zzquery", "zzquery");
        own.grammar = GrammarSource::Builtin("rust".into());
        own.highlights = tree_sitter_rust::HIGHLIGHTS_QUERY.into();
        register("test.query", vec![own]);
        let language = language_by_name("zzquery").unwrap();
        assert!(language.compiled_query().is_none());
        assert!(crate::HighlightMap::try_new(&language, &["keyword"]).is_none());
        assert!(
            language.compiled_query().is_none(),
            "try_new must not compile"
        );
        assert!(language.query().is_some());
        let map = crate::HighlightMap::try_new(&language, &["keyword"]).unwrap();
        let keyword = language
            .capture_names()
            .iter()
            .position(|name| *name == "keyword");
        assert_eq!(map.get(keyword.unwrap() as u32), Some(crate::Highlight(0)));
        unregister("test.query");
    }

    #[test]
    fn display_names_are_set() {
        standard::register();
        for language in languages()
            .into_iter()
            .filter(|language| language.owner() == standard::OWNER)
        {
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
        standard::register();
        let language = language_by_name("rust").unwrap();
        let compiled = std::thread::spawn(move || language.query().is_some());
        assert!(compiled.join().unwrap());
    }
}
