//! The Markdown preview (part 10.4): a Markdown file opens as its text, its text and the rendered
//! preview side by side, or the preview, as the Markdown editor of JetBrains IDEs — three buttons
//! at the top right of the editor and the palette's Markdown: Editor Only / Editor And Preview /
//! Preview Only. The layout chosen last is the default of the next Markdown file
//! (`settings.json`: `markdown.layout`).
//!
//! The preview lives in the [`Editor`] of the file (its `markdown` state): the tab stays a regular
//! editor tab (saving, the dirty dot, closing) whatever the layout. The document is parsed by its
//! own block parser ([`parse_document`], on the helpers of [`crate::markdown`]): every block knows
//! its source lines, so the preview follows the editor's scroll — the block at the editor's top
//! line goes to the top of the preview. What READMEs use renders: headings with anchors, lists and
//! task lists, quotes and GitHub alerts (`> [!NOTE]`), code blocks with highlighting, tables,
//! images (local files; a picture from the web shows as its description), centered HTML blocks
//! (`<div align="center">`), `<kbd>`. An edit re-parses after a short pause, off the UI thread;
//! only the blocks that changed are measured again.
//!
//! Links: a web link opens in the browser, a file of the project in Flux (relative to the
//! document), `#anchor` scrolls the preview to its heading. A task's box toggles `[ ]` / `[x]` in
//! the text.

use std::cell::Cell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, App, Context, ElementId, FontWeight, Hsla, InteractiveText, KeyBinding,
    ListAlignment, ListOffset, ListState, SharedString, StyledText, Task, WeakEntity, Window,
    actions, canvas, div, img, list, prelude::*, px,
};

use crate::editor::Editor;
use crate::i18n::tr;
use crate::icons::{IconName, icon};
use crate::markdown::{self, Align, CodeBlock, Span, Table, Tag};
use crate::plugin_links::{self, Link};
use crate::settings::{self, MarkdownLayout};
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, CheckState, RADIUS_MD, RADIUS_SM};
use crate::workspace::Workspace;

actions!(
    markdown,
    [
        /// The Markdown file as text only.
        EditorOnly,
        /// The text and the preview side by side.
        EditorAndPreview,
        /// The preview only.
        PreviewOnly,
        /// The preview a little up (↑ in Preview Only).
        ScrollUp,
        /// The preview a little down (↓).
        ScrollDown,
        /// The preview a screen up (Page Up, ⇧Space).
        ScrollPageUp,
        /// The preview a screen down (Page Down, Space).
        ScrollPageDown,
        /// The top of the preview (⌘↑, Home).
        ScrollToTop,
        /// The end of the preview (⌘↓, End).
        ScrollToBottom
    ]
);

/// The key context of the editor in Preview Only: no editing keys, the preview's scrolling.
const CONTEXT: &str = "MarkdownPreview";

/// The pause after an edit before the preview parses the text again.
const DEBOUNCE: Duration = Duration::from_millis(120);
/// The readable column of the preview, as the chat's; the side and top insets around it.
const COLUMN_MAX_WIDTH: f32 = 780.;
const INSET_X: f32 = 32.;
const INSET_TOP: f32 = 28.;
const INSET_BOTTOM: f32 = 64.;
/// Body text.
const TEXT_SIZE: f32 = 14.;
const TEXT_LINE: f32 = 23.;
/// One step of ↑ / ↓ in Preview Only.
const SCROLL_STEP: f32 = 46.;
/// A level of list nesting.
const LIST_INDENT: f32 = 22.;

pub fn init(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", ScrollUp, context),
        KeyBinding::new("down", ScrollDown, context),
        KeyBinding::new("pageup", ScrollPageUp, context),
        KeyBinding::new("pagedown", ScrollPageDown, context),
        KeyBinding::new("shift-space", ScrollPageUp, context),
        KeyBinding::new("space", ScrollPageDown, context),
        KeyBinding::new("cmd-up", ScrollToTop, context),
        KeyBinding::new("home", ScrollToTop, context),
        KeyBinding::new("cmd-down", ScrollToBottom, context),
        KeyBinding::new("end", ScrollToBottom, context),
        KeyBinding::new("cmd-s", crate::editor::Save, context),
    ]);
}

/// A Markdown file, by its extension.
pub fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            ["md", "markdown", "mdown", "mkd", "mkdn"]
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

// --- The document ---

/// A parsed document: one block per item of the preview's list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Doc {
    pub blocks: Vec<DocBlock>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DocBlock {
    /// The source lines the block comes from (0-based, the end excluded).
    pub lines: Range<usize>,
    /// Inside a centered HTML block (`<div align="center">`, `<p align="center">`).
    pub centered: bool,
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// `anchor` — the slug a `#link` names it by, as GitHub makes them.
    Heading {
        level: u8,
        spans: Vec<Span>,
        anchor: String,
    },
    Paragraph(Vec<Span>),
    /// A list item: its nesting (0 — the top level), its marker, its text.
    Item {
        depth: usize,
        marker: Marker,
        spans: Vec<Span>,
    },
    /// A quote with the blocks inside it.
    Quote(Vec<DocBlock>),
    /// A GitHub alert: `> [!NOTE]` and the blocks after it.
    Alert {
        kind: AlertKind,
        blocks: Vec<DocBlock>,
    },
    Code(CodeBlock),
    Table(Table),
    Rule,
    /// Pictures standing alone: a row of them.
    Images(Vec<Image>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Marker {
    Bullet,
    /// "1.", "2)": the number as written.
    Number(String),
    /// A task (`- [ ]`, `- [x]`): checked or not.
    Task(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl AlertKind {
    fn from_marker(text: &str) -> Option<Self> {
        let inner = text.trim().strip_prefix("[!")?.strip_suffix(']')?;
        Some(match inner.to_ascii_uppercase().as_str() {
            "NOTE" => AlertKind::Note,
            "TIP" => AlertKind::Tip,
            "IMPORTANT" => AlertKind::Important,
            "WARNING" => AlertKind::Warning,
            "CAUTION" => AlertKind::Caution,
            _ => return None,
        })
    }

    fn title(self) -> &'static str {
        match self {
            AlertKind::Note => tr("Note"),
            AlertKind::Tip => tr("Tip"),
            AlertKind::Important => tr("Important"),
            AlertKind::Warning => tr("Warning"),
            AlertKind::Caution => tr("Caution"),
        }
    }

    fn icon(self) -> IconName {
        match self {
            AlertKind::Note => IconName::Info,
            AlertKind::Tip => IconName::Bulb,
            AlertKind::Important => IconName::Sparkle,
            AlertKind::Warning => IconName::Warning,
            AlertKind::Caution => IconName::Error,
        }
    }

    fn color(self, ui: &UiColors) -> Hsla {
        match self {
            AlertKind::Note => ui.info,
            AlertKind::Tip => ui.success,
            AlertKind::Important => ui.violet,
            AlertKind::Warning => ui.warning,
            AlertKind::Caution => ui.error,
        }
    }
}

/// A picture: `![alt](src)`, `<img src alt width>`, maybe inside a link.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub src: String,
    pub alt: String,
    /// The `width` attribute: pixels or a share of the column.
    pub width: Option<Dimension>,
    /// The `height` attribute, in pixels.
    pub height: Option<f32>,
    /// Where a click on the picture goes (`[![alt](src)](link)`, `<a href><img></a>`).
    pub link: Option<String>,
    /// The picture's own size in pixels, once read from its file.
    pub size: Option<(f32, f32)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dimension {
    Pixels(f32),
    Percent(f32),
}

impl Dimension {
    fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if let Some(percent) = value.strip_suffix('%') {
            return percent.trim().parse().ok().map(Dimension::Percent);
        }
        let number = value.strip_suffix("px").unwrap_or(value).trim();
        number
            .parse()
            .ok()
            .filter(|n: &f32| *n > 0.)
            .map(Dimension::Pixels)
    }
}

/// Parses a Markdown document into the preview's blocks.
pub fn parse_document(text: &str) -> Doc {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut first = 0;
    // YAML front matter (`---` … `---` at the very top): shown as YAML, as GitHub shows it.
    if lines.first().is_some_and(|line| line.trim_end() == "---")
        && let Some(end) = lines
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, line)| matches!(line.trim_end(), "---" | "..."))
            .map(|(index, _)| index)
        && end > 1
    {
        blocks.push(DocBlock {
            lines: 0..end + 1,
            centered: false,
            kind: Kind::Code(CodeBlock {
                language: Some("yaml".to_string()),
                text: lines[1..end].join("\n"),
                highlights: Vec::new(),
            }),
        });
        first = end + 1;
    }
    blocks.extend(parse_lines(&lines[first..], first));
    assign_anchors(&mut blocks);
    Doc { blocks }
}

/// The blocks of `lines`, whose first one is line `offset` of the document.
fn parse_lines(lines: &[&str], offset: usize) -> Vec<DocBlock> {
    let mut parser = Parser {
        lines,
        offset,
        blocks: Vec::new(),
        open: Open::None,
        regions: Vec::new(),
        list_indents: Vec::new(),
    };
    parser.run();
    parser.blocks
}

/// Starts of blocks: what ends a quote's lazy continuation.
fn starts_block(trimmed: &str) -> bool {
    markdown::fence_start(trimmed).is_some()
        || markdown::is_rule(trimmed)
        || markdown::heading_level(trimmed).is_some()
        || markdown::list_marker(trimmed).is_some()
        || trimmed.starts_with('<')
}

struct Parser<'a> {
    lines: &'a [&'a str],
    offset: usize,
    blocks: Vec<DocBlock>,
    open: Open<'a>,
    /// The HTML blocks the parser is inside (`<div align="center">`): their tags, centered or not.
    regions: Vec<(String, bool)>,
    /// The indents of the list items above the current one: their nesting.
    list_indents: Vec<usize>,
}

enum Open<'a> {
    None,
    Paragraph {
        start: usize,
        lines: Vec<&'a str>,
    },
    Item {
        start: usize,
        depth: usize,
        marker: Marker,
        lines: Vec<&'a str>,
    },
    Quote {
        start: usize,
        lines: Vec<&'a str>,
    },
}

impl<'a> Parser<'a> {
    fn run(&mut self) {
        let mut i = 0;
        while i < self.lines.len() {
            i = self.line(i);
        }
        self.close(self.lines.len());
    }

    fn centered(&self) -> bool {
        self.regions.iter().any(|(_, centered)| *centered)
    }

    fn push(&mut self, lines: Range<usize>, kind: Kind) {
        let centered = self.centered();
        self.blocks.push(DocBlock {
            lines: self.offset + lines.start..self.offset + lines.end,
            centered,
            kind,
        });
    }

    /// Handles line `i`; returns the next line to look at.
    fn line(&mut self, i: usize) -> usize {
        let line = self.lines[i];
        let trimmed = line.trim_start();
        let indent = markdown::indent_width(line);

        // A quote goes on while its lines start with `>`, and lazily over a line of text.
        if let Open::Quote { lines, .. } = &mut self.open {
            if let Some(rest) = trimmed.strip_prefix('>') {
                lines.push(rest.strip_prefix(' ').unwrap_or(rest));
                return i + 1;
            }
            if !trimmed.is_empty() && !starts_block(trimmed) {
                lines.push(line);
                return i + 1;
            }
            self.close(i);
        }

        if trimmed.starts_with("<!--") {
            self.close(i);
            let end = (i..self.lines.len())
                .find(|&j| self.lines[j].contains("-->"))
                .unwrap_or(self.lines.len() - 1);
            return end + 1;
        }

        if let Some((fence, info)) = markdown::fence_start(trimmed) {
            self.close(i);
            let mut code = Vec::new();
            let mut end = i + 1;
            while end < self.lines.len() {
                let candidate = self.lines[end];
                if markdown::is_fence_end(candidate.trim_start(), fence) {
                    break;
                }
                code.push(markdown::strip_indent(candidate, indent));
                end += 1;
            }
            let language = info
                .split_whitespace()
                .next()
                .map(|word| word.trim_matches(['{', '}', '.']).to_ascii_lowercase())
                .filter(|word| !word.is_empty());
            let last = end.min(self.lines.len() - 1);
            self.push(
                i..last + 1,
                Kind::Code(CodeBlock {
                    language,
                    text: code.join("\n"),
                    highlights: Vec::new(),
                }),
            );
            self.list_indents.clear();
            return last + 1;
        }

        if trimmed.contains('|')
            && let Some(align) = self
                .lines
                .get(i + 1)
                .and_then(|next| markdown::table_separator(next))
                .filter(|align| align.len() == markdown::table_cells(trimmed).len())
        {
            self.close(i);
            let header = markdown::table_cells(trimmed);
            let mut rows = Vec::new();
            let mut end = i + 2;
            while let Some(row) = self.lines.get(end).map(|line| line.trim()) {
                if row.is_empty() || !row.contains('|') {
                    break;
                }
                let mut cells = markdown::table_cells(row);
                cells.resize(align.len(), String::new());
                rows.push(cells.iter().map(|cell| inline(cell)).collect());
                end += 1;
            }
            self.push(
                i..end,
                Kind::Table(Table {
                    header: header.iter().map(|cell| inline(cell)).collect(),
                    align,
                    rows,
                }),
            );
            self.list_indents.clear();
            return end;
        }

        if trimmed.is_empty() {
            self.close(i);
            return i + 1;
        }

        // A setext heading: a paragraph underlined with `===` or `---`.
        if indent < 4
            && let Open::Paragraph { .. } = self.open
            && let Some(level) = setext_level(trimmed)
        {
            let Open::Paragraph { start, lines } = std::mem::replace(&mut self.open, Open::None)
            else {
                unreachable!()
            };
            let spans = inline(&markdown::join_lines(&lines));
            self.push(
                start..i + 1,
                Kind::Heading {
                    level,
                    spans,
                    anchor: String::new(),
                },
            );
            self.list_indents.clear();
            return i + 1;
        }

        if markdown::is_rule(trimmed) {
            self.close(i);
            self.push(i..i + 1, Kind::Rule);
            self.list_indents.clear();
            return i + 1;
        }

        if let Some((level, text)) = markdown::heading_level(trimmed) {
            self.close(i);
            self.push(
                i..i + 1,
                Kind::Heading {
                    level,
                    spans: inline(text),
                    anchor: String::new(),
                },
            );
            self.list_indents.clear();
            return i + 1;
        }

        if trimmed.starts_with('<') && self.html(i, trimmed) {
            return i + 1;
        }

        if let Some((marker, rest)) = markdown::list_marker(trimmed) {
            self.close(i);
            let depth = self.list_depth(indent);
            let (marker, rest) = match task(rest) {
                Some((checked, rest)) => (Marker::Task(checked), rest),
                None if marker == "•" => (Marker::Bullet, rest),
                None => (Marker::Number(marker), rest),
            };
            self.open = Open::Item {
                start: i,
                depth,
                marker,
                lines: vec![rest],
            };
            return i + 1;
        }

        if let Some(rest) = trimmed.strip_prefix('>') {
            self.close(i);
            self.open = Open::Quote {
                start: i,
                lines: vec![rest.strip_prefix(' ').unwrap_or(rest)],
            };
            self.list_indents.clear();
            return i + 1;
        }

        match &mut self.open {
            Open::Paragraph { lines, .. } => lines.push(line),
            Open::Item { lines, .. } => lines.push(trimmed),
            Open::None | Open::Quote { .. } => {
                if indent == 0 {
                    self.list_indents.clear();
                }
                self.open = Open::Paragraph {
                    start: i,
                    lines: vec![line],
                };
            }
        }
        i + 1
    }

    /// The nesting of a list item at `indent`: deeper than the item above when indented more.
    fn list_depth(&mut self, indent: usize) -> usize {
        while self.list_indents.last().is_some_and(|&top| top > indent) {
            self.list_indents.pop();
        }
        if self.list_indents.last() != Some(&indent) {
            self.list_indents.push(indent);
        }
        self.list_indents.len() - 1
    }

    /// An HTML line (`<div align="center">`, `<img …>`, `<h1>…</h1>`, `</p>`): `true` if handled.
    fn html(&mut self, i: usize, trimmed: &'a str) -> bool {
        let chars: Vec<char> = trimmed.chars().collect();
        let Some(tag) = markdown::tag_at(&chars, 0) else {
            return false;
        };
        let after = trimmed
            .char_indices()
            .nth(tag.end)
            .map_or(trimmed.len(), |(byte, _)| byte);
        let rest: &'a str = &trimmed[after..];
        let name = tag.name.as_str();
        match name {
            "div" | "p" | "center" | "section" | "header" | "footer" | "article" | "main"
            | "nav" | "figure" => {
                self.close(i);
                if tag.closing {
                    if let Some(at) = self.regions.iter().rposition(|(open, _)| open == name) {
                        self.regions.truncate(at);
                    }
                    return true;
                }
                let centered = name == "center"
                    || tag
                        .attribute("align")
                        .is_some_and(|align| align.eq_ignore_ascii_case("center"));
                self.regions.push((name.to_string(), centered));
                // Content on the same line: `<p align="center"><img …></p>`.
                let closing = format!("</{name}>");
                let content = rest.trim();
                let (content, closes) = match content.strip_suffix(closing.as_str()) {
                    Some(content) => (content.trim(), true),
                    None => (content, false),
                };
                if !content.is_empty() {
                    self.inline_line(i, content);
                    self.close(i + 1);
                }
                if closes {
                    self.regions.pop();
                }
                true
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" if !tag.closing => {
                self.close(i);
                let level = name[1..].parse().unwrap_or(1);
                let closing = format!("</{name}>");
                let inner = rest.split(closing.as_str()).next().unwrap_or("");
                let centered = tag
                    .attribute("align")
                    .is_some_and(|align| align.eq_ignore_ascii_case("center"));
                self.regions.push((name.to_string(), centered));
                self.push(
                    i..i + 1,
                    Kind::Heading {
                        level,
                        spans: inline(inner.trim()),
                        anchor: String::new(),
                    },
                );
                self.regions.pop();
                true
            }
            "summary" if !tag.closing => {
                self.close(i);
                let inner = rest.split("</summary>").next().unwrap_or("");
                let mut spans = inline(inner.trim());
                for span in &mut spans {
                    span.style.bold = true;
                }
                self.push(i..i + 1, Kind::Paragraph(spans));
                true
            }
            "details" | "br" | "picture" | "source" | "tbody" | "thead" => {
                // Wrappers whose content follows on its own lines (or nothing): the tag line
                // itself shows nothing. A `<picture>` keeps its `<img>`.
                if name == "picture" || name == "source" {
                    let content = rest.trim();
                    if content.contains("<img") {
                        self.inline_line(i, content);
                        return true;
                    }
                }
                self.close(i);
                true
            }
            "hr" => {
                self.close(i);
                self.push(i..i + 1, Kind::Rule);
                true
            }
            "img" | "a" => {
                // A line of pictures (maybe linked): a paragraph that turns into a row of them.
                self.inline_line(i, trimmed);
                true
            }
            "table" | "tr" | "td" | "th" | "ul" | "ol" | "li" | "blockquote" | "pre" | "dl"
            | "dt" | "dd" | "figcaption" | "span" | "small" | "sub" | "sup" | "b" | "strong"
            | "i" | "em" => {
                // Other HTML reads as its text.
                let text = strip_tags(trimmed);
                if text.trim().is_empty() {
                    self.close(i);
                } else {
                    self.inline_line(i, trimmed);
                }
                true
            }
            _ => false,
        }
    }

    /// A line of text (inline HTML maybe) into the open paragraph.
    fn inline_line(&mut self, i: usize, text: &'a str) {
        match &mut self.open {
            Open::Paragraph { lines, .. } => lines.push(text),
            _ => {
                self.close(i);
                self.open = Open::Paragraph {
                    start: i,
                    lines: vec![text],
                };
            }
        }
    }

    /// Ends the block being collected; `end` — the line after it.
    fn close(&mut self, end: usize) {
        match std::mem::replace(&mut self.open, Open::None) {
            Open::None => {}
            Open::Paragraph { start, lines } => {
                let end = start + lines.len();
                let text = markdown::join_lines(&lines);
                match images_only(&text) {
                    Some(images) => self.push(start..end, Kind::Images(images)),
                    None => {
                        let spans = inline(&text);
                        if spans.iter().any(|span| !span.text.trim().is_empty()) {
                            self.push(start..end, Kind::Paragraph(spans));
                        }
                    }
                }
            }
            Open::Item {
                start,
                depth,
                marker,
                lines,
            } => {
                let end = start + lines.len();
                let spans = inline(&markdown::join_lines(&lines));
                self.push(
                    start..end,
                    Kind::Item {
                        depth,
                        marker,
                        spans,
                    },
                );
            }
            Open::Quote { start, lines } => {
                let end = (start + lines.len()).min(end.max(start + 1));
                let first = lines.iter().position(|line| !line.trim().is_empty());
                let alert = first.and_then(|first| {
                    AlertKind::from_marker(lines[first]).map(|kind| (first, kind))
                });
                let kind = match alert {
                    Some((first, kind)) => Kind::Alert {
                        kind,
                        blocks: parse_lines(&lines[first + 1..], self.offset + start + first + 1),
                    },
                    None => Kind::Quote(parse_lines(&lines, self.offset + start)),
                };
                self.push(start..end, kind);
            }
        }
    }
}

/// The inline markup of a document's text.
fn inline(text: &str) -> Vec<Span> {
    markdown::parse_inline_document(text)
}

/// `===` (a level 1 heading) or `---` (level 2) under a paragraph.
fn setext_level(trimmed: &str) -> Option<u8> {
    let line = trimmed.trim_end();
    if !line.is_empty() && line.chars().all(|c| c == '=') {
        Some(1)
    } else if line.len() >= 2 && line.chars().all(|c| c == '-') {
        Some(2)
    } else {
        None
    }
}

/// A task item's box: `[ ] text`, `[x] text` → checked, the text.
fn task(rest: &str) -> Option<(bool, &str)> {
    let checked = match rest.get(..3)? {
        "[ ]" => false,
        "[x]" | "[X]" => true,
        _ => return None,
    };
    let after = &rest[3..];
    (after.is_empty() || after.starts_with(' ')).then(|| (checked, after.trim_start()))
}

/// The text of HTML without its tags.
fn strip_tags(html: &str) -> String {
    let chars: Vec<char> = html.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<'
            && let Some(tag) = markdown::tag_at(&chars, i)
        {
            i = tag.end;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// The pictures of a paragraph made of nothing else (`![a](x.png) ![b](y.png)`, `<img>`, linked
/// pictures, line breaks between them).
fn images_only(text: &str) -> Option<Vec<Image>> {
    let chars: Vec<char> = text.chars().collect();
    let mut images = Vec::new();
    let mut link: Option<String> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() || c == '\u{a0}' {
            i += 1;
            continue;
        }
        if c == '&'
            && let Some((decoded, next)) = markdown::entity(&chars, i)
            && decoded.is_whitespace()
        {
            i = next;
            continue;
        }
        if c == '!' && chars.get(i + 1) == Some(&'[') {
            let (label, next) = markdown::link(&chars, i + 1)?;
            let target = markdown::link_target(&chars, label.end, next)?;
            let alt: String = chars[label].iter().collect();
            images.push(Image {
                src: target,
                alt: alt.trim().to_string(),
                width: None,
                height: None,
                link: link.clone(),
                size: None,
            });
            i = next;
            continue;
        }
        if c == '[' && chars.get(i + 1) == Some(&'!') {
            // [![alt](src)](link): the picture inside, the link around it.
            let (label, next) = markdown::link(&chars, i)?;
            let target = markdown::link_target(&chars, label.end, next);
            let inner: String = chars[label].iter().collect();
            let mut inner_images = images_only(&inner)?;
            for image in &mut inner_images {
                image.link = target.clone();
            }
            images.extend(inner_images);
            i = next;
            continue;
        }
        if c == '<' {
            let tag = markdown::tag_at(&chars, i)?;
            match (tag.name.as_str(), tag.closing) {
                ("img", false) => images.push(image_of(&tag, link.clone())?),
                ("a", false) => link = tag.attribute("href").map(str::to_string),
                ("a", true) => link = None,
                ("br" | "picture" | "source" | "p" | "div" | "span", _) => {}
                _ => return None,
            }
            i = tag.end;
            continue;
        }
        return None;
    }
    (!images.is_empty()).then_some(images)
}

/// An `<img>` tag as a picture.
fn image_of(tag: &Tag, link: Option<String>) -> Option<Image> {
    let src = tag.attribute("src")?.trim().to_string();
    (!src.is_empty()).then(|| Image {
        src,
        alt: tag.attribute("alt").unwrap_or("").trim().to_string(),
        width: tag.attribute("width").and_then(Dimension::parse),
        height: tag.attribute("height").and_then(Dimension::parse).and_then(
            |height| match height {
                Dimension::Pixels(pixels) => Some(pixels),
                Dimension::Percent(_) => None,
            },
        ),
        link,
        size: None,
    })
}

/// Anchors for the headings, as GitHub makes them: lowercase, punctuation dropped, spaces as
/// hyphens; a repeated one gets `-1`, `-2`.
fn assign_anchors(blocks: &mut [DocBlock]) {
    let mut seen: HashMap<String, usize> = HashMap::new();
    for block in blocks {
        if let Kind::Heading { spans, anchor, .. } = &mut block.kind {
            let text: String = spans.iter().map(|span| span.text.as_str()).collect();
            let base = slug(&text);
            let count = seen.entry(base.clone()).or_insert(0);
            *anchor = if *count == 0 {
                base
            } else {
                format!("{base}-{count}")
            };
            *count += 1;
        }
    }
}

/// A heading's text as GitHub's anchor.
pub fn slug(text: &str) -> String {
    text.trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// The anchors of a document's headings: anchor → block.
fn anchors(doc: &Doc) -> HashMap<String, usize> {
    doc.blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| match &block.kind {
            Kind::Heading { anchor, .. } => Some((anchor.clone(), index)),
            _ => None,
        })
        .collect()
}

// --- Images and code off the UI thread ---

/// Reads the size of the document's local pictures from their files (only the headers).
fn measure_images(doc: &mut Doc, dir: Option<&Path>) {
    fn visit(blocks: &mut [DocBlock], dir: Option<&Path>) {
        for block in blocks {
            match &mut block.kind {
                Kind::Images(images) => {
                    for image in images {
                        if image.size.is_none()
                            && let Some(Destination::File(path, _)) = resolve(&image.src, dir)
                        {
                            image.size = picture_size(&path);
                        }
                    }
                }
                Kind::Quote(blocks) | Kind::Alert { blocks, .. } => visit(blocks, dir),
                _ => {}
            }
        }
    }
    visit(&mut doc.blocks, dir);
}

/// A picture file's size in pixels: a raster's from its header, an SVG's from its `viewBox`.
fn picture_size(path: &Path) -> Option<(f32, f32)> {
    let is_svg = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"));
    if is_svg {
        let data = std::fs::read(path).ok()?;
        let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
        let size = tree.size();
        return Some((size.width(), size.height()));
    }
    let (width, height) = image::image_dimensions(path).ok()?;
    (width > 0 && height > 0).then_some((width as f32, height as f32))
}

/// Highlights the document's code blocks by their fences' languages.
fn highlight(doc: &mut Doc, scopes: &[String]) {
    fn visit(blocks: &mut [DocBlock], scopes: &[String]) {
        for block in blocks {
            match &mut block.kind {
                Kind::Code(code) => markdown::highlight_code(code, None, scopes),
                Kind::Quote(blocks) | Kind::Alert { blocks, .. } => visit(blocks, scopes),
                _ => {}
            }
        }
    }
    visit(&mut doc.blocks, scopes);
}

// --- Links ---

/// Where a link of the document goes.
#[derive(Debug, Clone, PartialEq)]
pub enum Destination {
    /// The browser.
    Web(String),
    /// A file, opened in Flux; the anchor after `#`, if any.
    File(PathBuf, Option<String>),
    /// A heading of this document.
    Anchor(String),
}

/// A link's target relative to the document's folder `dir`: a web address, `#anchor`, a file.
pub fn resolve(target: &str, dir: Option<&Path>) -> Option<Destination> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    if markdown::is_web_link(target) {
        return Some(Destination::Web(target.to_string()));
    }
    if let Some(anchor) = target.strip_prefix('#') {
        return Some(Destination::Anchor(anchor.to_lowercase()));
    }
    // Another scheme (`data:`, `ftp:`) or a protocol-relative address: not ours.
    let scheme = target.split_once(':').map(|(scheme, _)| scheme);
    if scheme.is_some_and(|scheme| {
        scheme.len() > 1
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '+')
    }) || target.starts_with("//")
    {
        return None;
    }
    let (path, anchor) = match target.split_once('#') {
        Some((path, anchor)) => (path, Some(anchor.to_lowercase())),
        None => (target, None),
    };
    let path = path.split('?').next().unwrap_or(path);
    let path = percent_decode(path);
    let path = Path::new(&path);
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        dir?.join(path)
    };
    Some(Destination::File(normalize(&full), anchor))
}

/// `%20` and the like in a link's path.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = text.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_string())
}

/// `a/./b/../c` → `a/c`, without touching the disk.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

// --- Scroll sync ---

/// The block at the editor's top line `line` (fractional, 0-based) and how far into it, 0–1: the
/// first block whose lines reach past `line`.
pub fn block_at_line(blocks: &[DocBlock], line: f32) -> Option<(usize, f32)> {
    if blocks.is_empty() {
        return None;
    }
    let index = blocks.partition_point(|block| (block.lines.end as f32) <= line);
    if index >= blocks.len() {
        return Some((blocks.len() - 1, 1.));
    }
    let block = &blocks[index];
    let start = block.lines.start as f32;
    let len = (block.lines.end - block.lines.start).max(1) as f32;
    Some((index, ((line - start) / len).clamp(0., 1.)))
}

// --- The state in the editor ---

/// A Markdown file's preview in its editor.
pub struct MarkdownState {
    pub layout: MarkdownLayout,
    doc: Rc<Doc>,
    list: ListState,
    /// The preview's width in the last frame: the column for the pictures.
    width: Rc<Cell<f32>>,
    /// The editor's top line the preview last followed.
    synced: Rc<Cell<Option<f32>>>,
    /// The parse after an edit (dropping it cancels it).
    parse: Option<Task<()>>,
    /// The text changed while the preview was hidden (Editor Only): it parses when shown.
    stale: bool,
}

impl MarkdownState {
    fn new(text: &str, dir: Option<&Path>, layout: MarkdownLayout) -> Self {
        let mut doc = parse_document(text);
        measure_images(&mut doc, dir);
        let list = ListState::new(doc.blocks.len(), ListAlignment::Top, px(600.));
        Self {
            layout,
            doc: Rc::new(doc),
            list,
            width: Rc::new(Cell::new(COLUMN_MAX_WIDTH + 2. * INSET_X)),
            synced: Rc::new(Cell::new(None)),
            parse: None,
            stale: false,
        }
    }

    /// Takes a newly parsed document: only the blocks that differ are measured again.
    fn replace(&mut self, doc: Doc) {
        let old = &self.doc.blocks;
        let new = &doc.blocks;
        let same = |a: &DocBlock, b: &DocBlock| a.kind == b.kind && a.centered == b.centered;
        let prefix = old
            .iter()
            .zip(new)
            .take_while(|(a, b)| same(a, b) && a.lines == b.lines)
            .count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| same(a, b))
            .count();
        let old_range = prefix..old.len() - suffix;
        let count = new.len() - suffix - prefix;
        if old_range.is_empty() && count == 0 && old.len() == new.len() {
            // Only the lines moved (an edit inside a block's lines did nothing to its look).
            self.doc = Rc::new(doc);
            return;
        }
        self.list.splice(old_range, count);
        self.doc = Rc::new(doc);
        self.synced.set(None);
    }
}

/// The layout of the editor's Markdown file, creating its preview state the first time; `None` —
/// not a Markdown file (or an editor that shows no layouts, see [`Editor::shows_layouts`]).
pub(crate) fn layout(editor: &mut Editor, cx: &mut Context<Editor>) -> Option<MarkdownLayout> {
    if !editor.shows_layouts() {
        return None;
    }
    let path = editor.document.path()?;
    if !is_markdown(path) {
        return None;
    }
    if editor.markdown.is_none() {
        let dir = path.parent().map(Path::to_path_buf);
        let text = editor.document.text().to_string();
        editor.markdown = Some(MarkdownState::new(
            &text,
            dir.as_deref(),
            settings::markdown_layout(cx),
        ));
        // Code blocks get their colors off the UI thread.
        schedule(editor, Duration::ZERO, cx);
    }
    editor.markdown.as_ref().map(|state| state.layout)
}

/// The text changed: the preview parses it again after a pause (in Editor Only, once it shows).
pub(crate) fn text_changed(editor: &mut Editor, cx: &mut Context<Editor>) {
    match &mut editor.markdown {
        Some(state) if state.layout == MarkdownLayout::Editor => state.stale = true,
        Some(_) => schedule(editor, DEBOUNCE, cx),
        None => {}
    }
}

/// The theme changed: code blocks get the new colors.
pub(crate) fn theme_changed(editor: &mut Editor, cx: &mut Context<Editor>) {
    if editor.markdown.is_some() {
        schedule(editor, Duration::ZERO, cx);
    }
}

/// Parses the text again after `delay` (a newer call cancels an older one).
fn schedule(editor: &mut Editor, delay: Duration, cx: &mut Context<Editor>) {
    let task = cx.spawn(async move |this: WeakEntity<Editor>, cx| {
        if !delay.is_zero() {
            cx.background_executor().timer(delay).await;
        }
        let Ok((text, dir, scopes)) = this.update(cx, |editor, cx| {
            let dir = editor
                .document
                .path()
                .and_then(Path::parent)
                .map(Path::to_path_buf);
            (
                editor.document.text().to_string(),
                dir,
                markdown::scopes(cx),
            )
        }) else {
            return;
        };
        let doc = cx
            .background_spawn(async move {
                let mut doc = parse_document(&text);
                measure_images(&mut doc, dir.as_deref());
                highlight(&mut doc, &scopes);
                doc
            })
            .await;
        this.update(cx, |editor, cx| {
            if let Some(state) = &mut editor.markdown {
                state.replace(doc);
                state.parse = None;
                cx.notify();
            }
        })
        .ok();
    });
    if let Some(state) = &mut editor.markdown {
        state.parse = Some(task);
    }
}

/// Switches the layout of the editor's Markdown file; it becomes the default of the next one.
pub(crate) fn set_layout(
    editor: &mut Editor,
    layout: MarkdownLayout,
    window: &mut Window,
    cx: &mut Context<Editor>,
) {
    let Some(state) = &mut editor.markdown else {
        return;
    };
    let mut reparse = false;
    if state.layout != layout {
        state.layout = layout;
        state.synced.set(None);
        settings::set_markdown_layout(layout, cx);
        reparse = std::mem::take(&mut state.stale) && layout != MarkdownLayout::Editor;
    }
    if reparse {
        schedule(editor, Duration::ZERO, cx);
    }
    // The keyboard stays with the file whatever it shows.
    window.focus(&editor.focus_handle);
    editor.autoscroll = Some(crate::editor::Autoscroll::Fit);
    cx.notify();
}

/// The preview's scrolling in Preview Only.
fn scroll(editor: &mut Editor, how: ScrollHow, cx: &mut Context<Editor>) {
    let Some(state) = &editor.markdown else {
        return;
    };
    let list = &state.list;
    let page = (list.viewport_bounds().size.height - px(SCROLL_STEP)).max(px(SCROLL_STEP));
    match how {
        ScrollHow::By(step) => list.scroll_by(px(step)),
        ScrollHow::Page(direction) => list.scroll_by(page * direction),
        ScrollHow::Top => list.scroll_to(ListOffset::default()),
        ScrollHow::Bottom => list.scroll_to(ListOffset {
            item_ix: list.item_count(),
            offset_in_item: px(0.),
        }),
    }
    cx.notify();
}

/// Preview Only → Editor and Preview, letting the command that needs the text go on.
fn reveal_text(editor: &mut Editor, window: &mut Window, cx: &mut Context<Editor>) {
    set_layout(editor, MarkdownLayout::Split, window, cx);
    cx.propagate();
}

/// How Preview Only scrolls: by pixels or by screens (down — positive), to an end.
#[derive(Clone, Copy)]
enum ScrollHow {
    By(f32),
    Page(f32),
    Top,
    Bottom,
}

/// The actions of a Markdown file's editor in every layout: the three layouts.
pub(crate) fn layout_actions<E: InteractiveElement>(element: E, cx: &mut Context<Editor>) -> E {
    element
        .on_action(cx.listener(|editor, _: &EditorOnly, window, cx| {
            set_layout(editor, MarkdownLayout::Editor, window, cx)
        }))
        .on_action(cx.listener(|editor, _: &EditorAndPreview, window, cx| {
            set_layout(editor, MarkdownLayout::Split, window, cx)
        }))
        .on_action(cx.listener(|editor, _: &PreviewOnly, window, cx| {
            set_layout(editor, MarkdownLayout::Preview, window, cx)
        }))
}

/// The editor in Preview Only: the preview takes the whole area, the keys scroll it, ⌘S saves.
pub(crate) fn render_preview_only(
    editor: &mut Editor,
    window: &mut Window,
    cx: &mut Context<Editor>,
) -> AnyElement {
    let root = div()
        .key_context(CONTEXT)
        .track_focus(&editor.focus_handle)
        .size_full()
        .relative()
        .flex()
        .on_action(cx.listener(|editor, _: &crate::editor::Save, _, cx| editor.save(cx).detach()))
        .on_action(cx.listener(|editor, _: &ScrollUp, _, cx| {
            scroll(editor, ScrollHow::By(-SCROLL_STEP), cx)
        }))
        .on_action(cx.listener(|editor, _: &ScrollDown, _, cx| {
            scroll(editor, ScrollHow::By(SCROLL_STEP), cx)
        }))
        .on_action(
            cx.listener(|editor, _: &ScrollPageUp, _, cx| scroll(editor, ScrollHow::Page(-1.), cx)),
        )
        .on_action(
            cx.listener(|editor, _: &ScrollPageDown, _, cx| {
                scroll(editor, ScrollHow::Page(1.), cx)
            }),
        )
        .on_action(cx.listener(|editor, _: &ScrollToTop, _, cx| scroll(editor, ScrollHow::Top, cx)))
        .on_action(
            cx.listener(|editor, _: &ScrollToBottom, _, cx| scroll(editor, ScrollHow::Bottom, cx)),
        );
    // Commands about the text (find, replace, go to line) show it first: Editor and Preview, and
    // the window goes on with the command.
    let root = root
        .on_action(
            cx.listener(|editor, _: &crate::find_bar::Deploy, window, cx| {
                reveal_text(editor, window, cx)
            }),
        )
        .on_action(
            cx.listener(|editor, _: &crate::find_bar::DeployReplace, window, cx| {
                reveal_text(editor, window, cx)
            }),
        )
        .on_action(
            cx.listener(|editor, _: &crate::go_to_line::Toggle, window, cx| {
                reveal_text(editor, window, cx)
            }),
        );
    let root = layout_actions(root, cx);
    root.child(render_preview(editor, false, window, cx))
        .child(render_toolbar(MarkdownLayout::Preview, cx))
        .into_any_element()
}

/// The text area and the preview side by side (Editor and Preview), or the text area alone with the
/// toolbar over it (Editor Only).
pub(crate) fn render_with_text(
    editor: &mut Editor,
    text_area: AnyElement,
    layout: MarkdownLayout,
    window: &mut Window,
    cx: &mut Context<Editor>,
) -> AnyElement {
    let ui = Theme::ui(cx);
    let content = match layout {
        MarkdownLayout::Split => div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_row()
            .child(div().flex_1().min_w_0().flex().flex_col().child(text_area))
            .child(div().flex_none().w(px(1.)).my_1().bg(ui.divider))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .child(render_preview(editor, true, window, cx)),
            )
            .into_any_element(),
        _ => text_area,
    };
    div()
        .flex_1()
        .min_h_0()
        .relative()
        .flex()
        .flex_col()
        .child(content)
        .child(render_toolbar(layout, cx))
        .into_any_element()
}

/// The three layout buttons at the top right of the editor, as in JetBrains IDEs.
fn render_toolbar(layout: MarkdownLayout, cx: &mut Context<Editor>) -> impl IntoElement + use<> {
    let ui = Theme::ui(cx);
    let buttons = [
        (
            MarkdownLayout::Editor,
            IconName::LayoutEditor,
            "Editor Only",
        ),
        (
            MarkdownLayout::Split,
            IconName::LayoutSplit,
            "Editor and Preview",
        ),
        (
            MarkdownLayout::Preview,
            IconName::LayoutPreview,
            "Preview Only",
        ),
    ];
    // Over the text and the preview's content: quieter until the mouse comes to it.
    div()
        .id("markdown-layouts")
        .absolute()
        .top(px(6.))
        .right(px(14.))
        .flex()
        .gap(px(2.))
        .p(px(2.))
        .rounded(px(RADIUS_MD))
        .bg(ui.island)
        .border_1()
        .border_color(ui.divider)
        .opacity(0.72)
        .hover(|toolbar| toolbar.opacity(1.))
        .children(
            buttons
                .into_iter()
                .enumerate()
                .map(|(index, (choice, name, label))| {
                    let action: Box<dyn gpui::Action> = match choice {
                        MarkdownLayout::Editor => Box::new(EditorOnly),
                        MarkdownLayout::Split => Box::new(EditorAndPreview),
                        MarkdownLayout::Preview => Box::new(PreviewOnly),
                    };
                    ui::toggle_button(
                        ElementId::NamedInteger("markdown-layout".into(), index as u64),
                        name,
                        layout == choice,
                        ui,
                    )
                    .tooltip(ui::tooltip(
                        tr(label),
                        ui::shortcut_for(action.as_ref(), cx),
                    ))
                    .on_click(cx.listener(move |editor, _, window, cx| {
                        set_layout(editor, choice, window, cx)
                    }))
                }),
        )
}

/// What the preview's items need to draw themselves.
struct Ctx {
    doc: Rc<Doc>,
    dir: Option<PathBuf>,
    width: Rc<Cell<f32>>,
    list: ListState,
    anchors: HashMap<String, usize>,
    editor: WeakEntity<Editor>,
}

impl Ctx {
    /// The width of the column in the last frame.
    fn column(&self) -> f32 {
        (self.width.get() - 2. * INSET_X).clamp(120., COLUMN_MAX_WIDTH)
    }

    /// Follows a link of the document.
    fn follow(&self, destination: &Destination, window: &mut Window, cx: &mut App) {
        match destination {
            Destination::Web(url) => plugin_links::open_url(url, cx),
            Destination::Anchor(anchor) => {
                if let Some(&index) = self.anchors.get(anchor) {
                    self.list.scroll_to(ListOffset {
                        item_ix: index,
                        offset_in_item: px(0.),
                    });
                    if let Some(editor) = self.editor.upgrade() {
                        editor.update(cx, |_, cx| cx.notify());
                    }
                }
            }
            Destination::File(path, _) => {
                let Some(workspace) = window.root::<Workspace>().flatten() else {
                    return;
                };
                let path = path.clone();
                workspace.update(cx, |workspace, cx| {
                    workspace.open_file(path, true, window, cx)
                });
            }
        }
    }
}

/// The preview: a list of the document's blocks in a readable column. In Editor and Preview
/// (`follow`), it keeps the block at the editor's top line at its top.
fn render_preview(
    editor: &mut Editor,
    follow: bool,
    _window: &mut Window,
    cx: &mut Context<Editor>,
) -> AnyElement {
    let ui = Theme::ui(cx);
    let dir = editor
        .document
        .path()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    let Some(state) = &editor.markdown else {
        return div().into_any_element();
    };
    let ctx = Rc::new(Ctx {
        doc: state.doc.clone(),
        dir,
        width: state.width.clone(),
        list: state.list.clone(),
        anchors: anchors(&state.doc),
        editor: cx.entity().downgrade(),
    });
    // Before the list lays its items out: the column's width for the pictures and, following the
    // editor, the scroll — after the text's element has scrolled in this very frame.
    let sync = {
        let ctx = ctx.clone();
        let synced = state.synced.clone();
        canvas(
            move |bounds, _, cx| {
                ctx.width.set(f32::from(bounds.size.width));
                if !follow {
                    return;
                }
                let Some(editor) = ctx.editor.upgrade() else {
                    return;
                };
                let editor = editor.read(cx);
                let line_height = editor
                    .layout
                    .as_ref()
                    .map_or(theme::LINE_HEIGHT, |layout| f32::from(layout.line_height));
                // The display row at the top, and the line it is in with how far into it (soft
                // wraps, part 10.5: a line may take several rows).
                let top_row = editor.scroll.y.max(0.) / line_height;
                let wrap = &editor.wrap;
                let line = wrap.row_to_line(top_row as usize);
                let into = (top_row - wrap.line_to_row(line) as f32) / wrap.line_rows(line) as f32;
                let top = line as f32 + into.clamp(0., 0.999);
                if synced.get().is_some_and(|last| (last - top).abs() < 0.001) {
                    return;
                }
                synced.set(Some(top));
                let rows = wrap.row_count();
                let visible = editor
                    .layout
                    .as_ref()
                    .map_or(0, |layout| layout.visible_lines());
                // The editor shows the end of the text: so does the preview.
                if top_row > 0. && top_row as usize + visible >= rows {
                    ctx.list.scroll_to(ListOffset {
                        item_ix: ctx.doc.blocks.len(),
                        offset_in_item: px(0.),
                    });
                    return;
                }
                if let Some((index, into)) = block_at_line(&ctx.doc.blocks, top) {
                    let height = ctx
                        .list
                        .bounds_for_item(index)
                        .map_or(px(0.), |bounds| bounds.size.height);
                    ctx.list.scroll_to(ListOffset {
                        item_ix: index,
                        offset_in_item: height * into,
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
    };
    let items = {
        let ctx = ctx.clone();
        list(state.list.clone(), move |index, _, cx| {
            render_item(&ctx, index, cx)
        })
    };
    div()
        .flex_1()
        .min_w_0()
        .size_full()
        .relative()
        .bg(ui.island)
        .font_family(theme::UI_FONT)
        .text_size(px(TEXT_SIZE))
        .line_height(px(TEXT_LINE))
        .text_color(ui.foreground)
        .child(sync)
        .child(items.size_full())
        .into_any_element()
}

/// The space above and under a block: what GitHub's margins come to, collapsed between blocks.
fn spacing(blocks: &[DocBlock], index: usize) -> (f32, f32) {
    let above = |kind: &Kind| -> f32 {
        match kind {
            Kind::Heading { level: 1 | 2, .. } => 26.,
            Kind::Heading { .. } => 20.,
            Kind::Rule => 10.,
            _ => 0.,
        }
    };
    let below = |kind: &Kind, next: Option<&Kind>| -> f32 {
        match (kind, next) {
            (Kind::Item { .. }, Some(Kind::Item { .. })) => 4.,
            (Kind::Heading { .. }, _) => 12.,
            (Kind::Rule, _) => 18.,
            _ => 14.,
        }
    };
    let kind = &blocks[index].kind;
    let top = if index == 0 {
        INSET_TOP
    } else {
        let previous = below(&blocks[index - 1].kind, Some(kind));
        (above(kind) - previous).max(0.)
    };
    let bottom = if index + 1 == blocks.len() {
        INSET_BOTTOM
    } else {
        below(kind, blocks.get(index + 1).map(|block| &block.kind))
    };
    (top, bottom)
}

/// One item of the preview's list: a block in the column.
fn render_item(ctx: &Rc<Ctx>, index: usize, cx: &mut App) -> AnyElement {
    let Some(block) = ctx.doc.blocks.get(index) else {
        return div().into_any_element();
    };
    let theme = Theme::get(cx);
    let (top, bottom) = spacing(&ctx.doc.blocks, index);
    div()
        .id(ElementId::NamedInteger(
            "markdown-block".into(),
            index as u64,
        ))
        .w_full()
        .px(px(INSET_X))
        .flex()
        .justify_center()
        .child(
            div()
                .w_full()
                .max_w(px(COLUMN_MAX_WIDTH))
                .pt(px(top))
                .pb(px(bottom))
                .child(render_block(block, 0, ctx, theme)),
        )
        .into_any_element()
}

/// A block (a quote's blocks too); `id` — the base of its elements' ids, unique within the list
/// item (the item has an id of its own).
fn render_block(block: &DocBlock, id: u64, ctx: &Rc<Ctx>, theme: &Theme) -> AnyElement {
    let ui = theme.ui;
    let centered = block.centered;
    let text = |spans: &[Span], color: Hsla, weight: FontWeight, n: u64| {
        doc_text(element_id(id + n), spans, color, weight, ctx, ui)
    };
    let element = match &block.kind {
        Kind::Heading { level, spans, .. } => {
            let (size, line) = match level {
                1 => (27., 36.),
                2 => (21., 30.),
                3 => (17., 25.),
                4 => (15., 22.),
                _ => (TEXT_SIZE, 21.),
            };
            let color = if *level >= 6 {
                ui.text_muted
            } else {
                ui.foreground
            };
            div()
                .text_size(px(size))
                .line_height(px(line))
                .when(*level <= 2, |heading| {
                    heading.pb(px(7.)).border_b_1().border_color(ui.divider)
                })
                .child(text(spans, color, FontWeight::SEMIBOLD, 0))
                .into_any_element()
        }
        Kind::Paragraph(spans) => div()
            .child(text(spans, ui.foreground, FontWeight::NORMAL, 0))
            .into_any_element(),
        Kind::Item {
            depth,
            marker,
            spans,
        } => {
            let marker = match marker {
                Marker::Bullet => div()
                    .w(px(16.))
                    .flex_none()
                    .text_color(ui.dim)
                    .child(match depth % 3 {
                        0 => "•",
                        1 => "◦",
                        _ => "▪",
                    })
                    .into_any_element(),
                Marker::Number(number) => div()
                    .min_w(px(20.))
                    .flex_none()
                    .text_color(ui.text_muted)
                    .child(number.clone())
                    .into_any_element(),
                Marker::Task(checked) => {
                    let line = block.lines.start;
                    let editor = ctx.editor.clone();
                    div()
                        .w(px(22.))
                        .h(px(TEXT_LINE))
                        .flex_none()
                        .flex()
                        .items_center()
                        .child(
                            ui::checkbox(element_id(id + 900), CheckState::from_bool(*checked), ui)
                                .on_click(move |_, _, cx| {
                                    if let Some(editor) = editor.upgrade() {
                                        editor
                                            .update(cx, |editor, cx| toggle_task(editor, line, cx));
                                    }
                                }),
                        )
                        .into_any_element()
                }
            };
            let done = matches!(marker_of(block), Some(Marker::Task(true)));
            div()
                .flex()
                .gap(px(6.))
                .pl(px(*depth as f32 * LIST_INDENT))
                .child(marker)
                .child(div().flex_1().min_w_0().child(text(
                    spans,
                    if done { ui.text_muted } else { ui.foreground },
                    FontWeight::NORMAL,
                    0,
                )))
                .into_any_element()
        }
        Kind::Quote(blocks) => div()
            .pl(px(14.))
            .border_l_2()
            .border_color(UiColors::tint(ui.foreground, 0.16))
            .text_color(ui.text_muted)
            .flex()
            .flex_col()
            .gap(px(10.))
            .children(
                blocks
                    .iter()
                    .enumerate()
                    .map(|(n, inner)| render_block(inner, nested_id(id, n), ctx, theme)),
            )
            .into_any_element(),
        Kind::Alert { kind, blocks } => {
            let color = kind.color(&ui);
            div()
                .pl(px(14.))
                .py(px(2.))
                .border_l_2()
                .border_color(color)
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.))
                        .text_color(color)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(icon(kind.icon(), color).size(px(15.)))
                        .child(kind.title()),
                )
                .children(
                    blocks
                        .iter()
                        .enumerate()
                        .map(|(n, inner)| render_block(inner, nested_id(id, n), ctx, theme)),
                )
                .into_any_element()
        }
        Kind::Code(code) => {
            markdown::chat_code_block(code, theme, element_id(id + 1000), None, None)
        }
        Kind::Table(table) => render_table(table, id, ctx, ui),
        Kind::Rule => div().h(px(1.)).bg(ui.divider).into_any_element(),
        Kind::Images(images) => render_images(images, centered, id, ctx, ui),
    };
    // Centered text keeps its block's width (a heading's rule runs across the column); a row of
    // pictures centers itself.
    if centered {
        div()
            .w_full()
            .text_center()
            .child(element)
            .into_any_element()
    } else {
        element
    }
}

fn marker_of(block: &DocBlock) -> Option<&Marker> {
    match &block.kind {
        Kind::Item { marker, .. } => Some(marker),
        _ => None,
    }
}

fn element_id(n: u64) -> ElementId {
    ElementId::NamedInteger("markdown".into(), n)
}

/// The id base of the `n`-th block inside a quote whose base is `id`: a range of its own.
fn nested_id(id: u64, n: usize) -> u64 {
    id + ((n as u64 + 1) << 20)
}

/// Toggles the box of the task on `line`: `[ ]` ↔ `[x]` in the text (one undo step).
fn toggle_task(editor: &mut Editor, line: usize, cx: &mut Context<Editor>) {
    let text = editor.document.text();
    if line >= text.len_lines() {
        return;
    }
    let Some((column, replacement)) = task_toggle(&text.line(line).to_string()) else {
        return;
    };
    let at = editor.position(line, column);
    let keep_scroll = editor.scroll;
    editor.replace_ranges(vec![(at..at + 1, replacement.to_string())], cx);
    // The box changes in place: the editor doesn't jump to its cursor.
    editor.autoscroll = None;
    editor.scroll = keep_scroll;
}

/// The edit that toggles a task item's box on its line: the column (in characters) of the mark
/// inside `[ ]` and what replaces it.
fn task_toggle(line: &str) -> Option<(usize, &'static str)> {
    let trimmed = line.trim_start();
    let (_, rest) = markdown::list_marker(trimmed)?;
    task(rest)?;
    let open = line.len() - rest.len();
    let replacement = match line[open + 1..].chars().next()? {
        ' ' => "x",
        _ => " ",
    };
    Some((line[..open + 1].chars().count(), replacement))
}

/// Text whose links click: web links open in the browser, files in Flux, anchors scroll the
/// preview, plugins' links go to their plugins.
fn doc_text(
    id: ElementId,
    spans: &[Span],
    color: Hsla,
    weight: FontWeight,
    ctx: &Rc<Ctx>,
    ui: UiColors,
) -> AnyElement {
    let (text, runs) = markdown::styled_parts(spans, color, ui, weight, false, None);
    let mut targets: Vec<(Range<usize>, Target)> = Vec::new();
    let mut code = Vec::new();
    let mut start = 0;
    for span in spans.iter().filter(|span| !span.text.is_empty()) {
        let range = start..start + span.text.len();
        start = range.end;
        if let Some(target) = &span.target {
            if let Some(destination) = resolve(target, ctx.dir.as_deref()) {
                targets.push((range, Target::Go(destination)));
            }
        } else if span.style.code || span.style.link || span.style.kbd {
            code.push(range);
        } else {
            for found in plugin_links::web_links(&span.text) {
                let url = span.text[found.clone()].to_string();
                targets.push((
                    range.start + found.start..range.start + found.end,
                    Target::Go(Destination::Web(url)),
                ));
            }
        }
    }
    let taken: Vec<Range<usize>> = targets
        .iter()
        .map(|(range, _)| range.clone())
        .chain(code)
        .collect();
    targets.extend(
        plugin_links::plugin_targets(&text, &taken)
            .into_iter()
            .filter_map(|(range, target)| match target {
                plugin_links::Target::Plugin(link) => Some((range, Target::Plugin(link))),
                plugin_links::Target::Url(_) => None,
            }),
    );
    targets.sort_by_key(|(range, _)| range.start);
    let text = SharedString::from(text);
    if targets.is_empty() {
        return StyledText::new(text).with_runs(runs).into_any_element();
    }
    let ranges: Vec<Range<usize>> = targets.iter().map(|(range, _)| range.clone()).collect();
    let runs = plugin_links::link_runs(runs, &ranges, ui.accent_text);
    let targets = Rc::new(targets);
    let clicked = targets.clone();
    let ctx = ctx.clone();
    InteractiveText::new(id, StyledText::new(text).with_runs(runs))
        .on_click(ranges, move |index, window, cx| match clicked.get(index) {
            Some((_, Target::Go(destination))) => ctx.follow(destination, window, cx),
            Some((_, Target::Plugin(link))) => {
                let store = plugin_links::store_of(window, cx);
                plugin_links::activate(link, store, cx);
            }
            None => {}
        })
        .tooltip(move |index, window, cx| {
            let (_, target) = targets.iter().find(|(range, _)| range.contains(&index))?;
            match target {
                Target::Go(Destination::Web(url)) => Some(plugin_links::url_tooltip(url, cx)),
                Target::Go(Destination::File(path, _)) => {
                    Some(plugin_links::url_tooltip(&path.to_string_lossy(), cx))
                }
                Target::Go(Destination::Anchor(_)) => None,
                Target::Plugin(link) => {
                    let store = plugin_links::store_of(window, cx);
                    plugin_links::tooltip_view(link, store, cx)
                }
            }
        })
        .into_any_element()
}

/// What a clickable range of the preview's text does.
enum Target {
    Go(Destination),
    Plugin(Link),
}

/// A table: the header on a tinted background, rows between thin lines.
fn render_table(table: &Table, id: u64, ctx: &Rc<Ctx>, ui: UiColors) -> AnyElement {
    let columns = table.align.len().max(1) as u64;
    let header_empty = table
        .header
        .iter()
        .all(|cell| cell.iter().all(|span| span.text.trim().is_empty()));
    let row = |row_index: u64, cells: &[Vec<Span>], header: bool| {
        div()
            .flex()
            .when(header, |row| row.bg(UiColors::tint(ui.foreground, 0.05)))
            .children(cells.iter().enumerate().map(|(column, spans)| {
                let align = table.align.get(column).copied().unwrap_or_default();
                let weight = if header {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                };
                let n = 1 + row_index * columns + column as u64;
                // A block, not a flex row: the text takes the cell's width and wraps in it.
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(10.))
                    .py(px(6.))
                    .when(column > 0, |cell| {
                        cell.border_l_1().border_color(ui.divider)
                    })
                    .map(|cell| match align {
                        Align::Left => cell.text_left(),
                        Align::Center => cell.text_center(),
                        Align::Right => cell.text_right(),
                    })
                    .child(doc_text(
                        element_id(id + n),
                        spans,
                        ui.foreground,
                        weight,
                        ctx,
                        ui,
                    ))
            }))
    };
    div()
        .flex()
        .flex_col()
        .rounded(px(RADIUS_SM))
        .border_1()
        .border_color(ui.divider)
        .overflow_hidden()
        // A header of empty cells (`| | |`, a table of two columns without titles) isn't drawn.
        .when(!header_empty, |table_box| {
            table_box.child(row(0, &table.header, true))
        })
        .children(table.rows.iter().enumerate().map(|(index, cells)| {
            row(index as u64 + 1, cells, false).when(index > 0 || !header_empty, |row| {
                row.border_t_1().border_color(ui.divider)
            })
        }))
        .into_any_element()
}

/// A row of pictures: a local file at its size within the column (`width` as written), a picture
/// from the web as its description.
fn render_images(
    images: &[Image],
    centered: bool,
    id: u64,
    ctx: &Rc<Ctx>,
    ui: UiColors,
) -> AnyElement {
    let column = ctx.column();
    div()
        .w_full()
        .flex()
        .flex_wrap()
        .items_end()
        .gap(px(8.))
        .when(centered, |row| row.justify_center())
        .children(images.iter().enumerate().map(|(n, image)| {
            let destination = resolve(&image.src, ctx.dir.as_deref());
            let picture = match &destination {
                Some(Destination::File(path, _)) => {
                    let (width, height) = picture_box(image, column);
                    let alt = SharedString::from(image.alt.clone());
                    img(path.clone())
                        .w(px(width))
                        .when_some(height, |picture, height| picture.h(px(height)))
                        .rounded(px(RADIUS_SM))
                        .with_fallback(move || placeholder(alt.clone(), ui))
                        .into_any_element()
                }
                _ => placeholder(
                    SharedString::from(if image.alt.is_empty() {
                        image.src.clone()
                    } else {
                        image.alt.clone()
                    }),
                    ui,
                ),
            };
            // A click goes where the picture links to, or opens the picture itself.
            let target = image
                .link
                .as_deref()
                .and_then(|link| resolve(link, ctx.dir.as_deref()))
                .or(destination);
            let ctx = ctx.clone();
            div()
                .id(element_id(id + 500 + n as u64))
                .flex_none()
                .max_w_full()
                .when_some(target, |picture, target| {
                    picture
                        .cursor_pointer()
                        .on_click(move |_, window, cx| ctx.follow(&target, window, cx))
                })
                .when(!image.alt.is_empty(), |picture| {
                    picture.tooltip(ui::tooltip(image.alt.clone(), None))
                })
                .child(picture)
        }))
        .into_any_element()
}

/// The size a picture is shown at in a column `column` px wide: its `width` (pixels or a share),
/// otherwise its own, never wider than the column; the height by its proportions (`None` while
/// they are unknown).
pub fn picture_box(image: &Image, column: f32) -> (f32, Option<f32>) {
    let own = image.size;
    let wanted = match image.width {
        Some(Dimension::Percent(percent)) => column * percent / 100.,
        Some(Dimension::Pixels(pixels)) => pixels,
        None => own.map_or(column, |(width, _)| width),
    };
    let width = wanted.min(column).max(1.);
    let height = match own {
        Some((own_width, own_height)) if own_width > 0. => Some(width * own_height / own_width),
        _ => image.height,
    };
    (width, height)
}

/// A picture that can't be shown (from the web, missing): its description in a quiet frame.
fn placeholder(label: SharedString, ui: UiColors) -> AnyElement {
    div()
        .h(px(24.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(RADIUS_SM))
        .border_1()
        .border_color(ui.divider)
        .bg(UiColors::tint(ui.foreground, 0.04))
        .text_size(px(theme::TEXT_SM))
        .text_color(ui.text_muted)
        .child(icon(IconName::Globe, ui.dim).size(px(13.)))
        .child(label)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(doc: &Doc) -> Vec<&'static str> {
        doc.blocks
            .iter()
            .map(|block| match &block.kind {
                Kind::Heading { .. } => "heading",
                Kind::Paragraph(_) => "paragraph",
                Kind::Item { .. } => "item",
                Kind::Quote(_) => "quote",
                Kind::Alert { .. } => "alert",
                Kind::Code(_) => "code",
                Kind::Table(_) => "table",
                Kind::Rule => "rule",
                Kind::Images(_) => "images",
            })
            .collect()
    }

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|span| span.text.as_str()).collect()
    }

    #[test]
    fn blocks_know_their_source_lines() {
        let doc = parse_document(
            "# Title\n\nFirst line\nsecond line\n\n- one\n- two\n  more\n\n```rust\nfn x() {}\n```\n\n---\n| a | b |\n|---|---|\n| 1 | 2 |\n",
        );
        assert_eq!(
            kinds(&doc),
            [
                "heading",
                "paragraph",
                "item",
                "item",
                "code",
                "rule",
                "table"
            ]
        );
        let lines: Vec<Range<usize>> = doc.blocks.iter().map(|block| block.lines.clone()).collect();
        assert_eq!(lines, [0..1, 2..4, 5..6, 6..8, 9..12, 13..14, 14..17]);
    }

    #[test]
    fn the_editor_line_maps_to_its_block() {
        let doc = parse_document("# A\n\npara one\npara two\n\n## B\n\ntext\n");
        // Line 2 starts the paragraph; line 3 is its middle; line 4 (blank) belongs to the next.
        assert_eq!(block_at_line(&doc.blocks, 0.), Some((0, 0.)));
        assert_eq!(block_at_line(&doc.blocks, 2.), Some((1, 0.)));
        assert_eq!(block_at_line(&doc.blocks, 3.), Some((1, 0.5)));
        assert_eq!(block_at_line(&doc.blocks, 4.), Some((2, 0.)));
        assert_eq!(block_at_line(&doc.blocks, 7.), Some((3, 0.)));
        assert_eq!(block_at_line(&doc.blocks, 40.), Some((3, 1.)));
        assert_eq!(block_at_line(&[], 1.), None);
    }

    #[test]
    fn headings_get_github_anchors() {
        let doc = parse_document("# Get started\n## Keys & shortcuts\n# Get started\nTitle\n===\n");
        let anchors: Vec<String> = doc
            .blocks
            .iter()
            .filter_map(|block| match &block.kind {
                Kind::Heading { anchor, level, .. } => Some(format!("{level}:{anchor}")),
                _ => None,
            })
            .collect();
        assert_eq!(
            anchors,
            [
                "1:get-started",
                "2:keys--shortcuts",
                "1:get-started-1",
                "1:title"
            ]
        );
    }

    #[test]
    fn readme_html_becomes_centered_pictures_and_keys() {
        let doc = parse_document(
            "<div align=\"center\">\n\n<img src=\"docs/icon.png\" width=\"112\" alt=\"Flux\">\n\n# Flux\n\n![status](https://img.shields.io/x) ![l](https://img.shields.io/y)\n\n</div>\n\n<p align=\"center\">\n  <img src=\"docs/editor.png\" alt=\"Editor\" width=\"100%\">\n</p>\n\nPress <kbd>⌘</kbd><kbd>P</kbd> to find a file.\n",
        );
        assert_eq!(
            kinds(&doc),
            ["images", "heading", "images", "images", "paragraph"]
        );
        assert!(doc.blocks[..4].iter().all(|block| block.centered));
        assert!(!doc.blocks[4].centered);
        let Kind::Images(icon) = &doc.blocks[0].kind else {
            unreachable!()
        };
        assert_eq!(icon[0].src, "docs/icon.png");
        assert_eq!(icon[0].width, Some(Dimension::Pixels(112.)));
        let Kind::Images(badges) = &doc.blocks[2].kind else {
            unreachable!()
        };
        assert_eq!(badges.len(), 2);
        let Kind::Images(editor) = &doc.blocks[3].kind else {
            unreachable!()
        };
        assert_eq!(editor[0].width, Some(Dimension::Percent(100.)));
        let Kind::Paragraph(spans) = &doc.blocks[4].kind else {
            unreachable!()
        };
        let keys: Vec<&str> = spans
            .iter()
            .filter(|span| span.style.kbd)
            .map(|span| span.text.trim_matches('\u{2009}'))
            .collect();
        assert_eq!(keys, ["⌘", "P"]);
        assert_eq!(
            text(spans).replace('\u{2009}', ""),
            "Press ⌘P to find a file."
        );
    }

    #[test]
    fn same_line_html_blocks_close_their_region() {
        let doc = parse_document(
            "<p align=\"center\"><img src=\"a.png\"></p>\n\nText after.\n\n<h1 align=\"center\">Big</h1>\n",
        );
        assert_eq!(kinds(&doc), ["images", "paragraph", "heading"]);
        assert!(doc.blocks[0].centered);
        assert!(!doc.blocks[1].centered);
        assert!(doc.blocks[2].centered);
    }

    #[test]
    fn alerts_quotes_and_tasks() {
        let doc = parse_document(
            "> [!NOTE]\n> Flux is early.\n\n> A quote\nlazily continued\n> - item\n\n- [ ] todo\n- [x] done\n  - nested\n",
        );
        assert_eq!(kinds(&doc), ["alert", "quote", "item", "item", "item"]);
        let Kind::Alert { kind, blocks } = &doc.blocks[0].kind else {
            unreachable!()
        };
        assert_eq!(*kind, AlertKind::Note);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lines, 1..2);
        let Kind::Quote(inner) = &doc.blocks[1].kind else {
            unreachable!()
        };
        assert_eq!(inner.len(), 2);
        assert_eq!(doc.blocks[1].lines, 3..6);
        let markers: Vec<(usize, Marker)> = doc.blocks[2..]
            .iter()
            .map(|block| match &block.kind {
                Kind::Item { depth, marker, .. } => (*depth, marker.clone()),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(
            markers,
            [
                (0, Marker::Task(false)),
                (0, Marker::Task(true)),
                (1, Marker::Bullet)
            ]
        );
    }

    #[test]
    fn comments_front_matter_and_setext() {
        let doc = parse_document(
            "---\ntitle: x\n---\n<!-- hidden\nstill hidden -->\nHeading\n-------\n\n<details>\n<summary>More</summary>\n\nInside\n\n</details>\n",
        );
        assert_eq!(kinds(&doc), ["code", "heading", "paragraph", "paragraph"]);
        assert_eq!(doc.blocks[0].lines, 0..3);
        assert_eq!(doc.blocks[1].lines, 5..7);
        let Kind::Paragraph(summary) = &doc.blocks[2].kind else {
            unreachable!()
        };
        assert!(summary.iter().all(|span| span.style.bold));
    }

    #[test]
    fn links_resolve_against_the_document() {
        let dir = Path::new("/repo/docs");
        assert_eq!(
            resolve("https://example.com", Some(dir)),
            Some(Destination::Web("https://example.com".into()))
        );
        assert_eq!(
            resolve("#Get-Started", Some(dir)),
            Some(Destination::Anchor("get-started".into()))
        );
        assert_eq!(
            resolve("../README.md#keys", Some(dir)),
            Some(Destination::File(
                PathBuf::from("/repo/README.md"),
                Some("keys".into())
            ))
        );
        assert_eq!(
            resolve("images/my%20shot.png", Some(dir)),
            Some(Destination::File(
                PathBuf::from("/repo/docs/images/my shot.png"),
                None
            ))
        );
        assert_eq!(resolve("data:image/png;base64,xx", Some(dir)), None);
        assert_eq!(resolve("guide.md", None), None);
    }

    #[test]
    fn pictures_fit_the_column() {
        let mut image = Image {
            src: "a.png".into(),
            alt: String::new(),
            width: None,
            height: None,
            link: None,
            size: Some((2400., 1600.)),
        };
        assert_eq!(picture_box(&image, 600.), (600., Some(400.)));
        image.width = Some(Dimension::Pixels(112.));
        image.size = Some((512., 512.));
        assert_eq!(picture_box(&image, 600.), (112., Some(112.)));
        image.width = Some(Dimension::Percent(50.));
        assert_eq!(picture_box(&image, 600.), (300., Some(300.)));
        image.size = None;
        image.height = Some(40.);
        assert_eq!(picture_box(&image, 600.), (300., Some(40.)));
    }

    #[test]
    fn an_edit_splices_only_the_changed_blocks() {
        let mut state =
            MarkdownState::new("# A\n\none\n\ntwo\n\nthree\n", None, MarkdownLayout::Split);
        assert_eq!(state.list.item_count(), 4);
        state.replace(parse_document(
            "# A\n\none\n\ntwo, longer\nand more\n\nthree\n",
        ));
        assert_eq!(state.list.item_count(), 4);
        assert_eq!(state.doc.blocks[3].lines, 7..8);
        state.replace(parse_document("# A\n\none\n"));
        assert_eq!(state.list.item_count(), 2);
    }

    #[test]
    fn task_boxes_toggle_in_their_line() {
        assert_eq!(task_toggle("- [ ] todo"), Some((3, "x")));
        assert_eq!(task_toggle("  * [x] done"), Some((5, " ")));
        assert_eq!(task_toggle("1. [X] numbered"), Some((4, " ")));
        assert_eq!(task_toggle("- plain [ ] item"), None);
        assert_eq!(task_toggle("[ ] not a list"), None);
    }

    #[test]
    fn markdown_files_by_extension() {
        assert!(is_markdown(Path::new("README.md")));
        assert!(is_markdown(Path::new("notes.MARKDOWN")));
        assert!(!is_markdown(Path::new("main.rs")));
        assert!(!is_markdown(Path::new("Makefile")));
    }

    #[test]
    fn the_layout_default_persists() {
        let settings = crate::settings::Settings::default();
        assert_eq!(settings.markdown_layout, MarkdownLayout::Split);
    }
}
