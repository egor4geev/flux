//! Syntax highlighting via tree-sitter with incremental parsing.
//!
//! The crate knows nothing about the UI or colors:
//! - [`Language`] is a grammar plus a highlighting query, created lazily; [`language_for_path`]
//!   picks the language by file name;
//! - [`Syntax`] is the document tree: cheap edits via [`ChangeSet`](flux_core::ChangeSet) and
//!   parsing as a [`ParseJob`], which the application runs wherever it likes (usually in the
//!   background, since parsing must not block the UI thread);
//! - [`HighlightMap`] maps capture names (`keyword`, `function.method`…) to theme scope indices,
//!   and [`Syntax::highlight_lines`] returns the spans for lines.
//!
//! Positions are characters on the outside (as everywhere in flux) and UTF-8 bytes and tree-sitter
//! points on the inside.

mod edit;
mod highlight;
mod language;
mod syntax;
mod text;

pub use highlight::{Highlight, HighlightMap, HighlightSpan};
pub use language::{Language, language_by_name, language_for_path, languages};
pub use syntax::{ParseJob, ParseResult, Syntax};
pub use tree_sitter;
