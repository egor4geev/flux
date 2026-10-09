//! The TODO items of a scan, apart from Flux: the search pattern, the matches grouped by file, the
//! labels of the rows. Pure functions, tested natively.

/// A line of the project search, as the host reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// Zero-based.
    pub line: u32,
    /// The line's text; a long line is a window around the first match.
    pub text: String,
    /// The column in characters where `text` starts in the real line.
    pub column_offset: u32,
    /// The matches: spans of columns in characters within `text`.
    pub ranges: Vec<(u32, u32)>,
}

/// The lines of a file with matches.
#[derive(Debug, Clone, PartialEq)]
pub struct FileLines {
    /// Relative to the project root.
    pub path: String,
    pub lines: Vec<Line>,
}

/// A TODO item: a line with a match.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// Zero-based.
    pub line: u32,
    /// The first match in the real line, columns in characters: opening the item selects it.
    pub start: u32,
    pub end: u32,
    /// The line from the first match on, as JetBrains IDEs show it: «TODO: handle errors».
    pub label: String,
    /// The matches within `label`, columns in characters.
    pub highlights: Vec<(u32, u32)>,
}

/// The items of a file, in line order.
#[derive(Debug, Clone, PartialEq)]
pub struct File {
    /// Relative to the project root, `/`-separated.
    pub path: String,
    pub items: Vec<Item>,
}

/// One regular expression for all the patterns: `(?:a)|(?:b)`; none when every pattern is blank.
pub fn pattern(patterns: &[String]) -> Option<String> {
    let parts: Vec<String> = patterns
        .iter()
        .map(|pattern| pattern.trim())
        .filter(|pattern| !pattern.is_empty())
        .map(|pattern| format!("(?:{pattern})"))
        .collect();
    (!parts.is_empty()).then(|| parts.join("|"))
}

/// The files sorted by path (folders first are not special: a path order, case-insensitive), the
/// items by line.
pub fn group(files: Vec<FileLines>) -> Vec<File> {
    let mut files: Vec<File> = files
        .into_iter()
        .map(|file| {
            let mut items: Vec<Item> = file.lines.into_iter().map(item).collect();
            items.sort_by_key(|item| (item.line, item.start));
            File {
                path: file.path.replace('\\', "/"),
                items,
            }
        })
        .filter(|file| !file.items.is_empty())
        .collect();
    files.sort_by(|a, b| {
        a.path
            .to_lowercase()
            .cmp(&b.path.to_lowercase())
            .then_with(|| a.path.cmp(&b.path))
    });
    files
}

/// The item of a line: the label starts at the first match.
fn item(line: Line) -> Item {
    let mut ranges = line.ranges;
    ranges.sort();
    let whole = |line: &Line| Item {
        line: line.line,
        start: line.column_offset,
        end: line.column_offset,
        label: line.text.trim().to_string(),
        highlights: Vec::new(),
    };
    // The host dropped the matches it couldn't fit into the window of a long line.
    let Some(&(first, first_end)) = ranges.first() else {
        return whole(&Line { ranges, ..line });
    };
    let label: String = line.text.chars().skip(first as usize).collect();
    let label = without_comment_end(&label);
    if label.is_empty() {
        return whole(&Line { ranges, ..line });
    }
    let length = label.chars().count() as u32;
    let highlights = ranges
        .iter()
        .map(|&(start, end)| (start - first, end.saturating_sub(first).min(length)))
        .filter(|(start, end)| start < end)
        .collect();
    Item {
        line: line.line,
        start: line.column_offset + first,
        end: line.column_offset + first_end,
        label: label.to_string(),
        highlights,
    }
}

/// The text without the end of a block comment it closes (`*/`, `-->`) and trailing spaces:
/// the item is the comment's text, as JetBrains IDEs show it.
fn without_comment_end(text: &str) -> &str {
    let text = text.trim_end();
    ["*/", "-->"]
        .iter()
        .find_map(|end| text.strip_suffix(end))
        .map_or(text, str::trim_end)
}

/// The items and the files.
pub fn count<'a>(files: impl IntoIterator<Item = &'a File>) -> (usize, usize) {
    files.into_iter().fold((0, 0), |(items, files), file| {
        (items + file.items.len(), files + 1)
    })
}

/// A file's name and its folder: `src/ui/main.rs` → (`main.rs`, `src/ui`); the folder is empty at
/// the root.
pub fn split_path(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((folder, name)) => (name, folder),
        None => (path, ""),
    }
}

/// The path relative to the root, `/`-separated; none outside the root.
pub fn relative(root: &str, path: &str) -> Option<String> {
    let root = root.trim_end_matches('/');
    let rest = path.strip_prefix(root)?.strip_prefix('/')?;
    (!rest.is_empty()).then(|| rest.to_string())
}

/// The absolute path of a file of the project.
pub fn absolute(root: &str, relative: &str) -> String {
    format!("{}/{relative}", root.trim_end_matches('/'))
}

/// The key of a file's row.
pub fn file_key(path: &str) -> String {
    format!("f:{path}")
}

/// The key of an item's row: stable while the item stays where it is.
pub fn item_key(path: &str, item: &Item) -> String {
    format!("i:{path}:{}:{}", item.line, item.start)
}

/// The label split into plain and highlighted pieces, in order: `(text, highlighted)`.
pub fn pieces(item: &Item) -> Vec<(String, bool)> {
    let chars: Vec<char> = item.label.chars().collect();
    let mut pieces = Vec::new();
    let mut at = 0usize;
    for &(start, end) in &item.highlights {
        let (start, end) = (start as usize, end as usize);
        if start < at || end > chars.len() {
            continue;
        }
        if start > at {
            pieces.push((chars[at..start].iter().collect(), false));
        }
        pieces.push((chars[start..end].iter().collect(), true));
        at = end;
    }
    if at < chars.len() {
        pieces.push((chars[at..].iter().collect(), false));
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(line: u32, text: &str, ranges: &[(u32, u32)]) -> Line {
        Line {
            line,
            text: text.to_string(),
            column_offset: 0,
            ranges: ranges.to_vec(),
        }
    }

    #[test]
    fn joins_the_patterns() {
        let patterns = vec![
            "\\bTODO\\b".to_string(),
            " ".to_string(),
            "FIXME".to_string(),
        ];
        assert_eq!(pattern(&patterns).unwrap(), "(?:\\bTODO\\b)|(?:FIXME)");
        assert_eq!(pattern(&[]), None);
        assert_eq!(pattern(&["  ".to_string()]), None);
    }

    #[test]
    fn labels_start_at_the_match() {
        let files = group(vec![FileLines {
            path: "src/main.rs".into(),
            lines: vec![line(
                11,
                "    let x = 1; // TODO: handle errors   ",
                &[(18, 22)],
            )],
        }]);
        let item = &files[0].items[0];
        assert_eq!(item.label, "TODO: handle errors");
        assert_eq!(item.highlights, [(0, 4)]);
        assert_eq!((item.line, item.start, item.end), (11, 18, 22));
        assert_eq!(
            pieces(item),
            [
                ("TODO".to_string(), true),
                (": handle errors".to_string(), false)
            ]
        );
    }

    #[test]
    fn cyrillic_and_a_window_of_a_long_line() {
        let item = item(Line {
            line: 3,
            text: "ф // FIXME и TODO".into(),
            column_offset: 100,
            ranges: vec![(13, 17), (5, 10)],
        });
        assert_eq!(item.label, "FIXME и TODO");
        assert_eq!(item.highlights, [(0, 5), (8, 12)]);
        assert_eq!((item.start, item.end), (105, 110));
        assert_eq!(
            pieces(&item),
            [
                ("FIXME".to_string(), true),
                (" и ".to_string(), false),
                ("TODO".to_string(), true)
            ]
        );
    }

    #[test]
    fn block_comment_ends_are_not_part_of_the_item() {
        let c = item(line(0, "/* FIXME: the layout breaks */  ", &[(3, 8)]));
        assert_eq!(c.label, "FIXME: the layout breaks");
        let html = item(line(0, "<!-- TODO: alt text -->", &[(5, 9)]));
        assert_eq!(html.label, "TODO: alt text");
        let math = item(line(0, "// TODO: a */ b", &[(3, 7)]));
        assert_eq!(math.label, "TODO: a */ b");
    }

    #[test]
    fn a_line_without_ranges_keeps_its_text() {
        let item = item(line(0, "  todo!()  ", &[]));
        assert_eq!(item.label, "todo!()");
        assert!(item.highlights.is_empty());
    }

    #[test]
    fn files_by_path_items_by_line() {
        let files = group(vec![
            FileLines {
                path: "b.rs".into(),
                lines: vec![line(9, "TODO b2", &[(0, 4)]), line(1, "TODO b1", &[(0, 4)])],
            },
            FileLines {
                path: "A.rs".into(),
                lines: vec![line(0, "TODO a", &[(0, 4)])],
            },
            FileLines {
                path: "empty.rs".into(),
                lines: vec![],
            },
        ]);
        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["A.rs", "b.rs"]);
        assert_eq!(files[1].items[0].line, 1);
        assert_eq!(count(&files), (3, 2));
    }

    #[test]
    fn paths() {
        assert_eq!(split_path("src/ui/main.rs"), ("main.rs", "src/ui"));
        assert_eq!(split_path("README.md"), ("README.md", ""));
        assert_eq!(
            relative("/p/flux", "/p/flux/src/a.rs").as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(
            relative("/p/flux/", "/p/flux/a.rs").as_deref(),
            Some("a.rs")
        );
        assert_eq!(relative("/p/flux", "/p/fluxx/a.rs"), None);
        assert_eq!(relative("/p/flux", "/p/flux"), None);
        assert_eq!(absolute("/p/flux/", "src/a.rs"), "/p/flux/src/a.rs");
    }
}
