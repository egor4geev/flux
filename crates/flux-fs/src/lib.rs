//! Project files, without UI:
//! - [`rules`] — which files belong to the project: VCS metadata directories and junk are hidden,
//!   hidden files are visible, and `.gitignore` is honored by ripgrep's rules ([`project_walker`]);
//!   search (`flux-search`) uses the same rules;
//! - [`list`] — the contents of a single directory for the tree: directories first, natural name
//!   order, ignored entries marked ([`list_dir`]);
//! - [`tree`] — the file tree state: read and expanded directories, visible rows;
//! - [`ops`] — file operations: create, rename, move, copy, delete to the Trash, without
//!   overwriting existing files;
//! - [`watch`] — watching for changes on disk ([`Watcher`]).
//!
//! Directory reads and operations block the thread, so call them from the background (gpui's
//! `background_spawn`).

pub mod list;
pub mod ops;
pub mod rules;
pub mod tree;
pub mod watch;

pub use list::{DirEntry, EntryKind, compare_names, list_dir};
pub use ops::{copy_into, create_dir, create_file, move_into, remap, rename, trash, validate_name};
pub use rules::{is_skipped, project_walker};
pub use watch::{FsChange, Watcher};
