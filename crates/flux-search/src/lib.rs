//! Поиск без UI:
//! - [`fuzzy`] — нечёткий поиск: синхронно по небольшим спискам ([`match_list`], палитра
//!   команд) и в потоках nucleo по путям проекта ([`PathMatcher`], поиск файла);
//! - [`files`] — корень проекта ([`find_vcs_root`]) и обход файлов с учётом `.gitignore`
//!   ([`walk_files`]);
//! - [`buffer`] — поиск и замена в документе ([`find_all`], [`replace_all`]);
//! - [`grep`] — поиск по проекту ([`search_project`]), результаты потоком по файлам.
//!
//! Запрос ([`SearchQuery`]) один для поиска в документе и по проекту: буквально или
//! регулярным выражением, с учётом регистра или без, целым словом.
//!
//! Позиции в документе — символы (как во всём flux); колонки в результатах поиска по
//! проекту — тоже символы внутри строки. Всё, что может занять больше миллисекунды,
//! рассчитано на фоновый поток и принимает флаг отмены.

pub mod buffer;
pub mod files;
pub mod fuzzy;
pub mod grep;
mod query;

pub use buffer::{BufferMatches, MAX_BUFFER_MATCHES, find_all, replace_all, replacement_for};
pub use files::{MAX_FILES, WalkSummary, find_vcs_root, walk_files};
pub use fuzzy::{FuzzyMatch, PathInjector, PathMatch, PathMatcher, match_list};
pub use grep::{FileMatches, GrepOptions, GrepSummary, LineMatch, search_project};
pub use query::{QueryError, SearchQuery};
