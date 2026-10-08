//! Search without a UI:
//! - [`fuzzy`] — fuzzy search: synchronous over small lists ([`match_list`], the command palette)
//!   and on nucleo threads over the project paths ([`PathMatcher`], file search);
//! - [`files`] — the project root ([`find_vcs_root`]) and a file walk that respects `.gitignore`
//!   ([`walk_files`]);
//! - [`buffer`] — search and replace in a document ([`find_all`], [`replace_all`]);
//! - [`grep`] — project search ([`search_project`]), results streamed file by file.
//!
//! The query ([`SearchQuery`]) is the same for document search and project search: literal or
//! regular expression, case-sensitive or not, whole word.
//!
//! Positions in a document are characters (as throughout flux); columns in project search results
//! are also characters within a line. Anything that can take more than a millisecond is designed
//! for a background thread and accepts a cancellation flag.

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
