//! Links in terminal output: URLs and file locations — `src/main.rs:12:5` (rustc, go, gcc, grep -n,
//! rg --vimgrep), `src/app.ts(12,5)` (tsc), `File "app.py", line 3` (Python), `at f (/abs/x.js:10:5)`
//! (node), `/abs/path`, `~/path`, `./a`, `README.md`. OSC 8 hyperlinks set by the program are links
//! too ([`crate::Terminal::link_at`]).
//!
//! Detection is textual and generous: a word with a slash, an extension, or a line number is a
//! candidate, and [`resolve`] keeps only what exists on disk. Plain words are not candidates.

use std::ops::Range;
use std::path::{Component, Path, PathBuf};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::{Flags, Hyperlink};

use crate::content::GridPoint;

/// What a link points to, as written in the output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    Url(String),
    /// A path with an optional 1-based line and column; not yet checked against the filesystem
    /// ([`resolve`]).
    Path {
        path: String,
        line: Option<u32>,
        column: Option<u32>,
    },
}

/// A link in the grid: its first and last cells (inclusive) and where it points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub start: GridPoint,
    pub end: GridPoint,
    pub target: LinkTarget,
}

/// URL schemes recognized in text.
const SCHEMES: [&str; 3] = ["https://", "http://", "file://"];

/// How many rows a logical line may span on each side of the point: a wrapped line can be
/// thousands of rows long (a minified file), a link isn't.
const MAX_WRAPPED_ROWS: i32 = 20;

/// The link at a cell of the grid: an OSC 8 hyperlink the program put there, otherwise a URL or a
/// file location in the text of the logical line (rows the terminal wrapped count as one).
pub(crate) fn in_grid<T>(term: &Term<T>, point: Point) -> Option<Link> {
    let cells = logical_line(term, point.line);
    let index = cells.iter().position(|cell| {
        cell.point.line == point.line
            && (cell.point.column == point.column
                || (cell.wide && cell.point.column + 1 == point.column))
    })?;
    if let Some(hyperlink) = cells[index].hyperlink.clone() {
        let same = |cell: &LineCell| cell.hyperlink.as_ref() == Some(&hyperlink);
        let mut first = index;
        while first > 0 && same(&cells[first - 1]) {
            first -= 1;
        }
        let mut last = index;
        while last + 1 < cells.len() && same(&cells[last + 1]) {
            last += 1;
        }
        let uri = hyperlink.uri();
        let target = match file_url_path(uri) {
            Some(path) => LinkTarget::Path {
                path,
                line: None,
                column: None,
            },
            None => LinkTarget::Url(uri.to_string()),
        };
        return Some(Link {
            start: cells[first].start(),
            end: cells[last].end(),
            target,
        });
    }
    let text: String = cells.iter().map(|cell| cell.c).collect();
    let (range, target) = find_at(&text, index)?;
    Some(Link {
        start: cells[range.start].start(),
        end: cells[range.end - 1].end(),
        target,
    })
}

/// A character of a logical line and its cell.
struct LineCell {
    c: char,
    point: Point,
    /// The character takes this cell and the next one.
    wide: bool,
    hyperlink: Option<Hyperlink>,
}

impl LineCell {
    fn start(&self) -> GridPoint {
        GridPoint::new(self.point.line.0, self.point.column.0)
    }

    /// The last cell of the character: the right half of a wide one.
    fn end(&self) -> GridPoint {
        GridPoint::new(
            self.point.line.0,
            self.point.column.0 + usize::from(self.wide),
        )
    }
}

/// The characters of the logical line through `line`: one per character, wide characters' spacers
/// skipped.
fn logical_line<T>(term: &Term<T>, line: Line) -> Vec<LineCell> {
    let grid = term.grid();
    let last_column = term.last_column();
    let wraps = |line: Line| {
        grid[Point::new(line, last_column)]
            .flags
            .contains(Flags::WRAPLINE)
    };
    let mut top = line;
    while top > term.topmost_line() && (line - top).0 < MAX_WRAPPED_ROWS && wraps(top - 1) {
        top -= 1;
    }
    let mut bottom = line;
    while bottom < term.bottommost_line() && (bottom - line).0 < MAX_WRAPPED_ROWS && wraps(bottom) {
        bottom += 1;
    }
    let mut cells = Vec::new();
    for row in top.0..=bottom.0 {
        for column in 0..term.columns() {
            let point = Point::new(Line(row), Column(column));
            let cell = &grid[point];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            cells.push(LineCell {
                c: cell.c,
                point,
                wide: cell.flags.contains(Flags::WIDE_CHAR),
                hyperlink: cell.hyperlink(),
            });
        }
    }
    cells
}

/// The link that covers the character at `column` (a character index) of `line`: its character
/// range (end exclusive) and target.
pub fn find_at(line: &str, column: usize) -> Option<(Range<usize>, LinkTarget)> {
    let chars: Vec<char> = line.chars().collect();
    if column >= chars.len() {
        return None;
    }
    url_at(&chars, column)
        .or_else(|| python_location_at(&chars, column))
        .or_else(|| path_at(&chars, column))
}

/// A URL around `column`. A `file://` URL is a path.
fn url_at(chars: &[char], column: usize) -> Option<(Range<usize>, LinkTarget)> {
    let span = url_spans(chars)
        .into_iter()
        .find(|span| span.contains(&column))?;
    let url: String = chars[span.clone()].iter().collect();
    let target = match file_url_path(&url) {
        Some(path) => LinkTarget::Path {
            path,
            line: None,
            column: None,
        },
        None => LinkTarget::Url(url),
    };
    Some((span, target))
}

/// Every URL in the line: a scheme at the start of a word, then URL characters; sentence
/// punctuation and unbalanced closing brackets at the end are not part of it.
fn url_spans(chars: &[char]) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let scheme = SCHEMES
            .iter()
            .find(|scheme| starts_with(chars, i, scheme))
            .filter(|_| i == 0 || !chars[i - 1].is_alphanumeric());
        let Some(scheme) = scheme else {
            i += 1;
            continue;
        };
        let body = i + scheme.len();
        let mut end = body;
        while end < chars.len() && is_url_char(chars[end]) {
            end += 1;
        }
        while end > body {
            let last = chars[end - 1];
            let open = match last {
                ')' => Some('('),
                ']' => Some('['),
                '}' => Some('{'),
                _ => None,
            };
            let unbalanced = open.is_some_and(|open| {
                let count = |c: char| chars[i..end].iter().filter(|&&x| x == c).count();
                count(last) > count(open)
            });
            if ".,;:!?".contains(last) || unbalanced {
                end -= 1;
            } else {
                break;
            }
        }
        if end > body {
            spans.push(i..end);
        }
        i = end.max(i + 1);
    }
    spans
}

fn is_url_char(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_graphic() && !matches!(c, '"' | '\'' | '<' | '>' | '`')
    } else {
        // International domain names and paths; box drawing and other symbols end the URL.
        c.is_alphanumeric()
    }
}

/// The path of a `file://` URL: the host is dropped, `%XX` escapes are decoded.
pub(crate) fn file_url_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let path = percent_decode(&rest[rest.find('/')?..]);
    (path.len() > 1).then_some(path)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            decoded.push((high * 16 + low) as u8);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Python's traceback line `File "path", line N`: the quoted path, with the line.
fn python_location_at(chars: &[char], column: usize) -> Option<(Range<usize>, LinkTarget)> {
    const FILE: &str = "File \"";
    const LINE: &str = "\", line ";
    let mut i = 0;
    while i < chars.len() {
        if !starts_with(chars, i, FILE) {
            i += 1;
            continue;
        }
        let start = i + FILE.chars().count();
        let quote = start + chars[start..].iter().position(|&c| c == '"')?;
        i = quote;
        if !(start..quote).contains(&column) || !starts_with(chars, quote, LINE) {
            continue;
        }
        let digits = quote + LINE.chars().count();
        let (line, _) = number(chars, digits)?;
        let path = chars[start..quote].iter().collect();
        let target = LinkTarget::Path {
            path,
            line: Some(line),
            column: None,
        };
        return Some((start..quote, target));
    }
    None
}

/// A path around `column`, with a `:line:col` or `(line,col)` location after it.
fn path_at(chars: &[char], column: usize) -> Option<(Range<usize>, LinkTarget)> {
    if is_delimiter(chars[column]) {
        return None;
    }
    let mut start = column;
    while start > 0 && !is_delimiter(chars[start - 1]) {
        start -= 1;
    }
    let mut end = column + 1;
    while end < chars.len() && !is_delimiter(chars[end]) {
        end += 1;
    }
    // A colon or a period that ends a sentence or a message (`main.go:12:5: …`, `see a.rs.`).
    while end > start && matches!(chars[end - 1], '.' | ':' | '!' | '?') {
        end -= 1;
    }
    if start == end {
        return None;
    }

    // `path:line`, `path:line:col`, `path:line:text` (grep -n): the first `:` before a digit.
    let token = &chars[start..end];
    let colon = token
        .windows(2)
        .position(|pair| pair[0] == ':' && pair[1].is_ascii_digit());
    let (path_end, line, column_number, span_end) = match colon {
        Some(colon) => {
            let (line, after_line) = number(chars, start + colon + 1)?;
            let (column_number, span_end) = match chars.get(after_line) {
                Some(':') => number(chars, after_line + 1)
                    .map_or((None, after_line), |(col, end)| (Some(col), end)),
                _ => (None, after_line),
            };
            (start + colon, Some(line), column_number, span_end)
        }
        None => match parenthesized_location(chars, end) {
            // tsc: `src/app.ts(12,5)`.
            Some((line, column_number, span_end)) => (end, Some(line), column_number, span_end),
            None => (end, None, None, end),
        },
    };
    let path: String = chars[start..path_end].iter().collect();
    if !(start..span_end).contains(&column) || !looks_like_path(&path, line.is_some()) {
        return None;
    }
    let target = LinkTarget::Path {
        path,
        line,
        column: column_number,
    };
    Some((start..span_end, target))
}

/// `(line)` or `(line,col)` right at `at`: the numbers and the index after `)`.
fn parenthesized_location(chars: &[char], at: usize) -> Option<(u32, Option<u32>, usize)> {
    if chars.get(at) != Some(&'(') {
        return None;
    }
    let (line, mut i) = number(chars, at + 1)?;
    let mut column = None;
    if chars.get(i) == Some(&',') {
        let (col, after) = number(chars, i + 1)?;
        column = Some(col);
        i = after;
    }
    (chars.get(i) == Some(&')')).then_some((line, column, i + 1))
}

/// Decimal digits at `at`: the number and the index after them.
fn number(chars: &[char], at: usize) -> Option<(u32, usize)> {
    let digits = chars[at.min(chars.len())..]
        .iter()
        .take_while(|c| c.is_ascii_digit())
        .count();
    if digits == 0 || digits > 9 {
        return None;
    }
    let text: String = chars[at..at + digits].iter().collect();
    Some((text.parse().ok()?, at + digits))
}

/// Characters that end a path in text: spaces, quotes, brackets, and separators file names rarely
/// have.
fn is_delimiter(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(
            c,
            '"' | '\''
                | '`'
                | '<'
                | '>'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '|'
                | ','
                | ';'
                | '='
        )
        || ('\u{2500}'..='\u{259f}').contains(&c)
}

/// Whether a word is worth checking on disk: it has a letter and a slash, an extension, a leading
/// `~/`, a dotfile name, or a line number after it.
fn looks_like_path(path: &str, has_line: bool) -> bool {
    if path.is_empty() || path.contains("://") || !path.chars().any(char::is_alphabetic) {
        return false;
    }
    if has_line || path.contains('/') {
        return true;
    }
    // `README.md`, `Cargo.toml`, `.gitignore`.
    match path.rfind('.') {
        Some(0) => path.len() > 1,
        Some(dot) => {
            let extension = &path[dot + 1..];
            (1..=12).contains(&extension.len())
                && extension.chars().all(|c| c.is_alphanumeric() || c == '_')
        }
        None => false,
    }
}

fn starts_with(chars: &[char], at: usize, prefix: &str) -> bool {
    (at..)
        .zip(prefix.chars())
        .all(|(i, c)| chars.get(i) == Some(&c))
}

/// A path from the output resolved to an existing file or directory: absolute, `~/…`, relative to
/// `cwd` (the shell's directory), then relative to `root` (the project). git's `a/` and `b/`
/// prefixes (`git diff`) are dropped if the path as written doesn't exist.
pub fn resolve(path: &str, cwd: Option<&Path>, root: Option<&Path>) -> Option<PathBuf> {
    let find = |path: &str| {
        let path = expand_home(path)?;
        if path.is_absolute() {
            return existing(&path);
        }
        [cwd, root]
            .into_iter()
            .flatten()
            .find_map(|base| existing(&base.join(&path)))
    };
    find(path).or_else(|| {
        let stripped = path
            .strip_prefix("a/")
            .or_else(|| path.strip_prefix("b/"))?;
        find(stripped)
    })
}

/// `~` and `~/…` in the home directory.
fn expand_home(path: &str) -> Option<PathBuf> {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            let home = std::env::var_os("HOME")?;
            Some(PathBuf::from(home).join(rest.trim_start_matches('/')))
        }
        _ => Some(PathBuf::from(path)),
    }
}

/// The path without `.` and `..` (lexically, symlinks stay), if something exists there.
fn existing(path: &Path) -> Option<PathBuf> {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normal.pop() {
                    normal.push(component);
                }
            }
            other => normal.push(other),
        }
    }
    normal.exists().then_some(normal)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The link under the first occurrence of `under` in `line`: its text and target.
    fn link(line: &str, under: &str) -> Option<(String, LinkTarget)> {
        let byte = line.find(under).expect("marker in the line");
        let column = line[..byte].chars().count();
        let (range, target) = find_at(line, column)?;
        let text = line.chars().skip(range.start).take(range.len()).collect();
        Some((text, target))
    }

    fn path(path: &str, line: Option<u32>, column: Option<u32>) -> LinkTarget {
        LinkTarget::Path {
            path: path.into(),
            line,
            column,
        }
    }

    #[test]
    fn compiler_locations() {
        assert_eq!(
            link("  --> src/main.rs:12:5", "main"),
            Some((
                "src/main.rs:12:5".into(),
                path("src/main.rs", Some(12), Some(5))
            ))
        );
        assert_eq!(
            link("./main.go:12:5: undefined: x", "main"),
            Some((
                "./main.go:12:5".into(),
                path("./main.go", Some(12), Some(5))
            ))
        );
        assert_eq!(
            link("thread 'main' panicked at src/lib.rs:5:9:", "lib"),
            Some((
                "src/lib.rs:5:9".into(),
                path("src/lib.rs", Some(5), Some(9))
            ))
        );
        assert_eq!(
            link("src/app.ts(12,5): error TS2322", "app"),
            Some((
                "src/app.ts(12,5)".into(),
                path("src/app.ts", Some(12), Some(5))
            ))
        );
        assert_eq!(
            link("main.c:3:1: warning: x", "main"),
            Some(("main.c:3:1".into(), path("main.c", Some(3), Some(1))))
        );
    }

    #[test]
    fn grep_and_test_runner_locations() {
        assert_eq!(
            link("src/a.rs:12:fn main() {", "a.rs"),
            Some(("src/a.rs:12".into(), path("src/a.rs", Some(12), None)))
        );
        assert_eq!(
            link("src/a.rs:12:5:let x = 1;", "a.rs"),
            Some(("src/a.rs:12:5".into(), path("src/a.rs", Some(12), Some(5))))
        );
        assert_eq!(
            link("tests/test_x.py:12: AssertionError", "test_x"),
            Some((
                "tests/test_x.py:12".into(),
                path("tests/test_x.py", Some(12), None)
            ))
        );
        assert_eq!(
            link("    at foo (/abs/file.js:10:5)", "file"),
            Some((
                "/abs/file.js:10:5".into(),
                path("/abs/file.js", Some(10), Some(5))
            ))
        );
        assert_eq!(
            link("make: *** [Makefile:12: all] Error 1", "Makefile"),
            Some(("Makefile:12".into(), path("Makefile", Some(12), None)))
        );
    }

    #[test]
    fn python_tracebacks() {
        let line = r#"  File "/x/app.py", line 3, in <module>"#;
        assert_eq!(
            link(line, "app"),
            Some(("/x/app.py".into(), path("/x/app.py", Some(3), None)))
        );
        // The word "File" itself is not a link.
        assert_eq!(link(line, "File"), None);
    }

    #[test]
    fn plain_paths() {
        assert_eq!(
            link("see README.md for details", "README"),
            Some(("README.md".into(), path("README.md", None, None)))
        );
        assert_eq!(
            link("cd ~/dev/flux/x.rs", "dev"),
            Some((
                "~/dev/flux/x.rs".into(),
                path("~/dev/flux/x.rs", None, None)
            ))
        );
        assert_eq!(
            link("built ../b and ./a.", "./a"),
            Some(("./a".into(), path("./a", None, None)))
        );
        assert_eq!(
            link("modified: .gitignore", "git"),
            Some((".gitignore".into(), path(".gitignore", None, None)))
        );
        assert_eq!(
            link("diff --git a/src/x.rs b/src/x.rs", "a/src"),
            Some(("a/src/x.rs".into(), path("a/src/x.rs", None, None)))
        );
    }

    #[test]
    fn plain_words_and_numbers_are_not_links() {
        assert_eq!(link("error: could not compile", "error"), None);
        assert_eq!(link("listening on localhost", "localhost"), None);
        assert_eq!(link("version 1.2.3 released", "1.2"), None);
        assert_eq!(link("a b", " "), None);
        // A host with a port looks like a location, but has no letters in the "path".
        assert_eq!(link("at 127.0.0.1:8080 now", "127"), None);
    }

    #[test]
    fn urls() {
        assert_eq!(
            link("open https://example.com/a?b=1#c now", "example"),
            Some((
                "https://example.com/a?b=1#c".into(),
                LinkTarget::Url("https://example.com/a?b=1#c".into())
            ))
        );
        assert_eq!(
            link("(see http://x.org/wiki/A_(b)).", "x.org"),
            Some((
                "http://x.org/wiki/A_(b)".into(),
                LinkTarget::Url("http://x.org/wiki/A_(b)".into())
            ))
        );
        assert_eq!(
            link("server at http://localhost:8080, ok", "local"),
            Some((
                "http://localhost:8080".into(),
                LinkTarget::Url("http://localhost:8080".into())
            ))
        );
        assert_eq!(
            link("│ https://a.io │", "a.io"),
            Some((
                "https://a.io".into(),
                LinkTarget::Url("https://a.io".into())
            ))
        );
        // The scheme must start a word.
        assert_eq!(link("xhttp://a.io", "a.io"), None);
    }

    #[test]
    fn file_urls_are_paths() {
        assert_eq!(
            link("file:///Users/me/My%20Notes/a.md", "Users"),
            Some((
                "file:///Users/me/My%20Notes/a.md".into(),
                path("/Users/me/My Notes/a.md", None, None)
            ))
        );
        assert_eq!(
            file_url_path("file://host/tmp/x").as_deref(),
            Some("/tmp/x")
        );
    }

    #[test]
    fn cyrillic_and_wide_text_use_character_indices() {
        assert_eq!(
            link("ошибка в src/main.rs:3", "src"),
            Some(("src/main.rs:3".into(), path("src/main.rs", Some(3), None)))
        );
    }

    #[test]
    fn resolve_finds_existing_paths_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        let cwd = root.join("crates/app");
        std::fs::create_dir_all(cwd.join("src")).unwrap();
        std::fs::write(cwd.join("src/main.rs"), "").unwrap();
        std::fs::write(root.join("Cargo.toml"), "").unwrap();

        let cwd = Some(cwd.as_path());
        let root = Some(root.as_path());
        let found = |path: &str| resolve(path, cwd, root);
        let expected = dir.path().join("project/crates/app/src/main.rs");
        assert_eq!(found("src/main.rs"), Some(expected.clone()));
        assert_eq!(found("./src/../src/main.rs"), Some(expected.clone()));
        // Relative to the project when not in the shell's directory.
        assert_eq!(
            found("Cargo.toml"),
            Some(dir.path().join("project/Cargo.toml"))
        );
        assert_eq!(found("b/src/main.rs"), Some(expected.clone()));
        assert_eq!(found(expected.to_str().unwrap()), Some(expected));
        assert_eq!(
            found("src"),
            Some(dir.path().join("project/crates/app/src"))
        );
        assert_eq!(found("nope.rs"), None);
        assert_eq!(resolve("src/main.rs", None, None), None);
    }

    fn grid_link(
        term: &Term<alacritty_terminal::event::VoidListener>,
        line: i32,
        column: usize,
    ) -> Option<Link> {
        in_grid(term, Point::new(Line(line), Column(column)))
    }

    #[test]
    fn links_continue_through_wrapped_rows() {
        use alacritty_terminal::term::test::mock_term;
        // "see src/main.rs:3", wrapped by the terminal every 8 columns.
        let term = mock_term("see src/\nmain.rs:\n3\r\n");
        let link = grid_link(&term, 1, 2).unwrap();
        assert_eq!(link.start, GridPoint::new(0, 4));
        assert_eq!(link.end, GridPoint::new(2, 0));
        assert_eq!(link.target, path("src/main.rs", Some(3), None));
        // A hard line break ends the line.
        let term = mock_term("see src/\r\nmain.rs:\r\n");
        let link = grid_link(&term, 0, 5).unwrap();
        assert_eq!(link.end, GridPoint::new(0, 7));
        assert_eq!(link.target, path("src/", None, None));
    }

    #[test]
    fn wide_characters_take_two_cells() {
        use alacritty_terminal::term::test::mock_term;
        let term = mock_term("漢字 a.rs 漢.md\r\n");
        // Columns: 漢 0-1, 字 2-3, space 4, a.rs 5-8, space 9, 漢 10-11, .md 12-14.
        let link = grid_link(&term, 0, 6).unwrap();
        assert_eq!(
            (link.start, link.end),
            (GridPoint::new(0, 5), GridPoint::new(0, 8))
        );
        // The right half of a wide character belongs to it.
        let link = grid_link(&term, 0, 11).unwrap();
        assert_eq!(
            (link.start, link.end),
            (GridPoint::new(0, 10), GridPoint::new(0, 14))
        );
        assert_eq!(link.target, path("漢.md", None, None));
        assert_eq!(grid_link(&term, 0, 3), None);
    }

    #[test]
    fn osc8_hyperlinks_win() {
        use alacritty_terminal::term::test::mock_term;
        let mut term = mock_term("open src/a.rs or this\r\n");
        let set = |term: &mut Term<_>, columns: Range<usize>, uri: &str| {
            let hyperlink = Hyperlink::new(None::<String>, uri.to_string());
            for column in columns {
                term.grid_mut()[Line(0)][Column(column)].set_hyperlink(Some(hyperlink.clone()));
            }
        };
        // "this" links to a web page, "src/a.rs" to a file through a file:// URI.
        set(&mut term, 17..21, "https://example.com/x");
        set(&mut term, 5..13, "file://host/tmp/My%20a.rs");
        let link = grid_link(&term, 0, 18).unwrap();
        assert_eq!(
            (link.start, link.end),
            (GridPoint::new(0, 17), GridPoint::new(0, 20))
        );
        assert_eq!(link.target, LinkTarget::Url("https://example.com/x".into()));
        let link = grid_link(&term, 0, 7).unwrap();
        assert_eq!(
            (link.start, link.end),
            (GridPoint::new(0, 5), GridPoint::new(0, 12))
        );
        assert_eq!(link.target, path("/tmp/My a.rs", None, None));
    }

    #[test]
    fn home_is_expanded() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(expand_home("~"), Some(home.clone()));
        assert_eq!(expand_home("~/x/y"), Some(home.join("x/y")));
        assert_eq!(expand_home("~user/x"), Some(PathBuf::from("~user/x")));
    }
}
