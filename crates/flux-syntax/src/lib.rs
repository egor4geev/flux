//! Подсветка синтаксиса через tree-sitter с инкрементальным разбором.
//!
//! Крейт не знает ни про UI, ни про цвета:
//! - [`Language`] — грамматика и запрос подсветки, создаются лениво;
//!   [`language_for_path`] выбирает язык по имени файла;
//! - [`Syntax`] — дерево документа: дешёвые правки по [`ChangeSet`](flux_core::ChangeSet)
//!   и разбор в виде [`ParseJob`], который приложение запускает где хочет
//!   (обычно в фоне — разбор не должен блокировать UI-поток);
//! - [`HighlightMap`] переводит имена capture (`keyword`, `function.method`…)
//!   в индексы областей темы, [`Syntax::highlight_lines`] отдаёт спаны строк.
//!
//! Позиции снаружи — символы (как во всём flux), внутри — байты UTF-8 и
//! точки tree-sitter.

mod edit;
mod highlight;
mod language;
mod syntax;
mod text;

pub use highlight::{Highlight, HighlightMap, HighlightSpan};
pub use language::{Language, language_by_name, language_for_path, languages};
pub use syntax::{ParseJob, ParseResult, Syntax};
pub use tree_sitter;
