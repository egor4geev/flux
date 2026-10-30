//! The files a server watches (part 10.5): it registers globs for `workspace/didChangeWatchedFiles`
//! (`client/registerCapability`), the window reports what changed on disk, and only the changes
//! that match go to the server. rust-analyzer registers relative patterns from the project root
//! (`**/*.rs`, `**/Cargo.{lock,toml}`) and absolute paths; gopls — `**/*.{go,mod,sum,work}`.

use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobMatcher};
use lsp_types::{
    DidChangeWatchedFilesRegistrationOptions, FileChangeType, GlobPattern, OneOf, WatchKind,
};
use serde_json::Value;

use crate::position::path_from_uri;

/// The method whose registrations are kept here.
pub(crate) const METHOD: &str = "workspace/didChangeWatchedFiles";

/// One watcher of a registration.
#[derive(Debug)]
struct Watcher {
    glob: GlobMatcher,
    /// A relative pattern's base: the glob is matched against the path under it.
    base: Option<PathBuf>,
    /// A string pattern that isn't absolute (`**/*.go`): matched against the path under the
    /// workspace root as well as the absolute path.
    relative: bool,
    kind: WatchKind,
}

impl Watcher {
    fn new(watcher: lsp_types::FileSystemWatcher) -> Option<Self> {
        let kind = watcher.kind.unwrap_or(WatchKind::all());
        let (pattern, base) = match watcher.glob_pattern {
            GlobPattern::String(pattern) => (pattern, None),
            GlobPattern::Relative(relative) => {
                let uri = match &relative.base_uri {
                    OneOf::Left(folder) => &folder.uri,
                    OneOf::Right(uri) => uri,
                };
                (relative.pattern, Some(path_from_uri(uri)?))
            }
        };
        let relative = base.is_none() && !pattern.starts_with('/');
        let glob = GlobBuilder::new(&pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .empty_alternates(true)
            .build()
            .ok()?
            .compile_matcher();
        Some(Self {
            glob,
            base,
            relative,
            kind,
        })
    }

    fn matches(&self, path: &Path, change: FileChangeType, root: &Path) -> bool {
        let wanted = match change {
            FileChangeType::CREATED => WatchKind::Create,
            FileChangeType::DELETED => WatchKind::Delete,
            _ => WatchKind::Change,
        };
        if !self.kind.contains(wanted) {
            return false;
        }
        match &self.base {
            Some(base) => path
                .strip_prefix(base)
                .is_ok_and(|under| self.glob.is_match(under)),
            None => {
                self.glob.is_match(path)
                    || (self.relative
                        && path
                            .strip_prefix(root)
                            .is_ok_and(|under| self.glob.is_match(under)))
            }
        }
    }
}

/// A server's registrations of watched files, by id.
#[derive(Debug, Default)]
pub(crate) struct Watchers {
    registrations: Vec<(String, Vec<Watcher>)>,
}

impl Watchers {
    /// A registration's `registerOptions`; one with the id of an earlier registration replaces it
    /// (rust-analyzer registers again under the same id after it reloads the project). Watchers
    /// whose glob can't be read are skipped. `false` — the options aren't watchers at all.
    pub(crate) fn register(&mut self, id: &str, options: Value) -> bool {
        let Ok(options) =
            serde_json::from_value::<DidChangeWatchedFilesRegistrationOptions>(options)
        else {
            return false;
        };
        let watchers = options
            .watchers
            .into_iter()
            .filter_map(Watcher::new)
            .collect();
        self.unregister(id);
        self.registrations.push((id.to_string(), watchers));
        true
    }

    /// `true` if there was such a registration.
    pub(crate) fn unregister(&mut self, id: &str) -> bool {
        let before = self.registrations.len();
        self.registrations.retain(|(known, _)| known != id);
        self.registrations.len() != before
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.registrations
            .iter()
            .all(|(_, watchers)| watchers.is_empty())
    }

    /// Whether a change of `path` interests the server; `root` — its workspace root.
    pub(crate) fn matches(&self, path: &Path, change: FileChangeType, root: &Path) -> bool {
        self.registrations
            .iter()
            .flat_map(|(_, watchers)| watchers)
            .any(|watcher| watcher.matches(path, change, root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ROOT: &str = "/work/proj";

    fn watchers(options: Value) -> Watchers {
        let mut watchers = Watchers::default();
        assert!(watchers.register("w", options));
        watchers
    }

    fn matches(watchers: &Watchers, path: &str, change: FileChangeType) -> bool {
        watchers.matches(Path::new(path), change, Path::new(ROOT))
    }

    /// A full path as a server writes it in a glob or a URI: `/work/proj`, on Windows
    /// `C:/work/proj`.
    fn slashed(path: &str) -> String {
        if cfg!(windows) {
            format!("C:{path}")
        } else {
            path.to_string()
        }
    }

    #[test]
    fn rust_analyzer_relative_patterns_and_absolute_paths() {
        // As rust-analyzer 1.99 registers them for a loaded project.
        let base = format!("file://{}", slashed(ROOT));
        let watchers = watchers(json!({ "watchers": [
            { "globPattern": { "baseUri": base, "pattern": "**/*.rs" } },
            { "globPattern": { "baseUri": base, "pattern": "**/Cargo.{lock,toml}" } },
            { "globPattern": slashed("/Users/me/Library/Application Support/rust-analyzer") },
            { "globPattern": slashed("/work/proj/Cargo.toml") },
        ]}));
        // The paths as the system writes them: on Windows `C:\work\proj\…`.
        let matches = |watchers: &Watchers, path: &str, change| {
            let style = flux_platform::PathStyle::NATIVE;
            let (path, root) = (slashed(path), slashed(ROOT));
            watchers.matches(
                Path::new(style.native(&path).as_ref()),
                change,
                Path::new(style.native(&root).as_ref()),
            )
        };
        let changed = FileChangeType::CHANGED;
        assert!(matches(&watchers, "/work/proj/src/main.rs", changed));
        assert!(matches(&watchers, "/work/proj/main.rs", changed));
        assert!(matches(
            &watchers,
            "/work/proj/crates/a/Cargo.toml",
            changed
        ));
        assert!(matches(
            &watchers,
            "/work/proj/Cargo.lock",
            FileChangeType::CREATED
        ));
        assert!(matches(
            &watchers,
            "/work/proj/Cargo.toml",
            FileChangeType::DELETED
        ));
        assert!(matches(
            &watchers,
            "/Users/me/Library/Application Support/rust-analyzer",
            changed
        ));
        assert!(!matches(&watchers, "/work/proj/README.md", changed));
        assert!(!matches(&watchers, "/work/proj/src/main.rs.orig", changed));
        // Outside the base.
        assert!(!matches(&watchers, "/work/other/src/main.rs", changed));
        assert!(!watchers.is_empty());
    }

    #[test]
    fn plain_globs_match_absolute_and_root_relative_paths() {
        let watchers = watchers(json!({ "watchers": [
            { "globPattern": "**/*.{go,mod,sum,work}" },
            { "globPattern": "*.toml" },
        ]}));
        let changed = FileChangeType::CHANGED;
        assert!(matches(&watchers, "/work/proj/cmd/main.go", changed));
        assert!(matches(&watchers, "/work/proj/go.mod", changed));
        // Relative to the root: only at its top.
        assert!(matches(&watchers, "/work/proj/ruff.toml", changed));
        assert!(!matches(&watchers, "/work/proj/sub/ruff.toml", changed));
        // `*` stays within a segment.
        assert!(!matches(&watchers, "/work/proj/a/b.txt", changed));
    }

    #[test]
    fn the_kind_of_change_is_filtered() {
        let watchers = watchers(json!({ "watchers": [
            { "globPattern": "**/*.py", "kind": 1 | 4 },
        ]}));
        assert!(matches(
            &watchers,
            "/work/proj/a.py",
            FileChangeType::CREATED
        ));
        assert!(matches(
            &watchers,
            "/work/proj/a.py",
            FileChangeType::DELETED
        ));
        assert!(!matches(
            &watchers,
            "/work/proj/a.py",
            FileChangeType::CHANGED
        ));
    }

    #[test]
    fn a_registration_replaces_one_with_its_id_and_goes_away() {
        let mut watchers = watchers(json!({ "watchers": [{ "globPattern": "**/*.rs" }] }));
        assert!(watchers.register("w", json!({ "watchers": [{ "globPattern": "**/*.go" }] })));
        assert!(!matches(
            &watchers,
            "/work/proj/a.rs",
            FileChangeType::CHANGED
        ));
        assert!(matches(
            &watchers,
            "/work/proj/a.go",
            FileChangeType::CHANGED
        ));
        assert!(watchers.register(
            "other",
            json!({ "watchers": [{ "globPattern": "**/*.rs" }] })
        ));
        assert!(matches(
            &watchers,
            "/work/proj/a.rs",
            FileChangeType::CHANGED
        ));
        assert!(watchers.unregister("w"));
        assert!(!watchers.unregister("w"));
        assert!(!matches(
            &watchers,
            "/work/proj/a.go",
            FileChangeType::CHANGED
        ));
        assert!(watchers.unregister("other"));
        assert!(watchers.is_empty());
    }

    #[test]
    fn broken_globs_and_options_are_skipped() {
        let mut watchers = Watchers::default();
        assert!(!watchers.register("bad", json!({ "nothing": true })));
        assert!(watchers.register(
            "w",
            json!({ "watchers": [{ "globPattern": "[" }, { "globPattern": "**/*.rs" }] })
        ));
        assert!(matches(
            &watchers,
            "/work/proj/x.rs",
            FileChangeType::CHANGED
        ));
    }
}
