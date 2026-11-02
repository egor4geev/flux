//! Syntax highlighting via tree-sitter with incremental parsing.
//!
//! The crate knows nothing about the UI or colors:
//! - [`Language`] is a grammar plus a highlighting query, created lazily; languages come from
//!   plugins and are registered at run time ([`register`], stage 8.3); [`language_for_path`] picks
//!   the language by file name (exact names, then patterns — [`glob_matches`] — then extensions);
//! - a language may embed others (injections, plugin API 0.4): an HTML `<script>`, a Markdown code
//!   block — [`Syntax`] parses them as layers over the document's tree (`injection`), and
//!   highlighting paints them over their host;
//! - [`Syntax`] is the document tree: cheap edits via [`ChangeSet`](flux_core::ChangeSet) and
//!   parsing as a [`ParseJob`], which the application runs wherever it likes (usually in the
//!   background, since parsing must not block the UI thread);
//! - [`HighlightMap`] maps capture names (`keyword`, `function.method`…) to theme scope indices,
//!   and [`Syntax::highlight_lines`] returns the spans for lines;
//! - grammars are compiled into Flux (JavaScript, TypeScript) or WebAssembly modules of plugins,
//!   run by tree-sitter on wasmtime (`wasm`): never on the UI thread, each parse on a thread of its
//!   own ([`ParseJob::spawn`]), under a watchdog; [`set_wasm_cache_dir`] keeps compiled grammars on
//!   disk.
//!
//! Positions are characters on the outside (as everywhere in flux) and UTF-8 bytes and tree-sitter
//! points on the inside.

mod edit;
mod highlight;
mod injection;
mod language;
/// The test languages: only in builds of the tests (`standard-languages`).
#[cfg(feature = "standard-languages")]
#[doc(hidden)]
pub mod standard;
mod syntax;
mod text;
mod wasm;

pub use flux_platform::glob::{check_glob, glob_matches};
pub use highlight::{Highlight, HighlightMap, HighlightMaps, HighlightSpan};
pub use language::{
    GrammarSource, Language, LanguageConfig, Precedence, builtin_grammar, builtin_grammar_names,
    generation, injected_language, language_by_name, language_for_path, languages, register,
    unregister,
};
pub use syntax::{Injection, ParseJob, ParseResult, Syntax};
pub use tree_sitter;
pub use wasm::{PARSE_LIMIT, set_wasm_cache_dir};
