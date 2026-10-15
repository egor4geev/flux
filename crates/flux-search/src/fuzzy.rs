//! Fuzzy search through nucleo:
//! - small lists (the command palette: hundreds of rows) are matched synchronously, with
//!   [`match_list`];
//! - large lists of paths (file search: tens of thousands) use [`PathMatcher`]: nucleo matches on
//!   its own threads, and the list is filled right during the walk.
//!
//! - file names in Find in Files (part 10.4): [`match_file_names`] — only close matches of a query
//!   that can be a name, so a name typed among text queries gives its file.
//!
//! The query syntax is like fzf's: words separated by spaces are matched independently, `'word` is
//! an exact match, `^start`, `end$`, `!exclude`. Case is "smart": an uppercase letter in a word
//! makes that word case-sensitive.

use std::path::Path;
use std::sync::Arc;

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Injector, Matcher, Nucleo, Utf32Str};
use unicode_segmentation::UnicodeSegmentation;

/// A list item's match against the query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    /// The index of the item in the input list.
    pub index: usize,
    /// The higher, the better the match.
    pub score: u32,
    /// Matched characters: `char` indices into the item's string, ascending, without duplicates.
    pub positions: Vec<usize>,
}

/// Matches the query against a list of strings. The result is sorted by descending score, with ties
/// kept in the original order. An empty query gives all items in order, without positions.
pub fn match_list<S: AsRef<str>>(query: &str, items: &[S]) -> Vec<FuzzyMatch> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    if pattern.atoms.is_empty() {
        return (0..items.len())
            .map(|index| FuzzyMatch {
                index,
                score: 0,
                positions: Vec::new(),
            })
            .collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut indices = Vec::new();
    let mut matches: Vec<FuzzyMatch> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let item = item.as_ref();
            indices.clear();
            let score =
                pattern.indices(Utf32Str::new(item, &mut buf), &mut matcher, &mut indices)?;
            Some(FuzzyMatch {
                index,
                score,
                positions: char_positions(item, &mut indices, Haystack::Str),
            })
        })
        .collect();
    matches.sort_by(|a, b| b.score.cmp(&a.score).then(a.index.cmp(&b.index)));
    matches
}

/// The shortest query [`match_file_names`] looks for names with: one or two letters are in half of
/// the project's names.
pub const MIN_NAME_QUERY: usize = 3;

/// A file found by its name ([`match_file_names`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameMatch {
    /// The index of the path in the input list.
    pub index: usize,
    /// The higher, the better the match.
    pub score: u32,
    /// Matched characters: `char` indices into the whole path, ascending, without duplicates.
    pub positions: Vec<usize>,
}

/// The query can be a file's name or path: [`MIN_NAME_QUERY`] characters or more, letters, digits
/// and `. _ - / + @ ~ #` only. Spaces and the characters of code (`(`, `=`, `;`, quotes) mean text;
/// so do the special characters of the fuzzy syntax (`!`, `^`, `$`, `'`).
pub fn is_name_query(query: &str) -> bool {
    query.chars().count() >= MIN_NAME_QUERY
        && query.chars().all(|c| {
            c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '+' | '@' | '~' | '#')
        })
}

/// The files whose names look like `query` (relative paths with `/`), as Find in Files lists them
/// above the text it found: a name typed there — even with a typo («radme.md») — gives the file.
///
/// Only a name query counts ([`is_name_query`]). It is matched as ⌘P matches (nucleo, the score of
/// paths, smart case), but against the file's name — against the whole path only when the query
/// has a `/`, so `test` doesn't bring every file under `tests/`. And only close matches stay: their
/// score per character of the query is that of consecutive characters and word starts, not of
/// letters scattered over a long name (the shorter the query, the closer: [`min_name_score`]). The
/// result is sorted by score, then shallower, shorter and alphabetically first paths first; the
/// first `limit` of them, with the number of all the matches.
pub fn match_file_names<S: AsRef<str>>(
    query: &str,
    paths: &[S],
    limit: usize,
) -> (Vec<NameMatch>, usize) {
    if !is_name_query(query) {
        return (Vec::new(), 0);
    }
    let whole_path = query.contains('/');
    let haystack = |path: &str| -> usize {
        // The byte where the matched part starts: the name, or the whole path.
        if whole_path {
            0
        } else {
            path.rfind('/').map_or(0, |slash| slash + 1)
        }
    };
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let min = min_name_score(query.chars().count());
    let mut buf = Vec::new();
    let mut found: Vec<(usize, u32)> = paths
        .iter()
        .enumerate()
        .filter_map(|(index, path)| {
            let path = path.as_ref();
            let part = &path[haystack(path)..];
            let score = pattern.score(Utf32Str::new(part, &mut buf), &mut matcher)?;
            (score >= min).then_some((index, score))
        })
        .collect();
    let total = found.len();
    let path = |index: usize| paths[index].as_ref();
    found.sort_by(|&(a, score_a), &(b, score_b)| {
        let (a, b) = (path(a), path(b));
        score_b
            .cmp(&score_a)
            .then_with(|| a.matches('/').count().cmp(&b.matches('/').count()))
            .then_with(|| a.len().cmp(&b.len()))
            .then_with(|| a.cmp(b))
    });
    found.truncate(limit);
    let mut indices = Vec::new();
    let matches = found
        .into_iter()
        .map(|(index, score)| {
            let path = path(index);
            let start = haystack(path);
            let part = &path[start..];
            indices.clear();
            pattern.indices(Utf32Str::new(part, &mut buf), &mut matcher, &mut indices);
            let offset = path[..start].chars().count();
            let positions = char_positions(part, &mut indices, Haystack::Str)
                .into_iter()
                .map(|position| position + offset)
                .collect();
            NameMatch {
                index,
                score,
                positions,
            }
        })
        .collect();
    (matches, total)
}

/// The least score of a close match for a query of `chars` characters: per character, a match
/// (16) plus the bonuses of consecutive characters and word starts minus the gaps. Calibrated on
/// the Flux repository: «radme.md» → `README.md` scores 22.4 per character, «prjsrch» →
/// `project_search.rs` 20.3, while scattered letters stay under 20 («editor.rs» →
/// `claude_history.rs`: 16.6; «let» → `file_tree.rs`: 19); a short query has to be closer still
/// («let» → `arrow-left.svg`: 23).
pub fn min_name_score(chars: usize) -> u32 {
    let per_char = match chars {
        0..=3 => 24,
        4 => 22,
        _ => 20,
    };
    per_char * chars as u32
}

/// A path's match against the query of a [`PathMatcher`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathMatch {
    /// The path relative to the project root, with `/` as the separator.
    pub path: Arc<str>,
    /// Matched characters: `char` indices into `path`, ascending, without duplicates.
    pub positions: Vec<usize>,
}

/// Fuzzy search over a large list of paths (file search). Paths are added through a
/// [`PathInjector`] from any thread, matching runs on nucleo's threads; results are collected by
/// [`tick`](Self::tick), which should be called after every `notify` and after the query changes.
/// The score takes `/` boundaries into account (as for paths in fzf).
pub struct PathMatcher {
    nucleo: Nucleo<Arc<str>>,
    /// For the positions of matched characters in the shown rows.
    matcher: Matcher,
    query: String,
    running: bool,
}

/// Feeds a [`PathMatcher`]. Cheap to clone and to send to other threads; while at least one clone
/// is alive, [`PathMatcher::is_running`] considers the list to be still growing.
#[derive(Clone)]
pub struct PathInjector {
    injector: Injector<Arc<str>>,
}

impl PathInjector {
    /// Adds a path relative to the project root.
    pub fn push(&self, relative: &Path) {
        let path = path_string(relative);
        self.injector.push(path, |path, columns| {
            columns[0] = path.as_ref().into();
        });
    }
}

impl PathMatcher {
    /// `notify` is called from nucleo's worker threads when it is time to call
    /// [`tick`](Self::tick): new paths have appeared or matches have been computed. By itself it is
    /// not rate-limited, so redraws should be throttled.
    pub fn new(notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            nucleo: Nucleo::new(Config::DEFAULT.match_paths(), notify, None, 1),
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            query: String::new(),
            running: false,
        }
    }

    pub fn injector(&self) -> PathInjector {
        PathInjector {
            injector: self.nucleo.injector(),
        }
    }

    /// Sets a new query; matching starts on the nearest [`tick`](Self::tick). If the query was only
    /// appended to, nucleo goes through just the previous matches.
    pub fn set_query(&mut self, query: &str) {
        if query == self.query {
            return;
        }
        // An appended query does not widen the result set, except when the old one ended with an
        // escape (`\`) or an end anchor (`$`): then the meaning of the last word changes. nucleo
        // checks negation (`!word`) itself.
        let append = query.starts_with(&self.query)
            && !self.query.ends_with('\\')
            && !self.query.ends_with('$');
        self.nucleo
            .pattern
            .reparse(0, query, CaseMatching::Smart, Normalization::Smart, append);
        self.query = query.to_string();
    }

    /// Collects the ready results without waiting for the threads. `true` means the list of matches
    /// changed.
    pub fn tick(&mut self) -> bool {
        let status = self.nucleo.tick(0);
        self.running = status.running;
        status.changed
    }

    /// Not everything is ready yet (as of the last [`tick`](Self::tick)): matching is in progress
    /// or someone holds a [`PathInjector`], so the walk continues.
    pub fn is_running(&self) -> bool {
        self.running || self.nucleo.active_injectors() > 0
    }

    /// The total number of paths (as of the last [`tick`](Self::tick)).
    pub fn item_count(&self) -> usize {
        self.nucleo.snapshot().item_count() as usize
    }

    /// The number of paths that match the query (as of the last [`tick`](Self::tick)).
    pub fn match_count(&self) -> usize {
        self.nucleo.snapshot().matched_item_count() as usize
    }

    /// The `n`-th match in descending score order (for an empty query, in insertion order) with the
    /// positions of the matched characters. The positions are computed here, so only the visible
    /// rows should be requested.
    pub fn get(&mut self, n: usize) -> Option<PathMatch> {
        let snapshot = self.nucleo.snapshot();
        let item = snapshot.get_matched_item(u32::try_from(n).ok()?)?;
        let mut indices = Vec::new();
        snapshot.pattern().column_pattern(0).indices(
            item.matcher_columns[0].slice(..),
            &mut self.matcher,
            &mut indices,
        );
        let path = item.data.clone();
        let positions = char_positions(&path, &mut indices, Haystack::String);
        Some(PathMatch { path, positions })
    }
}

/// The path as a string with `/` between components, used for both display and matching.
fn path_string(path: &Path) -> Arc<str> {
    let path = path.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '/' {
        path.into()
    } else {
        path.replace(std::path::MAIN_SEPARATOR, "/").into()
    }
}

/// How nucleo represented the string it computed the positions over.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Haystack {
    /// `Utf32Str::new`: ASCII is byte by byte; otherwise by grapheme (the first character of the
    /// grapheme), but if the first characters of all graphemes are ASCII, byte by byte again.
    Str,
    /// `Utf32String::from`: ASCII is byte by byte, otherwise always by grapheme.
    String,
}

/// nucleo indices → `char` indices into `text`, ascending, without duplicates.
fn char_positions(text: &str, indices: &mut Vec<u32>, haystack: Haystack) -> Vec<usize> {
    indices.sort_unstable();
    indices.dedup();
    if text.is_ascii() {
        return indices.iter().map(|&i| i as usize).collect();
    }
    let graphemes: Vec<(usize, &str)> = text.grapheme_indices(true).collect();
    let by_bytes = haystack == Haystack::Str
        && graphemes
            .iter()
            .all(|(_, g)| g.chars().next().is_some_and(|c| c.is_ascii()));
    let char_at_byte = |byte: usize| text[..byte.min(text.len())].chars().count();
    indices
        .iter()
        .filter_map(|&i| {
            let i = i as usize;
            if by_bytes {
                text.is_char_boundary(i).then(|| char_at_byte(i))
            } else {
                graphemes.get(i).map(|&(byte, _)| char_at_byte(byte))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    fn labels(query: &str, items: &[&str]) -> Vec<String> {
        match_list(query, items)
            .into_iter()
            .map(|m| items[m.index].to_string())
            .collect()
    }

    #[test]
    fn empty_query_keeps_everything_in_order() {
        let items = ["b", "a", "c"];
        let matches = match_list("  ", &items);
        assert_eq!(
            matches.iter().map(|m| m.index).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert!(matches.iter().all(|m| m.positions.is_empty()));
    }

    #[test]
    fn filters_and_ranks_by_score() {
        let items = [
            "editor: move word left",
            "workspace: close tab",
            "editor: move left",
        ];
        assert_eq!(
            labels("mvleft", &items),
            ["editor: move left", "editor: move word left"]
        );
        assert_eq!(labels("close", &items), ["workspace: close tab"]);
        assert!(labels("zzz", &items).is_empty());
    }

    #[test]
    fn words_match_independently_and_smart_case() {
        let items = ["Save File", "save all", "open file"];
        assert_eq!(labels("file save", &items), ["Save File"]);
        assert_eq!(labels("Save", &items), ["Save File"]);
        assert_eq!(labels("save", &items).len(), 2);
    }

    #[test]
    fn positions_are_char_indices() {
        let m = &match_list("ml", &["move left"])[0];
        assert_eq!(m.positions, [0, 5]);
        // Cyrillic: nucleo counts graphemes, we count characters.
        let m = &match_list("пф", &["путь к файлу"])[0];
        assert_eq!(m.positions, [0, 7]);
        // A multi-character emoji before the match.
        let m = &match_list("ok", &["👍🏽 ok"])[0];
        assert_eq!(m.positions, [3, 4]);
        // A combining accent: the first characters of the graphemes are ASCII, so nucleo counts
        // bytes.
        let m = &match_list("ex", &["e\u{301}x"])[0];
        assert_eq!(m.positions, [0, 2]);
    }

    fn names(query: &str, paths: &[&str]) -> Vec<String> {
        match_file_names(query, paths, usize::MAX)
            .0
            .into_iter()
            .map(|m| paths[m.index].to_string())
            .collect()
    }

    #[test]
    fn only_queries_that_can_be_names_look_for_files() {
        for query in [
            "radme.md",
            "Cargo.toml",
            "src/ui",
            "flux-app",
            "c++",
            "@scope",
            "план.md",
        ] {
            assert!(is_name_query(query), "{query}");
        }
        let texts = [
            "fn render",
            "Ok(",
            "a = b",
            "x;",
            "'exact",
            "!skip",
            "^start",
            "end$",
            "ab",
        ];
        for query in texts {
            assert!(!is_name_query(query), "{query}");
        }
    }

    #[test]
    fn a_name_with_a_typo_finds_its_file_first_in_the_root() {
        let paths = [
            "crates/flux-syntax/tests/fixtures/README.md",
            "docs/readme-old.txt",
            "README.md",
            "vendor/gpui/README.md",
            "crates/flux-app/src/editor.rs",
        ];
        let (found, total) = match_file_names("radme.md", &paths, 2);
        assert_eq!(total, 3);
        assert_eq!(
            found.iter().map(|m| paths[m.index]).collect::<Vec<_>>(),
            ["README.md", "vendor/gpui/README.md"]
        );
        // README.md: R, then A D M E . m d.
        assert_eq!(found[0].positions, [0, 2, 3, 4, 5, 6, 7, 8]);
        // The positions are in the whole path: the name starts after `vendor/gpui/`.
        assert_eq!(found[1].positions[0], "vendor/gpui/".len());
    }

    #[test]
    fn names_not_directories_unless_the_query_has_a_slash() {
        let paths = [
            "crates/flux-term/tests/pty.rs",
            "crates/flux-term/tests/bench.rs",
            "vendor/gpui/src/test.rs",
            "crates/flux-app/src/ui.rs",
        ];
        assert_eq!(names("test", &paths), ["vendor/gpui/src/test.rs"]);
        assert_eq!(
            names("tests/pty", &paths),
            ["crates/flux-term/tests/pty.rs"]
        );
        let (found, _) = match_file_names("src/ui", &paths, 1);
        assert_eq!(paths[found[0].index], "crates/flux-app/src/ui.rs");
        // A path query's positions start in the directories.
        assert_eq!(found[0].positions[0], "crates/flux-app/".len());
    }

    #[test]
    fn scattered_letters_are_not_a_name() {
        let paths = [
            "crates/flux-app/src/claude_history.rs",
            "crates/flux-app/src/editor.rs",
            "crates/flux-app/src/file_tree.rs",
            "crates/flux-app/assets/icons/arrow-left.svg",
            "crates/flux-app/src/project_search.rs",
        ];
        assert_eq!(
            names("editor.rs", &paths),
            ["crates/flux-app/src/editor.rs"]
        );
        assert!(names("let", &paths).is_empty());
        assert_eq!(
            names("prjsrch", &paths),
            ["crates/flux-app/src/project_search.rs"]
        );
        // Smart case, as in ⌘P: an uppercase letter wants that case.
        assert!(names("Editor", &paths).is_empty());
        // Cyrillic names: the positions are characters.
        let paths = ["заметки/план.md"];
        let (found, _) = match_file_names("план", &paths, 1);
        assert_eq!(found[0].positions, [8, 9, 10, 11]);
    }

    /// Waits for matching and feeding to finish.
    fn settle(matcher: &mut PathMatcher) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            matcher.tick();
            if !matcher.is_running() {
                return;
            }
            assert!(Instant::now() < deadline, "PathMatcher never settled");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn paths(matcher: &mut PathMatcher) -> Vec<String> {
        (0..matcher.match_count())
            .map(|n| matcher.get(n).unwrap().path.to_string())
            .collect()
    }

    #[test]
    fn path_matcher_fills_from_threads_and_settles() {
        let notified = Arc::new(AtomicUsize::new(0));
        let counter = notified.clone();
        let mut matcher = PathMatcher::new(Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }));
        let injector = matcher.injector();
        let workers: Vec<_> = (0..4)
            .map(|t| {
                let injector = injector.clone();
                thread::spawn(move || {
                    for i in 0..2_500 {
                        injector.push(Path::new(&format!("dir{t}/file{i}.rs")));
                    }
                })
            })
            .collect();
        drop(injector);
        assert!(matcher.is_running(), "injectors are still alive");
        for worker in workers {
            worker.join().unwrap();
        }
        settle(&mut matcher);
        assert_eq!(matcher.item_count(), 10_000);
        assert_eq!(matcher.match_count(), 10_000, "empty query matches all");
        assert!(notified.load(Ordering::Relaxed) > 0);

        matcher.set_query("dir3/file2499");
        settle(&mut matcher);
        assert_eq!(matcher.get(0).unwrap().path.as_ref(), "dir3/file2499.rs");
        assert!(matcher.get(matcher.match_count()).is_none());
    }

    #[test]
    fn path_matcher_ranks_and_reports_char_positions() {
        let mut matcher = PathMatcher::new(Arc::new(|| {}));
        let injector = matcher.injector();
        for path in [
            "crates/flux-app/src/editor.rs",
            "crates/flux-app/src/element.rs",
            "docs/заметки/редактор.md",
            "README.md",
        ] {
            injector.push(Path::new(path));
        }
        drop(injector);
        settle(&mut matcher);
        assert_eq!(
            paths(&mut matcher),
            [
                "crates/flux-app/src/editor.rs",
                "crates/flux-app/src/element.rs",
                "docs/заметки/редактор.md",
                "README.md",
            ],
            "empty query keeps insertion order"
        );

        matcher.set_query("edrs");
        settle(&mut matcher);
        let first = matcher.get(0).unwrap();
        assert_eq!(first.path.as_ref(), "crates/flux-app/src/editor.rs");
        assert_eq!(first.positions.len(), 4);

        matcher.set_query("ed");
        settle(&mut matcher);
        let mut found = paths(&mut matcher);
        found.sort();
        assert_eq!(found, ["README.md", "crates/flux-app/src/editor.rs"]);
        // The query was appended to: nucleo goes through only the previous matches.
        matcher.set_query("edit");
        settle(&mut matcher);
        assert_eq!(paths(&mut matcher), ["crates/flux-app/src/editor.rs"]);
        // The query got shorter: wider again.
        matcher.set_query("e");
        settle(&mut matcher);
        assert_eq!(matcher.match_count(), 3);
        // `$` is the exact end of the path, not a fuzzy one.
        matcher.set_query("md$");
        settle(&mut matcher);
        assert_eq!(matcher.match_count(), 2);

        // Cyrillic in a path: the positions are character indices, not grapheme or byte indices.
        matcher.set_query("редmd");
        settle(&mut matcher);
        let found = matcher.get(0).unwrap();
        assert_eq!(found.path.as_ref(), "docs/заметки/редактор.md");
        let chars: Vec<char> = found.path.chars().collect();
        let picked: String = found.positions.iter().map(|&i| chars[i]).collect();
        assert_eq!(picked, "редmd");
    }

    #[test]
    fn negated_append_rescores() {
        let mut matcher = PathMatcher::new(Arc::new(|| {}));
        let injector = matcher.injector();
        for path in ["a.rs", "ab.rs", "abc.rs"] {
            injector.push(Path::new(path));
        }
        drop(injector);
        matcher.set_query("!ab");
        settle(&mut matcher);
        assert_eq!(paths(&mut matcher), ["a.rs"]);
        // "!abc" excludes fewer than "!ab": the appended query widened the result set.
        matcher.set_query("!abc");
        settle(&mut matcher);
        let mut found = paths(&mut matcher);
        found.sort();
        assert_eq!(found, ["a.rs", "ab.rs"]);
    }
}
