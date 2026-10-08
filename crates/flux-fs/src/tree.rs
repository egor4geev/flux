//! State of the project's file tree: which directories have been read and expanded, and the visible
//! rows. It never reads from disk itself: the caller brings the directory listings
//! ([`crate::list_dir`], in the background). [`FileTree::pending_loads`] says what is still to be
//! read, and [`FileTree::refresh_plan`] what to re-read after changes on disk.
//!
//! Directories are identified by absolute paths. A directory that has been read remembers its
//! listing even when collapsed: expanding it again needs no read, and disk watching keeps the
//! listing fresh. The expansion of nested directories is preserved while their parent is collapsed.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::list::{DirEntry, EntryKind};
use crate::ops::remap;
use crate::watch::FsChange;

/// Rule files: changing one changes the `ignored` flag of the whole directory.
const IGNORE_FILES: [&str; 2] = [".gitignore", ".ignore"];

/// A visible tree row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    /// Nesting depth: children of the root are 0.
    pub depth: usize,
    pub kind: EntryKind,
    pub ignored: bool,
    /// The directory is expanded; `false` for files.
    pub expanded: bool,
}

#[derive(Debug, Default)]
struct Dir {
    /// The listing that has been read; `None` means not read yet.
    entries: Option<Vec<DirEntry>>,
    expanded: bool,
}

#[derive(Debug)]
pub struct FileTree {
    root: PathBuf,
    /// Directories that something is known about: read or expanded ones. The root is always present
    /// and always expanded.
    dirs: HashMap<PathBuf, Dir>,
}

impl FileTree {
    pub fn new(root: PathBuf) -> Self {
        let mut dirs = HashMap::new();
        dirs.insert(
            root.clone(),
            Dir {
                entries: None,
                expanded: true,
            },
        );
        Self { root, dirs }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Visible rows in order: the children of expanded and read directories.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        self.push_rows(&self.root, 0, &mut rows);
        rows
    }

    fn push_rows(&self, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
        let Some(entries) = self.entries(dir) else {
            return;
        };
        for entry in entries {
            let path = dir.join(&entry.name);
            let expanded = entry.kind == EntryKind::Dir && self.is_expanded(&path);
            rows.push(Row {
                path: path.clone(),
                name: entry.name.clone(),
                depth,
                kind: entry.kind,
                ignored: entry.ignored,
                expanded,
            });
            if expanded {
                self.push_rows(&path, depth + 1, rows);
            }
        }
    }

    fn entries(&self, dir: &Path) -> Option<&Vec<DirEntry>> {
        self.dirs.get(dir)?.entries.as_ref()
    }

    /// The entry for `path` in its directory's listing; there is no entry for the root.
    pub fn entry(&self, path: &Path) -> Option<&DirEntry> {
        let name = path.file_name()?.to_str()?;
        self.entries(path.parent()?)?
            .iter()
            .find(|entry| entry.name == name)
    }

    /// Whether it is a directory: the root, or a directory in its parent's listing.
    pub fn is_dir(&self, path: &Path) -> bool {
        path == self.root || self.entry(path).is_some_and(|e| e.kind == EntryKind::Dir)
    }

    pub fn is_loaded(&self, dir: &Path) -> bool {
        self.entries(dir).is_some()
    }

    pub fn is_expanded(&self, dir: &Path) -> bool {
        self.dirs.get(dir).is_some_and(|dir| dir.expanded)
    }

    /// Whether `path` is excluded by the `.gitignore` rules, judging by the parent's listing; the
    /// root is not. For a directory this is the `dir_ignored` argument of [`crate::list_dir`] for
    /// its children.
    pub fn is_ignored(&self, path: &Path) -> bool {
        self.entry(path).is_some_and(|entry| entry.ignored)
    }

    /// Directories that are visible (all ancestors expanded and read) but not read themselves yet,
    /// with the `ignored` flag for `list_dir`. Parents come before children.
    pub fn pending_loads(&self) -> Vec<(PathBuf, bool)> {
        let mut pending = Vec::new();
        self.collect_pending(&self.root, false, &mut pending);
        pending
    }

    fn collect_pending(&self, dir: &Path, ignored: bool, pending: &mut Vec<(PathBuf, bool)>) {
        let Some(entries) = self.entries(dir) else {
            pending.push((dir.to_path_buf(), ignored));
            return;
        };
        for entry in entries.iter().filter(|entry| entry.kind == EntryKind::Dir) {
            let path = dir.join(&entry.name);
            if self.is_expanded(&path) {
                self.collect_pending(&path, entry.ignored, pending);
            }
        }
    }

    /// A directory listing has been read. The state of the remaining subdirectories is kept; that
    /// of vanished ones (and everything inside them) is forgotten. Returns the read subdirectories
    /// whose `ignored` flag changed: they need to be re-read, since the flags of everything inside
    /// change with it. A listing for a directory the tree has already forgotten is discarded.
    pub fn set_listing(&mut self, dir: &Path, entries: Vec<DirEntry>) -> Vec<PathBuf> {
        let Some(state) = self.dirs.get_mut(dir) else {
            return Vec::new();
        };
        let old = state.entries.replace(entries);
        let children: Vec<PathBuf> = self
            .dirs
            .keys()
            .filter(|path| path.parent() == Some(dir))
            .cloned()
            .collect();
        let mut stale = Vec::new();
        for child in children {
            let name = child.file_name().and_then(|name| name.to_str());
            let find = |entries: &[DirEntry]| {
                entries
                    .iter()
                    .find(|entry| Some(entry.name.as_str()) == name)
                    .cloned()
            };
            match self.entries(dir).and_then(|entries| find(entries)) {
                Some(entry) if entry.kind == EntryKind::Dir => {
                    let was = old.as_deref().and_then(find).map(|entry| entry.ignored);
                    if was.is_some_and(|was| was != entry.ignored) && self.is_loaded(&child) {
                        stale.push(child);
                    }
                }
                _ => self.forget(&child),
            }
        }
        stale
    }

    /// Forgets a directory and everything inside it.
    fn forget(&mut self, dir: &Path) {
        if dir != self.root {
            self.dirs.retain(|path, _| !path.starts_with(dir));
        }
    }

    /// Expands a directory; an unread one will appear in [`Self::pending_loads`].
    pub fn expand(&mut self, dir: &Path) {
        if dir.starts_with(&self.root) {
            self.dirs.entry(dir.to_path_buf()).or_default().expanded = true;
        }
    }

    pub fn collapse(&mut self, dir: &Path) {
        if dir == self.root {
            return;
        }
        if let Some(state) = self.dirs.get_mut(dir) {
            state.expanded = false;
            // Expand and collapse right away, without waiting for the read: there is nothing to
            // remember.
            if state.entries.is_none() {
                self.dirs.remove(dir);
            }
        }
    }

    pub fn toggle(&mut self, dir: &Path) {
        if self.is_expanded(dir) {
            self.collapse(dir);
        } else {
            self.expand(dir);
        }
    }

    /// Collapses all directories; the listings that have been read stay.
    pub fn collapse_all(&mut self) {
        let root = self.root.clone();
        self.dirs.retain(|path, dir| {
            if *path != root {
                dir.expanded = false;
            }
            dir.entries.is_some() || *path == root
        });
    }

    /// Expands the directories from the root down to `path` (not `path` itself): once they are
    /// read, the `path` row becomes visible. `false` means the path is outside the root.
    pub fn reveal(&mut self, path: &Path) -> bool {
        let Ok(rest) = path.strip_prefix(&self.root) else {
            return false;
        };
        let mut dir = self.root.clone();
        let mut components = rest.components().peekable();
        while let Some(component) = components.next() {
            if components.peek().is_none() {
                break;
            }
            dir.push(component);
            self.expand(&dir);
        }
        true
    }

    /// `from` was renamed or moved to `to`: the state of the directories inside moves along with
    /// it. In the same directory the entry is renamed in place (re-reading will fix the order); if
    /// it came from another directory, the entry is removed, and the new parent will add it when it
    /// is re-read.
    pub fn rename(&mut self, from: &Path, to: &Path) {
        if from == to || from == self.root {
            return;
        }
        let moved: Vec<PathBuf> = self
            .dirs
            .keys()
            .filter(|path| path.starts_with(from))
            .cloned()
            .collect();
        for old in moved {
            if let (Some(state), Some(new)) = (self.dirs.remove(&old), remap(&old, from, to)) {
                self.dirs.insert(new, state);
            }
        }
        let (Some(from_dir), Some(from_name)) =
            (from.parent(), from.file_name().and_then(|n| n.to_str()))
        else {
            return;
        };
        let to_name = to.file_name().and_then(|name| name.to_str());
        let same_dir = to.parent() == Some(from_dir);
        if let Some(entries) = self.dirs.get_mut(from_dir).and_then(|d| d.entries.as_mut())
            && let Some(index) = entries.iter().position(|entry| entry.name == from_name)
        {
            match to_name.filter(|_| same_dir) {
                Some(name) => entries[index].name = name.to_string(),
                None => {
                    entries.remove(index);
                }
            }
        }
    }

    /// `path` was deleted: removes it from the parent's listing and forgets the directories inside
    /// it.
    pub fn remove(&mut self, path: &Path) {
        if path == self.root {
            return;
        }
        self.forget(path);
        let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
        else {
            return;
        };
        if let Some(entries) = self.dirs.get_mut(dir).and_then(|d| d.entries.as_mut()) {
            entries.retain(|entry| entry.name != name);
        }
    }

    /// Directories that have been read, parents before children.
    pub fn loaded_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = self
            .dirs
            .iter()
            .filter(|(_, dir)| dir.entries.is_some())
            .map(|(path, _)| path.clone())
            .collect();
        sort_parents_first(&mut dirs);
        dirs
    }

    /// What to re-read after changes on disk. For each path: its read directory, plus the path
    /// itself if it is a read directory; if `.gitignore` or `.ignore` changed, also all read
    /// directories inside its directory. `Rescan` means all read ones. Paths outside the root are
    /// skipped. Parents come before children.
    pub fn refresh_plan(&self, changes: &[FsChange]) -> Vec<PathBuf> {
        let mut plan = BTreeSet::new();
        for change in changes {
            let paths = match change {
                FsChange::Rescan => return self.loaded_dirs(),
                FsChange::Paths(paths) => paths,
            };
            for path in paths.iter().filter(|path| path.starts_with(&self.root)) {
                if self.is_loaded(path) {
                    plan.insert(path.clone());
                }
                let Some(dir) = path.parent().filter(|dir| self.is_loaded(dir)) else {
                    continue;
                };
                plan.insert(dir.to_path_buf());
                let rules = path
                    .file_name()
                    .is_some_and(|name| IGNORE_FILES.iter().any(|rule| name == *rule));
                if rules {
                    plan.extend(
                        self.loaded_dirs()
                            .into_iter()
                            .filter(|loaded| loaded.starts_with(dir)),
                    );
                }
            }
        }
        let mut plan: Vec<PathBuf> = plan.into_iter().collect();
        sort_parents_first(&mut plan);
        plan
    }

    /// The directory for a new file or a paste: the selected directory, the directory of the
    /// selected file, or the root when nothing is selected.
    pub fn target_dir(&self, selected: Option<&Path>) -> PathBuf {
        match selected {
            Some(path) if self.is_dir(path) => path.to_path_buf(),
            Some(path) => path
                .parent()
                .filter(|dir| dir.starts_with(&self.root))
                .unwrap_or(&self.root)
                .to_path_buf(),
            None => self.root.clone(),
        }
    }
}

/// Shallower first: a parent before its child, and the order is total.
fn sort_parents_first(dirs: &mut [PathBuf]) {
    dirs.sort_by(|a, b| {
        let depth = |path: &PathBuf| path.components().count();
        depth(a).cmp(&depth(b)).then_with(|| a.cmp(b))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/p";

    fn path(rel: &str) -> PathBuf {
        if rel.is_empty() {
            PathBuf::from(ROOT)
        } else {
            Path::new(ROOT).join(rel)
        }
    }

    /// A directory listing: `name/` is a directory, a leading `!` means excluded by `.gitignore`.
    fn listing(names: &[&str]) -> Vec<DirEntry> {
        names
            .iter()
            .map(|name| {
                let (ignored, name) = match name.strip_prefix('!') {
                    Some(rest) => (true, rest),
                    None => (false, *name),
                };
                let (kind, name) = match name.strip_suffix('/') {
                    Some(dir) => (EntryKind::Dir, dir),
                    None => (EntryKind::File, name),
                };
                DirEntry {
                    name: name.into(),
                    kind,
                    ignored,
                }
            })
            .collect()
    }

    /// Rows as "indent + name", directories with `/`, expanded ones with `/-`.
    fn shape(tree: &FileTree) -> Vec<String> {
        tree.rows()
            .iter()
            .map(|row| {
                let mark = match (row.kind, row.expanded) {
                    (EntryKind::Dir, true) => "/-",
                    (EntryKind::Dir, false) => "/",
                    (EntryKind::File, _) => "",
                };
                format!("{}{}{mark}", "  ".repeat(row.depth), row.name)
            })
            .collect()
    }

    fn tree() -> FileTree {
        let mut tree = FileTree::new(path(""));
        tree.set_listing(&path(""), listing(&["src/", "!target/", "Cargo.toml"]));
        tree
    }

    #[test]
    fn root_is_pending_until_read() {
        let tree = FileTree::new(path(""));
        assert_eq!(tree.pending_loads(), [(path(""), false)]);
        assert!(tree.rows().is_empty());
        let tree = self::tree();
        assert!(tree.pending_loads().is_empty());
        assert_eq!(shape(&tree), ["src/", "target/", "Cargo.toml"]);
    }

    #[test]
    fn expanded_dirs_show_children_and_load_with_their_ignored_flag() {
        let mut tree = tree();
        tree.expand(&path("src"));
        tree.expand(&path("target"));
        assert_eq!(
            tree.pending_loads(),
            [(path("src"), false), (path("target"), true)]
        );
        // Expanded but not read: no children for now.
        assert_eq!(shape(&tree), ["src/-", "target/-", "Cargo.toml"]);
        tree.set_listing(&path("src"), listing(&["app/", "main.rs"]));
        assert_eq!(
            shape(&tree),
            ["src/-", "  app/", "  main.rs", "target/-", "Cargo.toml"]
        );
        let rows = tree.rows();
        assert_eq!(rows[2].path, path("src/main.rs"));
        assert_eq!(rows[2].depth, 1);
        assert!(rows[3].ignored);
    }

    #[test]
    fn collapsing_a_parent_keeps_nested_expansion() {
        let mut tree = tree();
        tree.expand(&path("src"));
        tree.set_listing(&path("src"), listing(&["app/", "main.rs"]));
        tree.expand(&path("src/app"));
        tree.set_listing(&path("src/app"), listing(&["lib.rs"]));
        tree.collapse(&path("src"));
        assert_eq!(shape(&tree), ["src/", "target/", "Cargo.toml"]);
        // A collapsed but read directory doesn't ask to be read.
        assert!(tree.pending_loads().is_empty());
        tree.toggle(&path("src"));
        assert_eq!(
            shape(&tree),
            [
                "src/-",
                "  app/-",
                "    lib.rs",
                "  main.rs",
                "target/",
                "Cargo.toml"
            ]
        );
    }

    #[test]
    fn collapse_all_keeps_listings() {
        let mut tree = tree();
        tree.expand(&path("src"));
        tree.set_listing(&path("src"), listing(&["main.rs"]));
        tree.expand(&path("target"));
        tree.collapse_all();
        assert_eq!(shape(&tree), ["src/", "target/", "Cargo.toml"]);
        assert!(tree.is_loaded(&path("src")));
        // Expanded but never read: forgotten.
        assert!(!tree.dirs.contains_key(&path("target")));
        assert!(tree.pending_loads().is_empty());
    }

    #[test]
    fn relisting_forgets_vanished_dirs_and_keeps_the_rest() {
        let mut tree = tree();
        tree.expand(&path("src"));
        tree.set_listing(&path("src"), listing(&["app/", "old/"]));
        tree.expand(&path("src/app"));
        tree.expand(&path("src/old"));
        tree.set_listing(&path("src/old"), listing(&["deep/"]));
        tree.expand(&path("src/old/deep"));
        tree.set_listing(&path("src"), listing(&["app/", "new.rs"]));
        assert!(tree.is_expanded(&path("src/app")));
        assert!(!tree.dirs.contains_key(&path("src/old")));
        assert!(!tree.dirs.contains_key(&path("src/old/deep")));
        // A directory that became a file with the same name is forgotten too.
        tree.set_listing(&path("src"), listing(&["app"]));
        assert!(!tree.dirs.contains_key(&path("src/app")));
        // The listing of a forgotten directory is discarded.
        assert!(
            tree.set_listing(&path("src/old"), listing(&["x"]))
                .is_empty()
        );
        assert!(!tree.is_loaded(&path("src/old")));
    }

    #[test]
    fn changed_ignored_flag_asks_to_reread_loaded_subdirs() {
        let mut tree = tree();
        tree.expand(&path("target"));
        tree.set_listing(&path("target"), listing(&["!debug/"]));
        tree.expand(&path("src"));
        // `target` is no longer excluded (the .gitignore edit was reverted).
        let stale = tree.set_listing(&path(""), listing(&["src/", "target/", "Cargo.toml"]));
        assert_eq!(stale, [path("target")]);
        assert!(!tree.is_ignored(&path("target")));
        // `src` has not been read, so there is nothing to re-read; the flag didn't change, all the
        // more so.
        let stale = tree.set_listing(&path(""), listing(&["src/", "target/", "Cargo.toml"]));
        assert!(stale.is_empty());
    }

    #[test]
    fn reveal_expands_ancestors_one_level_at_a_time() {
        let mut tree = tree();
        assert!(tree.reveal(&path("src/app/deep/lib.rs")));
        // Only `src` can be read: nothing is known about `src/app` until `src` has been read.
        assert_eq!(tree.pending_loads(), [(path("src"), false)]);
        tree.set_listing(&path("src"), listing(&["app/"]));
        assert_eq!(tree.pending_loads(), [(path("src/app"), false)]);
        tree.set_listing(&path("src/app"), listing(&["!deep/"]));
        assert_eq!(tree.pending_loads(), [(path("src/app/deep"), true)]);
        tree.set_listing(&path("src/app/deep"), listing(&["!lib.rs"]));
        assert!(
            tree.rows()
                .iter()
                .any(|row| row.path == path("src/app/deep/lib.rs"))
        );
        // A file itself isn't "expanded"; outside the root, `false`.
        assert!(!tree.dirs.contains_key(&path("src/app/deep/lib.rs")));
        assert!(!tree.reveal(Path::new("/elsewhere/x.rs")));
        assert!(tree.reveal(&path("")));
    }

    #[test]
    fn reveal_through_a_missing_dir_stops_quietly() {
        let mut tree = tree();
        tree.reveal(&path("gone/file.rs"));
        // `gone` is not in the root's listing: there is nothing to read, and its state is forgotten
        // on re-reading.
        assert!(tree.pending_loads().is_empty());
        tree.set_listing(&path(""), listing(&["src/"]));
        assert!(!tree.dirs.contains_key(&path("gone")));
    }

    #[test]
    fn rename_in_place_keeps_position_and_expansion() {
        let mut tree = tree();
        tree.expand(&path("src"));
        tree.set_listing(&path("src"), listing(&["app/", "main.rs"]));
        tree.expand(&path("src/app"));
        tree.set_listing(&path("src/app"), listing(&["lib.rs"]));
        tree.rename(&path("src"), &path("source"));
        assert_eq!(
            shape(&tree),
            [
                "source/-",
                "  app/-",
                "    lib.rs",
                "  main.rs",
                "target/",
                "Cargo.toml"
            ]
        );
        assert!(tree.is_loaded(&path("source/app")));
        assert!(!tree.dirs.contains_key(&path("src")));
        tree.rename(&path("source/main.rs"), &path("source/lib.rs"));
        assert_eq!(tree.rows()[3].path, path("source/lib.rs"));
    }

    #[test]
    fn move_to_another_dir_takes_the_subtree_along() {
        let mut tree = tree();
        tree.expand(&path("src"));
        tree.set_listing(&path("src"), listing(&["app/"]));
        tree.expand(&path("src/app"));
        tree.set_listing(&path("src/app"), listing(&["lib.rs"]));
        tree.rename(&path("src/app"), &path("target/app"));
        // The entry left `src`; re-reading will add it to `target`.
        assert_eq!(shape(&tree), ["src/-", "target/", "Cargo.toml"]);
        assert!(tree.is_expanded(&path("target/app")));
        assert!(tree.is_loaded(&path("target/app")));
        tree.expand(&path("target"));
        tree.set_listing(&path("target"), listing(&["!app/"]));
        assert_eq!(
            shape(&tree),
            ["src/-", "target/-", "  app/-", "    lib.rs", "Cargo.toml"]
        );
    }

    #[test]
    fn remove_drops_the_row_and_the_subtree() {
        let mut tree = tree();
        tree.expand(&path("src"));
        tree.set_listing(&path("src"), listing(&["app/"]));
        tree.expand(&path("src/app"));
        tree.remove(&path("src"));
        assert_eq!(shape(&tree), ["target/", "Cargo.toml"]);
        assert!(!tree.dirs.contains_key(&path("src/app")));
        tree.remove(&path(""));
        assert!(tree.is_loaded(&path("")));
    }

    #[test]
    fn refresh_plan_covers_loaded_parents_and_dirs() {
        let mut tree = tree();
        tree.expand(&path("src"));
        tree.set_listing(&path("src"), listing(&["app/", "main.rs"]));
        tree.expand(&path("src/app"));
        tree.set_listing(&path("src/app"), listing(&["lib.rs"]));
        let paths = |paths: &[&str]| vec![FsChange::Paths(paths.iter().map(|p| path(p)).collect())];
        assert_eq!(tree.refresh_plan(&paths(&["src/main.rs"])), [path("src")]);
        // The read directory itself and its parent; the parent comes first.
        assert_eq!(
            tree.refresh_plan(&paths(&["src/app"])),
            [path("src"), path("src/app")]
        );
        // An unread directory and paths outside the root: nothing to re-read.
        assert!(tree.refresh_plan(&paths(&["target/debug/x"])).is_empty());
        assert!(
            tree.refresh_plan(&[FsChange::Paths(vec![PathBuf::from("/q/x")])])
                .is_empty()
        );
        // `.gitignore` rules: the directory and everything read inside it.
        assert_eq!(
            tree.refresh_plan(&paths(&[".gitignore"])),
            [path(""), path("src"), path("src/app")]
        );
        assert_eq!(
            tree.refresh_plan(&paths(&["src/.ignore", "src/app/lib.rs"])),
            [path("src"), path("src/app")]
        );
        assert_eq!(
            tree.refresh_plan(&[FsChange::Paths(vec![]), FsChange::Rescan]),
            [path(""), path("src"), path("src/app")]
        );
    }

    #[test]
    fn target_dir_is_the_selected_dir_or_the_file_dir() {
        let mut tree = tree();
        tree.expand(&path("src"));
        tree.set_listing(&path("src"), listing(&["main.rs"]));
        assert_eq!(tree.target_dir(Some(&path("src"))), path("src"));
        assert_eq!(tree.target_dir(Some(&path("src/main.rs"))), path("src"));
        assert_eq!(tree.target_dir(Some(&path("Cargo.toml"))), path(""));
        assert_eq!(tree.target_dir(None), path(""));
    }
}
