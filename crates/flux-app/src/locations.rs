//! A list of places the language server named — usages of a symbol, several definitions — as a
//! `Picker`: each row is the line of code with the symbol highlighted, and its file and line.
//! Enter jumps there (and Back returns).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use flux_core::Rope;
use flux_core::text::{line_len, line_start};
use flux_lsp::lsp_types::{self, GotoDefinitionResponse, Location};
use flux_lsp::position::{self, Lines};
use flux_search::{FuzzyMatch, match_list};
use gpui::{
    AnyElement, Context, DismissEvent, SharedString, WeakEntity, Window, div, prelude::*, px,
};

use crate::i18n::{tr, trf, trn};
use crate::icons::file_icon;
use crate::navigation::{self, Place, canonical};
use crate::picker::{Picker, PickerDelegate, highlighted_text};
use crate::theme::{self, Theme};
use crate::workspace::{Workspace, tilde};

/// A row shows at most this many characters of its line; a long line is cut around the symbol.
const MAX_LINE_CHARS: usize = 120;
/// A path longer than this is shortened in the middle.
const MAX_PATH_CHARS: usize = 44;

/// A place the server named: a file and an LSP range in it (converted to characters only against
/// the text it is opened with).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NavTarget {
    pub path: PathBuf,
    pub range: lsp_types::Range,
}

/// Places of a definition response; `LocationLink`s point at the name (`targetSelectionRange`).
/// Places outside files (other URI schemes) are dropped, duplicates removed.
pub(crate) fn from_definition(response: Option<GotoDefinitionResponse>) -> Vec<NavTarget> {
    let places: Vec<(lsp_types::Uri, lsp_types::Range)> = match response {
        None => Vec::new(),
        Some(GotoDefinitionResponse::Scalar(location)) => vec![(location.uri, location.range)],
        Some(GotoDefinitionResponse::Array(locations)) => locations
            .into_iter()
            .map(|location| (location.uri, location.range))
            .collect(),
        Some(GotoDefinitionResponse::Link(links)) => links
            .into_iter()
            .map(|link| (link.target_uri, link.target_selection_range))
            .collect(),
    };
    targets(places)
}

/// Places of a references response.
pub(crate) fn from_locations(locations: Vec<Location>) -> Vec<NavTarget> {
    targets(
        locations
            .into_iter()
            .map(|location| (location.uri, location.range)),
    )
}

fn targets(places: impl IntoIterator<Item = (lsp_types::Uri, lsp_types::Range)>) -> Vec<NavTarget> {
    let mut seen = HashSet::new();
    places
        .into_iter()
        .filter_map(|(uri, range)| {
            let path = position::path_from_uri(&uri)?;
            Some(NavTarget { path, range })
        })
        .filter(|target| seen.insert((target.path.clone(), target.range)))
        .collect()
}

/// What the list shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ListKind {
    Definitions,
    /// Usages of the symbol (its name, if the cursor was on a word).
    Usages {
        symbol: Option<String>,
    },
}

/// A row of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocationRow {
    pub target: NavTarget,
    /// The line of code without indentation, possibly cut ("…" at the cut edges).
    pub text: String,
    /// The symbol within `text`, in characters.
    pub symbol: Range<usize>,
    /// "src/main.rs:12" — the path relative to the project root (`~` outside it) and the line.
    pub place: String,
    pub file_name: String,
}

/// Rows for the places, in order: the file of `first` (the one the cursor is in) first, then by
/// path, line, and column. Lines come from `texts` (open documents, by canonical path — they may
/// have unsaved edits the server already knows), other files are read from disk; a file that
/// can't be read gives a row without code.
pub(crate) fn rows(
    targets: Vec<NavTarget>,
    texts: &HashMap<PathBuf, Rope>,
    root: Option<&Path>,
    first: Option<&Path>,
) -> Vec<LocationRow> {
    // Per file: the text is taken or read once, and its positions converted through one `Lines`.
    let mut index: HashMap<PathBuf, usize> = HashMap::new();
    let mut files: Vec<(PathBuf, Vec<NavTarget>)> = Vec::new();
    for target in targets {
        let path = canonical(&target.path);
        let at = *index.entry(path.clone()).or_insert_with(|| {
            files.push((path, Vec::new()));
            files.len() - 1
        });
        files[at].1.push(target);
    }
    let mut rows: Vec<(bool, String, LocationRow)> = Vec::new();
    for (path, targets) in files {
        let read;
        let text = match texts.get(&path) {
            Some(text) => Some(text),
            None => {
                read = fs::read_to_string(&path)
                    .ok()
                    .map(|text| Rope::from_str(&text));
                read.as_ref()
            }
        };
        let lines = text.map(Lines::new);
        let relative = display_path(&path, root);
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let elsewhere = first != Some(path.as_path());
        for target in targets {
            let (line, code, symbol) = match (text, &lines) {
                (Some(text), Some(lines)) => line_preview(text, lines, target.range),
                _ => (target.range.start.line as usize, String::new(), 0..0),
            };
            let row = LocationRow {
                place: format!("{}:{}", shorten(&relative, MAX_PATH_CHARS), line + 1),
                file_name: file_name.clone(),
                text: code,
                symbol,
                target,
            };
            rows.push((elsewhere, relative.clone(), row));
        }
    }
    rows.sort_by(|a, b| {
        let start = |row: &LocationRow| {
            (
                row.target.range.start.line,
                row.target.range.start.character,
            )
        };
        (a.0, &a.1, start(&a.2)).cmp(&(b.0, &b.1, start(&b.2)))
    });
    rows.into_iter().map(|(_, _, row)| row).collect()
}

/// The line of `range.start` for a row: zero-based line number, the text without the line break and
/// indentation (cut around the symbol if long), and the symbol's characters within it (to the end
/// of the line if the range continues past it).
fn line_preview(
    text: &Rope,
    lines: &Lines,
    range: lsp_types::Range,
) -> (usize, String, Range<usize>) {
    let chars = lines.range_from_lsp(range);
    let line = text.char_to_line(chars.start);
    let start = line_start(text, line);
    let content: Vec<char> = text
        .slice(start..start + line_len(text, line))
        .chars()
        .collect();
    let indent = content.iter().take_while(|c| c.is_whitespace()).count();
    let symbol_start = (chars.start - start).max(indent) - indent;
    let symbol_end = (chars.end.min(start + content.len()) - start).max(indent) - indent;
    let content = &content[indent..];
    let (text, symbol) = cut(content, symbol_start..symbol_end.max(symbol_start));
    (line, text, symbol)
}

/// At most `MAX_LINE_CHARS` characters of a long line, keeping the symbol in view: a cut edge is
/// marked with "…".
fn cut(line: &[char], symbol: Range<usize>) -> (String, Range<usize>) {
    if line.len() <= MAX_LINE_CHARS {
        return (line.iter().collect(), symbol);
    }
    // Some context before the symbol, the rest after it.
    let from = symbol
        .start
        .saturating_sub(MAX_LINE_CHARS / 4)
        .min(line.len() - MAX_LINE_CHARS);
    let to = from + MAX_LINE_CHARS;
    let mut text = String::new();
    let mut shift = from;
    if from > 0 {
        text.push('…');
        shift -= 1;
    }
    text.extend(&line[from..to]);
    if to < line.len() {
        text.push('…');
    }
    let clamp = |at: usize| (at.max(from).min(to)) - shift;
    (text, clamp(symbol.start)..clamp(symbol.end))
}

/// Relative to the project root; `~` outside it.
fn display_path(path: &Path, root: Option<&Path>) -> String {
    match root.and_then(|root| path.strip_prefix(root).ok()) {
        Some(relative) => relative.display().to_string(),
        None => tilde(path),
    }
}

/// "start…end": both the beginning of the path and the file name stay visible.
fn shorten(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_string();
    }
    let head = (max_chars - 1) / 3;
    let tail = max_chars - 1 - head;
    let mut short: String = chars[..head].iter().collect();
    short.push('…');
    short.extend(&chars[chars.len() - tail..]);
    short
}

/// Shows the rows as a list over the window; Enter jumps to the chosen place.
pub(crate) fn open(
    workspace: &mut Workspace,
    kind: ListKind,
    rows: Vec<LocationRow>,
    origin: Option<Place>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let weak = cx.entity().downgrade();
    workspace.toggle_modal(window, cx, move |window, cx| {
        Picker::new(LocationList::new(kind, rows, weak, origin), window, cx)
    });
}

pub(crate) struct LocationList {
    kind: ListKind,
    rows: Vec<LocationRow>,
    /// What the query is matched against: the code, then the place ("text src/a.rs:12").
    labels: Vec<String>,
    matches: Vec<FuzzyMatch>,
    /// The query is not empty: matched characters are highlighted instead of the symbol.
    filtering: bool,
    files: usize,
    workspace: WeakEntity<Workspace>,
    /// Where the cursor was: Back returns there.
    origin: Option<Place>,
}

impl LocationList {
    fn new(
        kind: ListKind,
        rows: Vec<LocationRow>,
        workspace: WeakEntity<Workspace>,
        origin: Option<Place>,
    ) -> Self {
        let labels = rows
            .iter()
            .map(|row| format!("{} {}", row.text, row.place))
            .collect();
        let files = rows
            .iter()
            .map(|row| &row.target.path)
            .collect::<HashSet<_>>()
            .len();
        Self {
            kind,
            rows,
            labels,
            matches: Vec::new(),
            filtering: false,
            files,
            workspace,
            origin,
        }
    }
}

impl PickerDelegate for LocationList {
    fn placeholder(&self) -> SharedString {
        match &self.kind {
            ListKind::Definitions => tr("Filter definitions…").into(),
            ListKind::Usages {
                symbol: Some(symbol),
            } => trf("Filter usages of {0}…", &[symbol]).into(),
            ListKind::Usages { symbol: None } => tr("Filter usages…").into(),
        }
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn update_matches(&mut self, query: &str, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.matches = match_list(query, &self.labels);
        self.filtering = !query.trim().is_empty();
        cx.notify();
    }

    fn confirm(&mut self, index: usize, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(row) = self.matches.get(index).map(|found| &self.rows[found.index]) else {
            return;
        };
        let (target, origin) = (row.target.clone(), self.origin.clone());
        cx.emit(DismissEvent);
        self.workspace
            .update(cx, |workspace, cx| {
                navigation::jump(workspace, target, origin, window, cx)
            })
            .ok();
    }

    fn render_match(
        &mut self,
        index: usize,
        _selected: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let Some(found) = self.matches.get(index) else {
            return div().into_any_element();
        };
        let row = &self.rows[found.index];
        // Matched characters of the label: the code first, then (after a space) the place.
        let text_len = row.text.chars().count();
        let (text_positions, place_positions) = if self.filtering {
            let (text, place): (Vec<usize>, Vec<usize>) =
                found.positions.iter().partition(|&&p| p < text_len);
            let place = place
                .into_iter()
                .filter_map(|p| p.checked_sub(text_len + 1))
                .collect();
            (text, place)
        } else {
            (row.symbol.clone().collect(), Vec::new())
        };
        let file = file_icon(&row.file_name, &ui);
        div()
            .w_full()
            .flex()
            .items_center()
            .gap_2p5()
            .whitespace_nowrap()
            .child(file.render())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .font_family(theme::code_font())
                    .text_color(ui.foreground)
                    .child(highlighted_text(
                        row.text.clone(),
                        &text_positions,
                        ui.match_text,
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(highlighted_text(
                        row.place.clone(),
                        &place_positions,
                        ui.match_text,
                    )),
            )
            .into_any_element()
    }

    fn render_footer(&self, _: &mut Window, _: &mut Context<Picker<Self>>) -> Option<AnyElement> {
        let count = self.rows.len();
        let text = match self.kind {
            ListKind::Definitions => trn(count, "{n} definition", "{n} definitions"),
            ListKind::Usages { .. } => format!(
                "{} {}",
                trn(count, "{n} usage", "{n} usages"),
                trn(self.files, "in {n} file", "in {n} files")
            ),
        };
        Some(text.into_any_element())
    }

    fn empty_message(&self) -> SharedString {
        tr("No matching places").into()
    }

    fn confirm_label(&self) -> &'static str {
        tr("go")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_lsp::lsp_types::{LocationLink, Position, Uri};

    fn range(line: u32, from: u32, to: u32) -> lsp_types::Range {
        lsp_types::Range::new(Position::new(line, from), Position::new(line, to))
    }

    fn uri(path: &str) -> Uri {
        position::uri_from_path(Path::new(path))
    }

    fn target(path: &str, line: u32, from: u32, to: u32) -> NavTarget {
        NavTarget {
            path: PathBuf::from(path),
            range: range(line, from, to),
        }
    }

    #[test]
    fn definition_responses_become_targets_without_duplicates() {
        let location = |line| Location::new(uri("/p/a.rs"), range(line, 3, 6));
        assert_eq!(
            from_definition(Some(GotoDefinitionResponse::Scalar(location(1)))),
            vec![target("/p/a.rs", 1, 3, 6)]
        );
        assert_eq!(
            from_definition(Some(GotoDefinitionResponse::Array(vec![
                location(1),
                location(1),
                location(4),
            ]))),
            vec![target("/p/a.rs", 1, 3, 6), target("/p/a.rs", 4, 3, 6)]
        );
        assert_eq!(from_definition(None), Vec::new());
    }

    #[test]
    fn links_point_at_the_name() {
        let link = LocationLink {
            origin_selection_range: None,
            target_uri: uri("/p/b.rs"),
            target_range: range(2, 0, 30),
            target_selection_range: range(2, 7, 10),
        };
        assert_eq!(
            from_definition(Some(GotoDefinitionResponse::Link(vec![link]))),
            vec![target("/p/b.rs", 2, 7, 10)]
        );
    }

    #[test]
    fn preview_drops_indentation_and_finds_the_symbol() {
        let text = Rope::from_str("fn main() {\n    let total = sum(1, 2);\n}\n");
        // `sum` is at line 1, columns 16..19.
        let (line, preview, symbol) = line_preview(&text, &Lines::new(&text), range(1, 16, 19));
        assert_eq!(line, 1);
        assert_eq!(preview, "let total = sum(1, 2);");
        assert_eq!(symbol, 12..15);
        assert_eq!(&preview[symbol], "sum");
    }

    #[test]
    fn preview_of_crlf_line_and_range_past_the_line() {
        let text = Rope::from_str("a\r\n  bc\r\nd");
        let range = lsp_types::Range::new(Position::new(1, 2), Position::new(2, 1));
        let (line, preview, symbol) = line_preview(&text, &Lines::new(&text), range);
        assert_eq!((line, preview.as_str(), symbol), (1, "bc", 0..2));
    }

    #[test]
    fn long_lines_are_cut_around_the_symbol() {
        let line: Vec<char> = format!("{}needle{}", "x".repeat(200), "y".repeat(200))
            .chars()
            .collect();
        let (text, symbol) = cut(&line, 200..206);
        assert_eq!(text.chars().count(), MAX_LINE_CHARS + 2);
        assert!(text.starts_with('…') && text.ends_with('…'));
        let chars: Vec<char> = text.chars().collect();
        assert_eq!(chars[symbol].iter().collect::<String>(), "needle");
        // A short line stays as it is.
        let short: Vec<char> = "let a = 1;".chars().collect();
        assert_eq!(cut(&short, 4..5), ("let a = 1;".to_string(), 4..5));
        // The symbol near the end of a long line: the cut keeps the end.
        let (text, symbol) = cut(&line, 395..400);
        assert!(!text.ends_with('…'));
        let chars: Vec<char> = text.chars().collect();
        assert_eq!(chars[symbol].iter().collect::<String>(), "yyyyy");
    }

    #[test]
    fn rows_come_from_open_texts_and_disk_in_order() {
        let dir = std::env::temp_dir().join(format!("flux-locations-{}", std::process::id()));
        fs::create_dir_all(dir.join("src")).unwrap();
        let dir = canonical(&dir);
        let open = dir.join("src/main.rs");
        let other = dir.join("src/lib.rs");
        fs::write(&open, "on disk\n").unwrap();
        fs::write(&other, "pub fn sum() {}\n\nfn x() { sum(); }\n").unwrap();
        let mut texts = HashMap::new();
        // The open document has unsaved text: rows show it, not the disk.
        texts.insert(open.clone(), Rope::from_str("fn main() { sum(); }\n"));
        let targets = vec![
            NavTarget {
                path: other.clone(),
                range: range(2, 9, 12),
            },
            NavTarget {
                path: other.clone(),
                range: range(0, 7, 10),
            },
            NavTarget {
                path: open.clone(),
                range: range(0, 12, 15),
            },
            NavTarget {
                path: dir.join("src/missing.rs"),
                range: range(4, 0, 3),
            },
        ];
        let rows = rows(targets, &texts, Some(&dir), Some(&other));
        let summary: Vec<(&str, &str)> = rows
            .iter()
            .map(|row| (row.place.as_str(), row.text.as_str()))
            .collect();
        assert_eq!(
            summary,
            vec![
                // The file the cursor is in goes first, by line.
                ("src/lib.rs:1", "pub fn sum() {}"),
                ("src/lib.rs:3", "fn x() { sum(); }"),
                ("src/main.rs:1", "fn main() { sum(); }"),
                ("src/missing.rs:5", ""),
            ]
        );
        assert_eq!(&rows[0].text[rows[0].symbol.clone()], "sum");
        assert_eq!(rows[2].file_name, "main.rs");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn long_paths_are_shortened_in_the_middle() {
        assert_eq!(shorten("src/main.rs", 44), "src/main.rs");
        let short = shorten(
            "crates/flux-app/src/very/deep/directory/structure/file.rs",
            30,
        );
        assert_eq!(short.chars().count(), 30);
        assert!(short.starts_with("crates/f") && short.ends_with("structure/file.rs"));
    }
}
