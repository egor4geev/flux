//! Documentation from language servers (hover, completion item docs): a Markdown subset parsed into
//! blocks and drawn with the design system. Servers send small, regular Markdown — signatures in
//! fenced code blocks, paragraphs with `code`, emphasis and links, lists, rules — so this is not a
//! CommonMark implementation: whatever it doesn't recognize stays plain text.
//!
//! Code blocks are highlighted with flux-syntax ([`highlight`]) off the UI thread: by the fence's
//! language, otherwise by the document's.
//!
//! The Claude chat (stage 9) draws Claude's answers with it too: GitHub-style tables, and code
//! blocks with a copy button ([`render_copyable`]).
//!
//! Links click (part 10.3): a web link (`[text](https://…)`, `<https://…>`, a bare `https://…`)
//! opens in the browser, with its address on hover; a plugin's link (`TRACK-142`) goes to its
//! plugin, with its tooltip ([`crate::plugin_links`]).

use std::cell::Cell;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use flux_core::Rope;
use flux_lsp::lsp_types::{MarkupContent, MarkupKind};
use flux_syntax::{
    HighlightMap, HighlightSpan, Language, Syntax, language_by_name, language_for_path,
};
use gpui::{
    AnyElement, App, ClipboardItem, ElementId, FontStyle, FontWeight, Hsla, StyledText, TextRun,
    div, font, prelude::*, px,
};

use crate::display::{display_line, text_runs};
use crate::theme::{self, Theme, UiColors};
use crate::plugin_links::Target;
use crate::ui::{self, RADIUS_MD, RADIUS_SM};

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Paragraph(Vec<Span>),
    Heading(Vec<Span>),
    /// A list item: nesting depth (0 is the top level), the marker ("•", "1."), the text.
    Item {
        depth: usize,
        marker: String,
        spans: Vec<Span>,
    },
    Quote(Vec<Span>),
    Code(CodeBlock),
    Rule,
    /// A GitHub-style table: `| a | b |`, a `|---|:---:|` row, then rows.
    Table(Table),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub header: Vec<Vec<Span>>,
    pub align: Vec<Align>,
    pub rows: Vec<Vec<Vec<Span>>>,
}

/// A table column's alignment, from the colons of its separator (`:---`, `:---:`, `---:`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// A piece of text with one style; a paragraph is a sequence of them.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub text: String,
    pub style: SpanStyle,
    /// A link's target (`[text](target)`, `<target>`): a web link opens when clicked.
    pub target: Option<String>,
}

impl Span {
    /// Text without markup.
    pub fn plain(text: &str) -> Self {
        Span {
            text: text.to_string(),
            style: SpanStyle::default(),
            target: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpanStyle {
    pub code: bool,
    pub bold: bool,
    pub italic: bool,
    /// Link text: drawn in the accent color, the target is not shown (it is the span's `target`).
    pub link: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodeBlock {
    /// The first word of the fence's info string, lowercased ("rust"); `None` without one.
    pub language: Option<String>,
    /// Without the final line break.
    pub text: String,
    /// Highlight spans of each line, filled by [`highlight`]; empty without a known language.
    pub highlights: Vec<Vec<HighlightSpan>>,
}

/// Parses Markdown into blocks.
pub fn parse(markdown: &str) -> Vec<Block> {
    let mut parser = BlockParser::default();
    let mut lines = markdown.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let indent = indent_width(line);
        if let Some(align) = lines
            .peek()
            .filter(|_| trimmed.contains('|'))
            .and_then(|next| table_separator(next))
            .filter(|align| align.len() == table_cells(trimmed).len())
        {
            parser.close();
            lines.next();
            let header = table_cells(trimmed);
            let mut rows = Vec::new();
            while let Some(row) = lines.peek().map(|line| line.trim()) {
                if row.is_empty() || !row.contains('|') {
                    break;
                }
                let mut cells = table_cells(row);
                cells.resize(align.len(), String::new());
                rows.push(cells.iter().map(|cell| parse_inline(cell)).collect());
                lines.next();
            }
            parser.blocks.push(Block::Table(Table {
                header: header.iter().map(|cell| parse_inline(cell)).collect(),
                align,
                rows,
            }));
        } else if let Some((fence, info)) = fence_start(trimmed) {
            parser.close();
            let mut code = Vec::new();
            for line in lines.by_ref() {
                if is_fence_end(line.trim_start(), fence) {
                    break;
                }
                code.push(strip_indent(line, indent));
            }
            let language = info
                .split_whitespace()
                .next()
                .map(|word| word.trim_matches(['{', '}', '.']).to_ascii_lowercase())
                .filter(|word| !word.is_empty());
            parser.blocks.push(Block::Code(CodeBlock {
                language,
                text: code.join("\n"),
                highlights: Vec::new(),
            }));
        } else if trimmed.is_empty() {
            parser.close();
        } else if is_rule(trimmed) {
            parser.close();
            parser.blocks.push(Block::Rule);
        } else if let Some(heading) = heading(trimmed) {
            parser.close();
            parser.blocks.push(Block::Heading(parse_inline(heading)));
        } else if let Some((marker, rest)) = list_marker(trimmed) {
            parser.close();
            parser.open = Open::Item {
                depth: indent / 2,
                marker,
                lines: vec![rest],
            };
        } else if let Some(rest) = trimmed.strip_prefix('>') {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            match &mut parser.open {
                Open::Quote(lines) => lines.push(rest),
                _ => {
                    parser.close();
                    parser.open = Open::Quote(vec![rest]);
                }
            }
        } else {
            match &mut parser.open {
                Open::Paragraph(lines) | Open::Quote(lines) => lines.push(line),
                Open::Item { lines, .. } => lines.push(trimmed),
                Open::None => parser.open = Open::Paragraph(vec![line]),
            }
        }
    }
    parser.close();
    parser.blocks
}

/// Documentation as the server marked it up: Markdown or plain text.
pub fn markup(content: &MarkupContent) -> Vec<Block> {
    match content.kind {
        MarkupKind::Markdown => parse(&content.value),
        MarkupKind::PlainText => plain(&content.value),
    }
}

/// Removes empty paragraphs, and rules at the edges or next to each other (a hover put together
/// from parts often has them).
pub fn tidy(blocks: &mut Vec<Block>) {
    blocks.retain(|block| match block {
        Block::Paragraph(spans) => spans.iter().any(|span| !span.text.trim().is_empty()),
        _ => true,
    });
    let mut previous_rule = true;
    blocks.retain(|block| {
        let rule = *block == Block::Rule;
        let keep = !(rule && previous_rule);
        previous_rule = rule;
        keep
    });
    if blocks.last() == Some(&Block::Rule) {
        blocks.pop();
    }
}

/// Plain text (`plaintext` documentation): paragraphs by blank lines, line breaks kept, no markup.
pub fn plain(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    let mut flush = |paragraph: &mut Vec<&str>| {
        if !paragraph.is_empty() {
            blocks.push(Block::Paragraph(vec![Span {
                text: paragraph.join("\n"),
                style: SpanStyle::default(),
                target: None,
            }]));
            paragraph.clear();
        }
    };
    for line in text.lines() {
        if line.trim().is_empty() {
            flush(&mut paragraph);
        } else {
            paragraph.push(line.trim_end());
        }
    }
    flush(&mut paragraph);
    blocks
}

/// Highlights the code blocks: by the fence's language, otherwise by `fallback` (the language of
/// the document the documentation is about). `scopes` are the theme scopes. Parses synchronously,
/// and may compile a language's query: call it off the UI thread.
pub fn highlight(blocks: &mut [Block], fallback: Option<Arc<Language>>, scopes: &[String]) {
    for block in blocks {
        let Block::Code(code) = block else {
            continue;
        };
        let language = match &code.language {
            Some(info) => fence_language(info),
            None => fallback.clone(),
        };
        let Some(language) = language else {
            continue;
        };
        let text = Rope::from_str(&code.text);
        let mut syntax = Syntax::new(language.clone());
        let Some(job) = syntax.parse_job(&text) else {
            continue;
        };
        let result = if language.is_wasm() {
            // A plugin's grammar parses on a thread of its own: a scanner that never returns
            // leaves that thread stuck, not this one (flux-syntax's watchdog turns the grammar off).
            let (sender, receiver) = std::sync::mpsc::channel();
            job.spawn(move |result| {
                sender.send(result).ok();
            });
            match receiver.recv_timeout(flux_syntax::PARSE_LIMIT) {
                Ok(result) => result,
                Err(_) => continue,
            }
        } else {
            job.run()
        };
        syntax.finish(result);
        let map = HighlightMap::new(&language, scopes);
        code.highlights = syntax.highlight_lines(&text, 0..text.len_lines(), &map);
    }
}

/// The theme's scope names, for [`highlight`] off the UI thread.
pub fn scopes(cx: &App) -> Vec<String> {
    Theme::get(cx)
        .syntax_scopes()
        .into_iter()
        .map(String::from)
        .collect()
}

/// The language of a fence's info string: a language's name ("rust") or alias (a plugin's
/// `aliases`), an extension ("rs", "py"), or a common alias.
fn fence_language(info: &str) -> Option<Arc<Language>> {
    let name = match info {
        "golang" => "go",
        "python3" | "py3" => "python",
        "shell" | "console" | "shellscript" => "bash",
        other => other,
    };
    language_by_name(name).or_else(|| language_for_path(Path::new(&format!("code.{name}"))))
}

// --- Blocks ---

#[derive(Default)]
struct BlockParser<'a> {
    blocks: Vec<Block>,
    open: Open<'a>,
}

#[derive(Default)]
enum Open<'a> {
    #[default]
    None,
    Paragraph(Vec<&'a str>),
    Item {
        depth: usize,
        marker: String,
        lines: Vec<&'a str>,
    },
    Quote(Vec<&'a str>),
}

impl BlockParser<'_> {
    /// Ends the block being collected.
    fn close(&mut self) {
        match std::mem::take(&mut self.open) {
            Open::None => {}
            Open::Paragraph(lines) => self
                .blocks
                .push(Block::Paragraph(parse_inline(&join_lines(&lines)))),
            Open::Item {
                depth,
                marker,
                lines,
            } => self.blocks.push(Block::Item {
                depth,
                marker,
                spans: parse_inline(&join_lines(&lines)),
            }),
            Open::Quote(lines) => self
                .blocks
                .push(Block::Quote(parse_inline(&join_lines(&lines)))),
        }
    }
}

/// The lines of a paragraph as one text: a line break becomes a space, except a hard break (two
/// spaces or a backslash at the end of the line), which stays a line break.
fn join_lines(lines: &[&str]) -> String {
    let mut text = String::new();
    for (i, line) in lines.iter().enumerate() {
        let line = line.trim_start();
        let last = i + 1 == lines.len();
        if let Some(stripped) = line.strip_suffix('\\').filter(|_| !last) {
            text.push_str(stripped);
            text.push('\n');
        } else if line.ends_with("  ") && !last {
            text.push_str(line.trim_end());
            text.push('\n');
        } else {
            text.push_str(line.trim_end());
            if !last {
                text.push(' ');
            }
        }
    }
    text
}

/// Leading whitespace width, a tab counting as 4.
fn indent_width(line: &str) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

/// Removes up to `indent` leading spaces (the fence's own indentation) from a code line.
fn strip_indent(line: &str, indent: usize) -> &str {
    let spaces = line.bytes().take(indent).take_while(|b| *b == b' ').count();
    &line[spaces..]
}

/// "```rust" → the fence ("```") and the info string ("rust").
fn fence_start(line: &str) -> Option<(&str, &str)> {
    let marker = line.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = line.chars().take_while(|c| *c == marker).count();
    if len < 3 {
        return None;
    }
    let (fence, info) = line.split_at(len);
    // A backtick fence's info string can't contain backticks (that is inline code).
    if marker == '`' && info.contains('`') {
        return None;
    }
    Some((fence, info.trim()))
}

fn is_fence_end(line: &str, fence: &str) -> bool {
    let marker = fence.chars().next().unwrap_or('`');
    let len = line.chars().take_while(|c| *c == marker).count();
    len >= fence.len() && line[len..].trim().is_empty()
}

/// The cells of a table row: `| a | b |` → ["a", "b"] (the outer pipes are optional, `\|` is a
/// pipe inside a cell).
fn table_cells(line: &str) -> Vec<String> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let line = line
        .strip_suffix('|')
        .filter(|_| !line.ends_with("\\|"))
        .unwrap_or(line);
    let mut cells = vec![String::new()];
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cells.last_mut().unwrap().push('|');
                chars.next();
            }
            '|' => cells.push(String::new()),
            c => cells.last_mut().unwrap().push(c),
        }
    }
    cells
        .into_iter()
        .map(|cell| cell.trim().to_string())
        .collect()
}

/// A table's separator row (`|---|:---:|---:|`): the columns' alignment.
fn table_separator(line: &str) -> Option<Vec<Align>> {
    let cells = table_cells(line);
    if cells.is_empty() || !line.contains('-') {
        return None;
    }
    cells
        .iter()
        .map(|cell| {
            let dashes = cell.trim_matches(':');
            if dashes.len() < 3 || !dashes.chars().all(|c| c == '-') {
                return None;
            }
            Some(match (cell.starts_with(':'), cell.ends_with(':')) {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                _ => Align::Left,
            })
        })
        .collect()
}

/// `---`, `***`, `___` (spaces allowed between).
fn is_rule(line: &str) -> bool {
    let mut chars = line.chars().filter(|c| !c.is_whitespace());
    let Some(first) = chars.next().filter(|c| matches!(c, '-' | '*' | '_')) else {
        return false;
    };
    let mut count = 1;
    for c in chars {
        if c != first {
            return false;
        }
        count += 1;
    }
    count >= 3
}

fn heading(line: &str) -> Option<&str> {
    let level = line.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &line[level..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some(rest.trim().trim_end_matches('#').trim_end())
}

/// "- item", "* item", "+ item" → ("•", "item"); "1. item", "2) item" → ("1.", "item").
fn list_marker(line: &str) -> Option<(String, &str)> {
    if let Some(rest) = line
        .strip_prefix(['-', '*', '+'])
        .and_then(|rest| rest.strip_prefix(' '))
    {
        return Some(("•".to_string(), rest.trim_start()));
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 9 {
        return None;
    }
    let rest = line[digits..].strip_prefix(['.', ')'])?.strip_prefix(' ')?;
    Some((format!("{}.", &line[..digits]), rest.trim_start()))
}

// --- Inline markup ---

/// Inline markup: `code`, **bold**, *italic*, links (`[text](url)`, `<url>`), backslash escapes,
/// and a few HTML entities.
pub fn parse_inline(text: &str) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    inline(&chars, SpanStyle::default(), &mut spans);
    spans
}

fn inline(chars: &[char], style: SpanStyle, spans: &mut Vec<Span>) {
    let mut text = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if chars.get(i + 1).is_some_and(char::is_ascii_punctuation) => {
                text.push(chars[i + 1]);
                i += 2;
            }
            '`' => {
                let run = run_length(chars, i, '`');
                match find_backticks(chars, i + run, run) {
                    Some(end) => {
                        push_span(spans, &mut text, style);
                        let code: String = chars[i + run..end]
                            .iter()
                            .map(|&c| if c == '\n' { ' ' } else { c })
                            .collect();
                        // One space on both sides is padding: `` ` a ` `` is "a".
                        let padded = code
                            .strip_prefix(' ')
                            .and_then(|code| code.strip_suffix(' '))
                            .filter(|inner| !inner.trim().is_empty())
                            .map(str::to_string);
                        let code = padded.unwrap_or(code);
                        spans.push(Span {
                            text: code,
                            style: SpanStyle {
                                code: true,
                                ..style
                            },
                            target: None,
                        });
                        i = end + run;
                    }
                    None => {
                        text.extend(std::iter::repeat_n('`', run));
                        i += run;
                    }
                }
            }
            '*' | '_' => {
                let run = run_length(chars, i, c).min(3);
                match emphasis_end(chars, i, run) {
                    Some(end) => {
                        push_span(spans, &mut text, style);
                        let inner = SpanStyle {
                            bold: style.bold || run >= 2,
                            italic: style.italic || run != 2,
                            ..style
                        };
                        inline(&chars[i + run..end], inner, spans);
                        i = end + run;
                    }
                    None => {
                        let all = run_length(chars, i, c);
                        text.extend(std::iter::repeat_n(c, all));
                        i += all;
                    }
                }
            }
            '[' => match link(chars, i) {
                Some((label, next)) => {
                    push_span(spans, &mut text, style);
                    // The label's own spans, with the target: two links in a row don't merge.
                    let mut label_spans = Vec::new();
                    inline(
                        &chars[label.clone()],
                        SpanStyle {
                            link: true,
                            ..style
                        },
                        &mut label_spans,
                    );
                    let target = link_target(chars, label.end, next);
                    for span in &mut label_spans {
                        span.target = target.clone();
                    }
                    spans.extend(label_spans);
                    i = next;
                }
                None => {
                    text.push('[');
                    i += 1;
                }
            },
            '<' => match autolink(chars, i) {
                Some((url, next)) => {
                    push_span(spans, &mut text, style);
                    let url: String = chars[url].iter().collect();
                    spans.push(Span {
                        text: url.clone(),
                        style: SpanStyle {
                            link: true,
                            ..style
                        },
                        target: Some(url),
                    });
                    i = next;
                }
                None => match html_break(chars, i) {
                    Some(next) => {
                        text.push('\n');
                        i = next;
                    }
                    None => {
                        text.push('<');
                        i += 1;
                    }
                },
            },
            '&' => match entity(chars, i) {
                Some((decoded, next)) => {
                    text.push(decoded);
                    i = next;
                }
                None => {
                    text.push('&');
                    i += 1;
                }
            },
            _ => {
                text.push(c);
                i += 1;
            }
        }
    }
    push_span(spans, &mut text, style);
}

fn push_span(spans: &mut Vec<Span>, text: &mut String, style: SpanStyle) {
    if text.is_empty() {
        return;
    }
    match spans.last_mut() {
        Some(last) if last.style == style && last.target.is_none() => last.text.push_str(text),
        _ => spans.push(Span {
            text: text.clone(),
            style,
            target: None,
        }),
    }
    text.clear();
}

/// The target of a link whose label ends at `close` (its `]`) and which ends at `next`: what is in
/// `(…)` — without a title (`"…"`) and `<…>` — or nothing for `[text][ref]` and `[`code`]`.
fn link_target(chars: &[char], close: usize, next: usize) -> Option<String> {
    if chars.get(close + 1) != Some(&'(') || next < close + 3 {
        return None;
    }
    let inner: String = chars[close + 2..next - 1].iter().collect();
    let inner = inner.trim();
    let target = inner.split_whitespace().next().unwrap_or("");
    let target = target.trim_start_matches('<').trim_end_matches('>');
    (!target.is_empty()).then(|| target.to_string())
}

fn run_length(chars: &[char], start: usize, c: char) -> usize {
    chars[start..].iter().take_while(|&&x| x == c).count()
}

/// The start of the closing backtick run of exactly `len` backticks.
fn find_backticks(chars: &[char], from: usize, len: usize) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == '`' {
            let run = run_length(chars, i, '`');
            if run == len {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

/// Emphasis opened by `run` (1–3) delimiters at `start`: the start of the matching closing run.
/// `_` doesn't work inside words (`snake_case`); a delimiter next to a space doesn't open or
/// close; code spans are skipped while looking.
fn emphasis_end(chars: &[char], start: usize, run: usize) -> Option<usize> {
    let c = chars[start];
    let is_word = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric());
    let before = start.checked_sub(1).map(|i| &chars[i]);
    let after = chars.get(start + run);
    let opens = after.is_some_and(|a| !a.is_whitespace()) && (c == '*' || !is_word(before));
    if !opens {
        return None;
    }
    let mut i = start + run;
    while i < chars.len() {
        match chars[i] {
            '`' => {
                let len = run_length(chars, i, '`');
                i = find_backticks(chars, i + len, len).map_or(i + len, |end| end + len);
            }
            '\\' => i += 2,
            x if x == c => {
                let len = run_length(chars, i, c);
                let before = chars.get(i.wrapping_sub(1));
                let after = chars.get(i + len);
                let closes = i > start + run
                    && before.is_some_and(|b| !b.is_whitespace())
                    && (c == '*' || !is_word(after));
                if closes && len >= run {
                    return Some(i);
                }
                i += len;
            }
            _ => i += 1,
        }
    }
    None
}

/// `[text](target)` or `[text][ref]`: the range of the text and where the link ends. A lone
/// `[text]` counts as a link only around code (`` [`Vec`] ``, a rustdoc intra-doc link); other
/// brackets stay text.
fn link(chars: &[char], start: usize) -> Option<(std::ops::Range<usize>, usize)> {
    let mut depth = 0;
    let mut i = start;
    let close = loop {
        match *chars.get(i)? {
            '\\' => i += 1,
            '`' => {
                let len = run_length(chars, i, '`');
                i = find_backticks(chars, i + len, len).map_or(i, |end| end + len - 1);
            }
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    break i;
                }
            }
            '\n' if depth == 1 && i > start && chars[i - 1] == '\n' => return None,
            _ => {}
        }
        i += 1;
    };
    let label = start + 1..close;
    match chars.get(close + 1).copied() {
        Some('(') => {
            let mut depth = 0;
            let mut j = close + 1;
            loop {
                match *chars.get(j)? {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some((label, j + 1));
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
        }
        Some('[') => {
            let end = chars[close + 2..].iter().position(|&c| c == ']')?;
            Some((label, close + 2 + end + 1))
        }
        _ => {
            let inner = &chars[label.clone()];
            let code = inner.first() == Some(&'`') && inner.last() == Some(&'`');
            code.then_some((label, close + 1))
        }
    }
}

/// `<https://…>`: the URL range and where it ends.
fn autolink(chars: &[char], start: usize) -> Option<(std::ops::Range<usize>, usize)> {
    let end = start + chars[start..].iter().position(|&c| c == '>')?;
    let inner: String = chars[start + 1..end].iter().collect();
    let scheme = inner.split_once(':')?.0;
    let valid = !scheme.is_empty()
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+')
        && !inner.contains(char::is_whitespace);
    valid.then_some((start + 1..end, end + 1))
}

/// `<br>`, `<br/>`, `<br />`: where the tag ends.
fn html_break(chars: &[char], start: usize) -> Option<usize> {
    let end = start + chars[start..].iter().take(8).position(|&c| c == '>')?;
    let tag: String = chars[start + 1..end]
        .iter()
        .filter(|c| !c.is_whitespace())
        .collect();
    matches!(tag.to_ascii_lowercase().as_str(), "br" | "br/").then_some(end + 1)
}

/// `&lt;`, `&gt;`, `&amp;`, `&quot;`, `&#39;`, `&nbsp;`, `&#NN;`, `&#xNN;`.
fn entity(chars: &[char], start: usize) -> Option<(char, usize)> {
    let end = start + chars[start..].iter().take(10).position(|&c| c == ';')?;
    let name: String = chars[start + 1..end].iter().collect();
    let decoded = match name.as_str() {
        "lt" => '<',
        "gt" => '>',
        "amp" => '&',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)?
        }
    };
    Some((decoded, end + 1))
}

// --- Drawing ---

/// The blocks as a column of elements: prose in the UI font, code in the code font with syntax
/// colors. `color` is the color of regular text.
pub fn render(blocks: &[Block], color: Hsla, theme: &Theme) -> AnyElement {
    render_blocks(blocks, color, theme, None, None)
}

/// As [`render`], in the look of Claude's chat: a code block is a card with its language and a copy
/// button in a header, inline code is a lighter pill. `id` keeps the buttons' ids apart from those
/// of other blocks in the window. `marks` — the matches of a search to mark (the chat's ⌘F, part
/// 10.2), in the texts [`block_texts`] lists, in that order.
pub fn render_copyable(
    blocks: &[Block],
    color: Hsla,
    theme: &Theme,
    id: impl Into<gpui::SharedString>,
    marks: Option<&Marks>,
) -> AnyElement {
    render_blocks(blocks, color, theme, Some(id.into()), marks)
}

/// The texts the blocks draw, in the order they are drawn (tables row by row, code blocks as their
/// lines): what a search counts its matches in, so [`Marks`] mark the same ones.
pub fn block_texts(blocks: &[Block]) -> Vec<String> {
    let spans_text = |spans: &[Span]| spans.iter().map(|span| span.text.as_str()).collect::<String>();
    let mut texts = Vec::new();
    for block in blocks {
        match block {
            Block::Paragraph(spans) | Block::Heading(spans) | Block::Quote(spans) => {
                texts.push(spans_text(spans))
            }
            Block::Item { spans, .. } => texts.push(spans_text(spans)),
            Block::Code(code) => texts.push(code.text.clone()),
            Block::Table(table) => {
                for row in std::iter::once(&table.header).chain(&table.rows) {
                    texts.extend(row.iter().map(|cell| spans_text(cell)));
                }
            }
            Block::Rule => {}
        }
    }
    texts
}

/// The matches of a search to mark in the texts of one entry as they are drawn (part 10.2): every
/// match gets `color`; the `active`-th one (counted over the entry's texts in drawing order, from
/// 0) gets `active_color`. `count` goes up as texts are marked.
pub struct Marks {
    /// The query, lowercased; matching is case-insensitive.
    pub needle: Vec<char>,
    pub active: Option<usize>,
    pub count: Cell<usize>,
    pub color: Hsla,
    pub active_color: Hsla,
}

impl Marks {
    /// `runs` (covering `text`) with the matches in `text` on their backgrounds.
    pub fn mark(&self, text: &str, runs: Vec<TextRun>) -> Vec<TextRun> {
        let found = occurrences(text, &self.needle);
        if found.is_empty() {
            return runs;
        }
        let mut marked = Vec::with_capacity(runs.len() + found.len() * 2);
        let mut start = 0;
        let mut next = 0;
        for run in runs {
            let end = start + run.len;
            let mut at = start;
            while at < end {
                // The match touching `at`, if any; else the plain stretch up to the next one.
                while next < found.len() && found[next].end <= at {
                    next += 1;
                    self.count.set(self.count.get() + 1);
                }
                let (until, background) = match found.get(next) {
                    Some(range) if range.start <= at => {
                        let index = self.count.get();
                        let color = if self.active == Some(index) {
                            self.active_color
                        } else {
                            self.color
                        };
                        (range.end.min(end), Some(color))
                    }
                    Some(range) => (range.start.min(end), run.background_color),
                    None => (end, run.background_color),
                };
                marked.push(TextRun {
                    len: until - at,
                    background_color: background,
                    ..run.clone()
                });
                at = until;
            }
            start = end;
        }
        while next < found.len() {
            next += 1;
            self.count.set(self.count.get() + 1);
        }
        marked
    }
}

impl Marks {
    /// The matches in `text` as highlights over the text's own style (a line drawn without runs).
    pub fn highlights(&self, text: &str) -> Vec<(Range<usize>, gpui::HighlightStyle)> {
        occurrences(text, &self.needle)
            .into_iter()
            .map(|range| {
                let index = self.count.get();
                self.count.set(index + 1);
                let color = if self.active == Some(index) {
                    self.active_color
                } else {
                    self.color
                };
                (
                    range,
                    gpui::HighlightStyle {
                        background_color: Some(color),
                        ..Default::default()
                    },
                )
            })
            .collect()
    }
}

/// Where `needle` (lowercase chars) occurs in `text`, case-insensitively, without overlaps: byte
/// ranges of `text`.
pub fn occurrences(text: &str, needle: &[char]) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    if needle.is_empty() {
        return found;
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let same = |a: char, b: char| a == b || a.to_lowercase().eq(b.to_lowercase());
    let mut index = 0;
    while index + needle.len() <= chars.len() {
        if needle
            .iter()
            .enumerate()
            .all(|(offset, &n)| same(chars[index + offset].1, n))
        {
            let start = chars[index].0;
            let end = chars
                .get(index + needle.len())
                .map_or(text.len(), |(byte, _)| *byte);
            found.push(start..end);
            index += needle.len();
        } else {
            index += 1;
        }
    }
    found
}

fn render_blocks(
    blocks: &[Block],
    color: Hsla,
    theme: &Theme,
    copy: Option<gpui::SharedString>,
    marks: Option<&Marks>,
) -> AnyElement {
    let ui = theme.ui;
    // The chat's look (copyable blocks are Claude's text).
    let chat = copy.is_some();
    let texts = TextIds(copy.clone());
    div()
        .flex()
        .flex_col()
        .gap_2()
        .children(blocks.iter().enumerate().map(|(index, block)| {
            let text = |spans: &[Span], color: Hsla, weight: FontWeight| {
                let (text, runs) = styled_parts(spans, color, ui, weight, chat, marks);
                clickable(texts.id(index, 0), text, runs, spans, ui)
            };
            match block {
                Block::Paragraph(spans) => div()
                    .child(text(spans, color, FontWeight::NORMAL))
                    .into_any_element(),
                Block::Heading(spans) => div()
                    .child(text(spans, color, FontWeight::SEMIBOLD))
                    .into_any_element(),
                Block::Item {
                    depth,
                    marker,
                    spans,
                } => div()
                    .flex()
                    .gap_1p5()
                    .pl(px(*depth as f32 * 14.))
                    .child(div().flex_none().text_color(ui.dim).child(marker.clone()))
                    .child(div().flex_1().min_w_0().child(text(spans, color, FontWeight::NORMAL)))
                    .into_any_element(),
                Block::Quote(spans) => div()
                    .pl_2()
                    .border_l_2()
                    .border_color(ui.divider)
                    .child(text(spans, ui.text_muted, FontWeight::NORMAL))
                    .into_any_element(),
                Block::Code(code) => match &copy {
                    Some(prefix) => chat_code_block(
                        code,
                        theme,
                        ElementId::NamedInteger(prefix.clone(), index as u64),
                        marks,
                    ),
                    None => code_block(code, theme),
                },
                Block::Rule => ui::divider(ui).into_any_element(),
                Block::Table(table) => render_table(table, color, ui, chat, marks, &texts, index),
            }
        }))
        .into_any_element()
}

/// The ids of a render's clickable texts: by the block and the cell, under the chat's prefix.
struct TextIds(Option<gpui::SharedString>);

impl TextIds {
    fn id(&self, block: usize, cell: usize) -> ElementId {
        let prefix = match &self.0 {
            Some(prefix) => gpui::SharedString::from(format!("{prefix}-text")),
            None => gpui::SharedString::new_static("markdown-text"),
        };
        ElementId::NamedInteger(prefix, ((block as u64) << 16) | cell as u64)
    }
}

/// Text whose links click: the spans' web links and bare `https://…` addresses (outside code)
/// open in the browser, the plugins' links go to their plugins.
fn clickable(
    id: ElementId,
    text: String,
    runs: Vec<TextRun>,
    spans: &[Span],
    ui: UiColors,
) -> AnyElement {
    let mut targets = Vec::new();
    let mut code = Vec::new();
    let mut start = 0;
    for span in spans.iter().filter(|span| !span.text.is_empty()) {
        let range = start..start + span.text.len();
        start = range.end;
        if let Some(url) = span.target.as_ref().filter(|url| is_web_link(url)) {
            targets.push((range, Target::Url(url.clone())));
        } else if span.style.code || span.style.link {
            code.push(range);
        } else {
            for found in crate::plugin_links::web_links(&span.text) {
                let url = span.text[found.clone()].to_string();
                targets.push((range.start + found.start..range.start + found.end, Target::Url(url)));
            }
        }
    }
    let taken: Vec<Range<usize>> = targets
        .iter()
        .map(|(range, _)| range.clone())
        .chain(code)
        .collect();
    targets.extend(crate::plugin_links::plugin_targets(&text, &taken));
    targets.sort_by_key(|(range, _)| range.start);
    crate::plugin_links::clickable_text(id, text.into(), runs, targets, ui.accent_text)
}

/// A target the browser opens.
fn is_web_link(target: &str) -> bool {
    let lower = target.to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")
}

/// A table: a header row on a tinted background, rows between thin lines, columns of equal share
/// (cells wrap).
fn render_table(
    table: &Table,
    color: Hsla,
    ui: UiColors,
    chat: bool,
    marks: Option<&Marks>,
    texts: &TextIds,
    block: usize,
) -> AnyElement {
    let columns = table.align.len().max(1);
    let row = |row_index: usize, cells: &[Vec<Span>], header: bool| {
        div()
            .flex()
            .when(header, |row| row.bg(UiColors::tint(ui.foreground, 0.05)))
            .children(cells.iter().enumerate().map(|(column, spans)| {
                let align = table.align.get(column).copied().unwrap_or_default();
                div()
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .py_1()
                    .when(column > 0, |cell| {
                        cell.border_l_1().border_color(ui.divider)
                    })
                    .flex()
                    .map(|cell| match align {
                        Align::Left => cell.justify_start(),
                        Align::Center => cell.justify_center(),
                        Align::Right => cell.justify_end(),
                    })
                    .child({
                        let weight = if header {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        };
                        let (text, runs) = styled_parts(spans, color, ui, weight, chat, marks);
                        let id = texts.id(block, 1 + row_index * columns + column);
                        clickable(id, text, runs, spans, ui)
                    })
            }))
    };
    div()
        .flex()
        .flex_col()
        .rounded(px(RADIUS_SM))
        .border_1()
        .border_color(ui.divider)
        .overflow_hidden()
        .child(row(0, &table.header, true))
        .children(table.rows.iter().enumerate().map(|(index, cells)| {
            row(index + 1, cells, false)
                .border_t_1()
                .border_color(ui.divider)
        }))
        .into_any_element()
}

/// A code block of the chat: a card with a header — the language (or "code") and a copy button —
/// over the code.
fn chat_code_block(
    code: &CodeBlock,
    theme: &Theme,
    id: ElementId,
    marks: Option<&Marks>,
) -> AnyElement {
    let ui = theme.ui;
    let text = code.text.clone();
    let language = code
        .language
        .clone()
        .unwrap_or_else(|| crate::i18n::tr("code").to_string());
    div()
        .flex()
        .flex_col()
        .my(px(2.))
        .rounded(px(RADIUS_MD))
        .border_1()
        .border_color(UiColors::tint(ui.foreground, 0.09))
        .bg(UiColors::tint(ui.foreground, 0.035))
        .child(
            div()
                .h(px(28.))
                .pl(px(10.))
                .pr(px(3.))
                .flex()
                .items_center()
                .border_b_1()
                .border_color(ui.divider)
                .text_size(px(theme::TEXT_XS))
                .text_color(ui.dim)
                .child(div().flex_1().child(language))
                .child(
                    ui::icon_button(id, crate::icons::IconName::Copy, ui)
                        .size(px(22.))
                        .tooltip(ui::tooltip(crate::i18n::tr("Copy"), None))
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
                        }),
                ),
        )
        .child(
            div()
                .px(px(10.))
                .py(px(8.))
                .text_size(px(12.5))
                .line_height(px(19.))
                .child(code_text(code, theme, marks)),
        )
        .into_any_element()
}

/// Spans as the text and the runs of styled text; in the chat (`chat`), inline code is a lighter,
/// accent-tinted pill.
fn styled_parts(
    spans: &[Span],
    color: Hsla,
    ui: UiColors,
    weight: FontWeight,
    chat: bool,
    marks: Option<&Marks>,
) -> (String, Vec<TextRun>) {
    let mut text = String::new();
    let mut runs = Vec::with_capacity(spans.len());
    for span in spans.iter().filter(|span| !span.text.is_empty()) {
        let style = span.style;
        let mut run_font = font(if style.code {
            theme::code_font()
        } else {
            theme::UI_FONT
        });
        run_font.weight = if style.bold { FontWeight::BOLD } else { weight };
        if style.italic {
            run_font.style = FontStyle::Italic;
        }
        let code_color = if chat {
            ui.accent_text.blend(UiColors::tint(ui.foreground, 0.55))
        } else {
            color
        };
        runs.push(TextRun {
            len: span.text.len(),
            font: run_font,
            color: if style.link {
                ui.accent_text
            } else if style.code {
                code_color
            } else {
                color
            },
            background_color: style.code.then(|| UiColors::tint(ui.foreground, 0.08)),
            underline: None,
            strikethrough: None,
        });
        text.push_str(&span.text);
    }
    let runs = match marks {
        Some(marks) => marks.mark(&text, runs),
        None => runs,
    };
    (text, runs)
}

fn code_block(code: &CodeBlock, theme: &Theme) -> AnyElement {
    let ui = theme.ui;
    div()
        .px_2()
        .py_1p5()
        .rounded(px(RADIUS_SM))
        .bg(UiColors::tint(ui.foreground, 0.04))
        .font_family(theme::code_font())
        .child(code_text(code, theme, None))
        .into_any_element()
}

/// A code block's text with its syntax colors, in the code font (search matches marked).
fn code_text(code: &CodeBlock, theme: &Theme, marks: Option<&Marks>) -> StyledText {
    let ui = theme.ui;
    let text = Rope::from_str(&code.text);
    let base = TextRun {
        len: 0,
        font: font(theme::code_font()),
        color: ui.foreground,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let mut display = String::new();
    let mut runs = Vec::new();
    for line in 0..text.len_lines() {
        if line > 0 {
            display.push('\n');
            runs.push(TextRun {
                len: 1,
                ..base.clone()
            });
        }
        let (line_text, char_to_byte) = display_line(&text, line);
        let spans = code.highlights.get(line).map_or(&[][..], Vec::as_slice);
        runs.extend(text_runs(spans, &char_to_byte, &base, |h| {
            theme.syntax_style(h)
        }));
        display.push_str(&line_text);
    }
    let runs = match marks {
        Some(marks) => marks.mark(&display, runs),
        None => runs,
    };
    StyledText::new(display).with_runs(runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|span| span.text.as_str()).collect()
    }

    fn span(text: &str, style: SpanStyle) -> Span {
        Span {
            text: text.to_string(),
            style,
            target: None,
        }
    }

    const PLAIN: SpanStyle = SpanStyle {
        code: false,
        bold: false,
        italic: false,
        link: false,
    };
    const CODE: SpanStyle = SpanStyle {
        code: true,
        ..PLAIN
    };

    #[test]
    fn rust_analyzer_hover_splits_into_code_rule_and_prose() {
        let hover = "```rust\nstd::vec\n```\n\n```rust\npub struct Vec<T>\n```\n\n---\n\n\
                     A contiguous growable array type, written as `Vec<T>`.\n\nSee **also** \
                     [`VecDeque`](https://doc.rust-lang.org/std/collections/struct.VecDeque.html).";
        let blocks = parse(hover);
        assert_eq!(blocks.len(), 5, "{blocks:#?}");
        let Block::Code(path) = &blocks[0] else {
            panic!("{blocks:#?}");
        };
        assert_eq!(path.language.as_deref(), Some("rust"));
        assert_eq!(path.text, "std::vec");
        assert!(matches!(&blocks[1], Block::Code(c) if c.text == "pub struct Vec<T>"));
        assert_eq!(blocks[2], Block::Rule);
        let Block::Paragraph(spans) = &blocks[3] else {
            panic!("{blocks:#?}");
        };
        assert_eq!(
            spans,
            &[
                span("A contiguous growable array type, written as ", PLAIN),
                span("Vec<T>", CODE),
                span(".", PLAIN),
            ]
        );
        let Block::Paragraph(spans) = &blocks[4] else {
            panic!("{blocks:#?}");
        };
        assert_eq!(text(spans), "See also VecDeque.");
        assert!(spans.iter().any(|s| s.text == "also" && s.style.bold));
        assert!(
            spans
                .iter()
                .any(|s| s.text == "VecDeque" && s.style.code && s.style.link)
        );
    }

    #[test]
    fn paragraph_lines_join_and_hard_breaks_stay() {
        let blocks = parse("one\ntwo  \nthree\\\nfour");
        assert_eq!(
            blocks,
            [Block::Paragraph(vec![span("one two\nthree\nfour", PLAIN)])]
        );
    }

    #[test]
    fn lists_headings_and_quotes() {
        let blocks = parse("# Title\n- first\n  continued\n  - nested\n2. second\n> quoted\nlazy");
        assert_eq!(blocks[0], Block::Heading(vec![span("Title", PLAIN)]));
        assert_eq!(
            blocks[1],
            Block::Item {
                depth: 0,
                marker: "•".into(),
                spans: vec![span("first continued", PLAIN)]
            }
        );
        assert!(matches!(&blocks[2], Block::Item { depth: 1, .. }));
        assert!(matches!(&blocks[3], Block::Item { marker, .. } if marker == "2."));
        assert_eq!(blocks[4], Block::Quote(vec![span("quoted lazy", PLAIN)]));
    }

    #[test]
    fn fences_keep_their_content_verbatim() {
        let blocks = parse("~~~~ Python extra\n  x = 1\n```\n# not a heading\n~~~~\nafter");
        let Block::Code(code) = &blocks[0] else {
            panic!("{blocks:#?}");
        };
        assert_eq!(code.language.as_deref(), Some("python"));
        assert_eq!(code.text, "  x = 1\n```\n# not a heading");
        assert_eq!(blocks[1], Block::Paragraph(vec![span("after", PLAIN)]));
        // An unclosed fence runs to the end; an indented fence loses its indentation.
        let blocks = parse("  ```\n  let a;\n    let b;");
        assert!(matches!(&blocks[0], Block::Code(c) if c.text == "let a;\n  let b;"));
    }

    #[test]
    fn emphasis_needs_matching_delimiters() {
        let spans = parse_inline("a *b* __c__ ***d*** snake_case_name 2 * 3 * 4 *open");
        assert_eq!(text(&spans), "a b c d snake_case_name 2 * 3 * 4 *open");
        let style_of = |t: &str| spans.iter().find(|s| s.text == t).unwrap().style;
        assert!(style_of("b").italic && !style_of("b").bold);
        assert!(style_of("c").bold && !style_of("c").italic);
        assert!(style_of("d").bold && style_of("d").italic);
        assert!(parse_inline("_x_")[0].style.italic);
    }

    #[test]
    fn code_spans_escapes_and_entities() {
        assert_eq!(
            parse_inline("``a ` b`` and ` x `"),
            vec![span("a ` b", CODE), span(" and ", PLAIN), span("x", CODE)]
        );
        assert_eq!(
            text(&parse_inline("\\*not\\* &lt;T&gt; &amp; &#65;")),
            "*not* <T> & A"
        );
        // An unmatched backtick stays text.
        assert_eq!(parse_inline("a `b"), vec![span("a `b", PLAIN)]);
    }

    #[test]
    fn links_show_their_text() {
        let spans = parse_inline("see [the docs](https://x.y/(a)) and <https://x.y> or [ref][1]");
        assert_eq!(text(&spans), "see the docs and https://x.y or ref");
        assert!(spans.iter().filter(|s| s.style.link).count() == 3);
        // Their targets, for a click (part 10.3).
        let targets: Vec<_> = spans.iter().filter_map(|s| s.target.as_deref()).collect();
        assert_eq!(targets, vec!["https://x.y/(a)", "https://x.y"]);
        let spans = parse_inline("[a](https://a.b \"Title\")[b](<https://c.d>)");
        let targets: Vec<_> = spans.iter().map(|s| s.target.as_deref()).collect();
        assert_eq!(targets, vec![Some("https://a.b"), Some("https://c.d")]);
        // Plain brackets are not links; brackets around code are (rustdoc).
        assert_eq!(text(&parse_inline("[1, 2] [`Vec`]")), "[1, 2] Vec");
        assert_eq!(text(&parse_inline("a<br>b <T>")), "a\nb <T>");
    }

    #[test]
    fn tidy_drops_empty_paragraphs_and_stray_rules() {
        let mut blocks = vec![
            Block::Rule,
            Block::Paragraph(vec![span(" ", PLAIN)]),
            Block::Paragraph(vec![span("a", PLAIN)]),
            Block::Rule,
            Block::Rule,
            Block::Paragraph(vec![span("b", PLAIN)]),
            Block::Rule,
        ];
        tidy(&mut blocks);
        assert_eq!(
            blocks,
            [
                Block::Paragraph(vec![span("a", PLAIN)]),
                Block::Rule,
                Block::Paragraph(vec![span("b", PLAIN)]),
            ]
        );
    }

    #[test]
    fn plain_text_keeps_line_breaks() {
        assert_eq!(
            plain("first\nsecond\n\n\nthird *not bold*"),
            [
                Block::Paragraph(vec![span("first\nsecond", PLAIN)]),
                Block::Paragraph(vec![span("third *not bold*", PLAIN)]),
            ]
        );
    }

    #[test]
    fn rules_and_list_markers() {
        assert!(is_rule("---") && is_rule("* * *") && is_rule("___"));
        assert!(!is_rule("--") && !is_rule("-*-") && !is_rule("--- x"));
        assert_eq!(list_marker("10) x"), Some(("10.".into(), "x")));
        assert_eq!(list_marker("-x"), None);
        assert_eq!(list_marker("1.5 x"), None);
    }

    #[test]
    fn fence_languages_resolve_by_name_extension_and_alias() {
        flux_syntax::standard::register();
        let name = |info: &str| fence_language(info).map(|l| l.name().to_string());
        assert_eq!(name("rust").as_deref(), Some("rust"));
        assert_eq!(name("rs").as_deref(), Some("rust"));
        assert_eq!(name("py").as_deref(), Some("python"));
        assert_eq!(name("golang").as_deref(), Some("go"));
        assert_eq!(name("sh").as_deref(), Some("bash"));
        assert_eq!(name("text"), None);
    }

    #[test]
    fn tables_with_alignment_and_ragged_rows() {
        let blocks = parse(
            "Intro\n| Name | Count | Note |\n|:-----|------:|:----:|\n| a | 1 | x \\| y |\n| b |\n\nAfter",
        );
        assert_eq!(blocks.len(), 3, "{blocks:#?}");
        let Block::Table(table) = &blocks[1] else {
            panic!("{blocks:#?}");
        };
        assert_eq!(table.align, [Align::Left, Align::Right, Align::Center]);
        assert_eq!(text(&table.header[1]), "Count");
        assert_eq!(table.rows.len(), 2);
        assert_eq!(text(&table.rows[0][2]), "x | y");
        // A short row gets empty cells.
        assert_eq!(table.rows[1].len(), 3);
        // A pipe in prose without a separator row is not a table.
        assert!(matches!(&parse("a | b\nc")[0], Block::Paragraph(_)));
    }

    #[test]
    fn code_blocks_get_highlighted() {
        flux_syntax::standard::register();
        let scopes: Vec<String> = Theme::flux_night()
            .syntax_scopes()
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut blocks = parse("```rust\nfn main() {}\nlet x = 1;\n```\n```\nplain\n```");
        highlight(&mut blocks, None, &scopes);
        let Block::Code(rust) = &blocks[0] else {
            panic!()
        };
        assert_eq!(rust.highlights.len(), 2);
        let keyword = scopes.iter().position(|s| s == "keyword").unwrap();
        assert!(
            rust.highlights[0]
                .iter()
                .any(|s| s.start == 0 && s.end == 2 && s.highlight.0 == keyword),
            "{:?}",
            rust.highlights
        );
        // Without a fence language and without a fallback: no highlighting.
        assert!(matches!(&blocks[1], Block::Code(c) if c.highlights.is_empty()));
    }
}
