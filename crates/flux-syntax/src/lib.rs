//! Syntax highlighting via tree-sitter with incremental parsing.
//!
//! The crate knows nothing about the UI or colors:
//! - [`Language`] is a grammar plus a highlighting query, created lazily; languages come from
//!   plugins and are registered at run time ([`register`], stage 8.3); [`language_for_path`] picks
//!   the language by file name;
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
mod language;
/// The test languages: only in builds of the tests (`standard-languages`).
#[cfg(feature = "standard-languages")]
#[doc(hidden)]
pub mod standard;
mod syntax;
mod text;
mod wasm;

pub use highlight::{Highlight, HighlightMap, HighlightSpan};
pub use language::{
    GrammarSource, Language, LanguageConfig, Precedence, builtin_grammar, builtin_grammar_names,
    generation, language_by_name, language_for_path, languages, register, unregister,
};
pub use syntax::{ParseJob, ParseResult, Syntax};
pub use tree_sitter;
pub use wasm::{PARSE_LIMIT, set_wasm_cache_dir};
