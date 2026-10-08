//! Which files belong to the project: one set of rules for the tree, file search, and project
//! search.

use std::ffi::OsStr;
use std::path::Path;

use ignore::WalkBuilder;

/// Version control system metadata directories: they are absent from the tree and the walk; they
/// also mark the project root (`find_vcs_root` in flux-search).
pub const VCS_DIRS: [&str; 4] = [".git", ".hg", ".jj", ".svn"];

/// Junk that is wanted neither in the tree nor in search.
pub const JUNK_FILES: [&str; 1] = [".DS_Store"];

/// The temporary file of an atomic save, `.name.flux-tmp` (`flux_core::Document::save`): it lives
/// for milliseconds, until it is renamed over the real file.
const SAVE_TMP_SUFFIX: &str = ".flux-tmp";

/// A name that is in neither the tree nor the walk: a VCS metadata directory, junk, or a temporary
/// save file.
pub fn is_skipped(name: &OsStr) -> bool {
    VCS_DIRS.iter().chain(&JUNK_FILES).any(|skip| name == *skip)
        || name
            .to_str()
            .is_some_and(|name| name.starts_with('.') && name.ends_with(SAVE_TMP_SUFFIX))
}

/// A walk of the directory `start` inside the project `root` by the shared rules: `.gitignore`
/// (inside a git repository), `.ignore`, the global gitignore, and `.git/info/exclude`, including
/// those from directories above `start`; hidden files are visible, symlinks are not followed,
/// [`is_skipped`] entries are skipped. The whole project is `project_walker(root, root)`.
pub fn project_walker(root: &Path, start: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(start);
    builder
        .hidden(false)
        .follow_links(false)
        // The global gitignore is applied relative to the project root, not to the directory the
        // editor was launched from (which is `/` when launched from Finder).
        .current_dir(root)
        .filter_entry(|entry| !is_skipped(entry.file_name()));
    builder
}
