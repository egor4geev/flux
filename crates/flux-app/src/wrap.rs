//! Soft wraps (part 10.5): which display rows each line of a document takes. Without soft wrap a
//! line is one row; with it, a long line continues on the next rows at the editor's width, as
//! JetBrains IDEs do (Settings → Editor → Soft Wraps; ⌃⌥W switches it for one editor).
//!
//! A line wraps at word starts (after spaces), else after punctuation (`,` `.` `/` `)` …) and
//! between wide characters (CJK), else inside the word; spaces at a wrap stay at the end of the
//! row. The wrapped parts start at the line's indent ("Use original line's indent for wrapped
//! fragments") plus an additional indent. Widths are the code font's advances of the characters
//! (measured once each), so the rows match what the text system draws.
//!
//! The map is incremental: an edit wraps again only the lines it touched; a new width wraps every
//! line (a code line that fits is counted without measuring its characters).

use std::collections::HashMap;
use std::sync::Arc;

use flux_core::movement::Direction;
use flux_core::text::{line_ending_len, line_len, line_start};
use flux_core::transaction::Operation;
use flux_core::{Range, Rope, RopeSlice, TextChange};
use gpui::{App, Font, KeyBinding, Pixels, TextRun, WindowTextSystem, actions};

use crate::editor::Editor;
use crate::theme;

actions!(
    editor,
    [
        /// ⌃⌥W: soft wraps on or off in this editor (JetBrains: View → Active Editor → Soft-Wrap).
        ToggleSoftWrap,
    ]
);

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "ctrl-alt-w",
        ToggleSoftWrap,
        Some("Editor"),
    )]);
}

/// A row never gets narrower than this many spaces, however narrow the editor: a tiny width would
/// otherwise make a row of every character.
const MIN_COLUMNS: f32 = 10.;

/// Lines wrapped in a frame when the width changes: a long document wraps the lines on screen at
/// once and the rest over the next frames (100 000 lines of prose take ~80 ms in a release build).
const LINES_PER_FRAME: usize = 10_000;

/// How the lines wrap: the width and the indent of the wrapped parts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WrapParams {
    /// The width a row's text may take, from the text's left edge.
    pub width: f32,
    /// The width of a space: tabs and the additional indent are counted in it.
    pub space: f32,
    /// Wrapped parts start at the line's indent.
    pub keep_indent: bool,
    /// Spaces added to the indent of the wrapped parts.
    pub extra_indent: u32,
}

/// Where a line's rows start, and where the wrapped parts are placed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LineWraps {
    /// The columns (characters of the line) where the rows after the first start; empty — the line
    /// is one row.
    pub breaks: Vec<u32>,
    /// The display column (tab stops counted) at each break: a row's tabs expand from it.
    pub display_columns: Vec<u32>,
    /// Where the text of the wrapped parts starts, from the text's left edge: the indent.
    pub indent: f32,
}

/// A character of a line with its width and the display column it starts at.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Item {
    c: char,
    width: f32,
    display: u32,
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// A row may start after these: the end of a clause, a path, an expression.
fn breaks_after(c: char) -> bool {
    matches!(
        c,
        ',' | ';'
            | '.'
            | ':'
            | ')'
            | ']'
            | '}'
            | '('
            | '['
            | '{'
            | '>'
            | '/'
            | '\\'
            | '|'
            | '&'
            | '='
            | '+'
            | '-'
            | '*'
            | '!'
            | '?'
    )
}

/// The width of a tab starting at display column `display`.
fn tab_columns(display: u32) -> u32 {
    let tab = theme::TAB_WIDTH as u32;
    tab - display % tab
}

/// Measures the characters of a line: their widths (`width_of` for all but tabs) and display
/// columns.
fn measure_line(
    chars: impl IntoIterator<Item = char>,
    width_of: &mut impl FnMut(char) -> f32,
    space: f32,
    items: &mut Vec<Item>,
) {
    items.clear();
    let mut display = 0;
    for c in chars {
        let (width, columns) = if c == '\t' {
            let columns = tab_columns(display);
            (columns as f32 * space, columns)
        } else {
            (width_of(c), 1)
        };
        items.push(Item { c, width, display });
        display += columns;
    }
}

/// Wraps a measured line: see the module's notes for where rows start.
fn wrap_items(items: &[Item], params: &WrapParams) -> LineWraps {
    let width = params.width.max(params.space * MIN_COLUMNS);
    let mut wraps = LineWraps::default();
    let total: f32 = items.iter().map(|item| item.width).sum();
    if total <= width {
        return wraps;
    }
    let indent = continuation_indent(items, params, width);
    let wide = params.space * 1.5;
    let mut row_start = 0;
    let mut row_x = 0.;
    let mut x = 0.;
    // The latest columns where the current row may wrap: a word's start, else after punctuation
    // or next to a wide character.
    let mut word: Option<usize> = None;
    let mut other: Option<usize> = None;
    let mut i = 0;
    while i < items.len() {
        let item = items[i];
        let blank = is_blank(item.c);
        if i > row_start && !blank {
            let prev = items[i - 1];
            // Wide characters (CJK) are words of their own: a row may start at any of them.
            if is_blank(prev.c) || prev.width > wide || item.width > wide {
                word = Some(i);
            } else if breaks_after(prev.c) {
                other = Some(i);
            }
            // Spaces never overflow: they hang past the edge until the next word.
            if row_x + x + item.width > width {
                let at = word
                    .filter(|&at| at > row_start)
                    .or(other.filter(|&at| at > row_start))
                    .unwrap_or(i);
                wraps.breaks.push(at as u32);
                wraps.display_columns.push(items[at].display);
                row_start = at;
                row_x = indent;
                x = 0.;
                word = None;
                other = None;
                // The characters from `at` are laid out again on the new row.
                i = at;
                continue;
            }
        }
        x += item.width;
        i += 1;
    }
    if !wraps.breaks.is_empty() {
        wraps.indent = indent;
    }
    wraps
}

/// Where the wrapped parts of a line start: its indent (plus the additional one), unless that
/// leaves less than half the width for the text.
fn continuation_indent(items: &[Item], params: &WrapParams, width: f32) -> f32 {
    if !params.keep_indent {
        return 0.;
    }
    let Some(first_text) = items.iter().position(|item| !is_blank(item.c)) else {
        return 0.;
    };
    let indent = items[..first_text]
        .iter()
        .map(|item| item.width)
        .sum::<f32>()
        + params.extra_indent as f32 * params.space;
    if indent <= width / 2. { indent } else { 0. }
}

/// The characters of a line, without its line break.
fn line_chars(line: RopeSlice<'_>) -> impl Iterator<Item = char> + '_ {
    let len = line.len_chars() - line_ending_len(line);
    line.chars().take(len)
}

/// The widths of the code font's characters, each measured once.
#[derive(Clone)]
pub struct Measure {
    text_system: Arc<WindowTextSystem>,
    font: Font,
    font_size: Pixels,
    /// ASCII widths; NaN until measured.
    ascii: [f32; 128],
    other: HashMap<char, f32>,
    /// Every printable ASCII character is as wide as a space: a line of them is counted without
    /// measuring each.
    monospace: bool,
}

impl std::fmt::Debug for Measure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Measure")
            .field("font", &self.font.family)
            .field("font_size", &self.font_size)
            .finish()
    }
}

impl Measure {
    pub fn new(text_system: Arc<WindowTextSystem>, font: Font, font_size: Pixels) -> Self {
        let mut measure = Self {
            text_system,
            font,
            font_size,
            ascii: [f32::NAN; 128],
            other: HashMap::new(),
            monospace: false,
        };
        // A proportional font differs on these; a monospace one doesn't.
        let space = measure.width(' ');
        measure.monospace = ['i', 'W', 'm', '.', '0']
            .into_iter()
            .all(|c| (measure.width(c) - space).abs() < 0.01);
        measure
    }

    fn is_for(&self, font: &Font, font_size: Pixels) -> bool {
        self.font == *font && self.font_size == font_size
    }

    /// The advance of `c` in the code font (or the font the system draws it with).
    pub fn width(&mut self, c: char) -> f32 {
        if (c as u32) < 128 {
            let known = self.ascii[c as usize];
            if !known.is_nan() {
                return known;
            }
            let width = self.compute(c);
            self.ascii[c as usize] = width;
            return width;
        }
        if let Some(&width) = self.other.get(&c) {
            return width;
        }
        let width = self.compute(c);
        self.other.insert(c, width);
        width
    }

    fn compute(&self, c: char) -> f32 {
        if c.is_control() {
            return 0.;
        }
        let mut buffer = [0; 4];
        let text = c.encode_utf8(&mut buffer);
        let run = TextRun {
            len: text.len(),
            font: self.font.clone(),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        f32::from(
            self.text_system
                .layout_line(text, self.font_size, &[run], None)
                .width,
        )
    }
}

/// Where the widths of characters come from: the code font, or fixed ones in the tests.
#[derive(Debug, Clone)]
enum Widths {
    Font(Box<Measure>),
    #[cfg(test)]
    Fixed,
}

impl Widths {
    fn width(&mut self, c: char) -> f32 {
        match self {
            Widths::Font(measure) => measure.width(c),
            #[cfg(test)]
            Widths::Fixed => tests::width_of(c),
        }
    }

    /// Printable ASCII characters are all as wide as a space.
    fn monospace(&self) -> bool {
        match self {
            Widths::Font(measure) => measure.monospace,
            #[cfg(test)]
            Widths::Fixed => true,
        }
    }

    fn is_for(&self, font: &Font, font_size: Pixels) -> bool {
        match self {
            Widths::Font(measure) => measure.is_for(font, font_size),
            #[cfg(test)]
            Widths::Fixed => true,
        }
    }
}

/// A display row of a line: its columns, where its text starts, whether the line goes on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowSpan {
    /// The columns of the line on the row: `start..end` (`end` — the next row's start, or the
    /// line's end).
    pub start: usize,
    pub end: usize,
    /// Where the row's text starts, from the text's left edge (the indent of a wrapped part).
    pub x: f32,
    /// The display column at `start`: the row's tabs expand from it.
    pub display: u32,
    /// More rows of the same line follow.
    pub wraps: bool,
}

/// The display rows of a document. Rows are what the editor draws, scrolls and moves the cursor
/// up and down by; the scrollbar maps them to its height.
#[derive(Debug, Clone, Default)]
pub struct WrapMap {
    enabled: bool,
    /// What the rows were counted with; `None` — not laid out yet (a row per line until then).
    params: Option<WrapParams>,
    /// For each line, where its rows start (while enabled and laid out).
    lines: Vec<LineWraps>,
    /// The first display row of each line, and the total after the last.
    starts: Vec<usize>,
    /// The document's lines.
    line_count: usize,
    /// The lines from this one on may still be wrapped at an old width: a long document is wrapped
    /// over several frames.
    pending: Option<usize>,
    widths: Option<Widths>,
    /// A line's characters while it is being wrapped.
    scratch: Vec<Item>,
}

impl WrapMap {
    /// The map of a new or reloaded document.
    pub fn reset(&mut self, text: &Rope) {
        self.line_count = text.len_lines();
        self.params = None;
        self.pending = None;
        self.lines.clear();
        self.starts.clear();
    }

    /// Soft wraps on or off; turned on, the lines are wrapped at the next layout.
    pub fn set_enabled(&mut self, on: bool) {
        if self.enabled == on {
            return;
        }
        self.enabled = on;
        self.params = None;
        self.pending = None;
        self.lines = Vec::new();
        self.starts = Vec::new();
    }

    /// Lays the rows out for `params` (the editor's width) with the code font: wraps the lines
    /// again when the width or the indent changed — `focus` (the lines on screen) at once, the rest
    /// [`LINES_PER_FRAME`] at a time ([`WrapMap::pending`] until done). `true` — the rows changed.
    pub fn layout(
        &mut self,
        text: &Rope,
        params: WrapParams,
        focus: std::ops::Range<usize>,
        text_system: &Arc<WindowTextSystem>,
        font: &Font,
        font_size: Pixels,
    ) -> bool {
        if !self
            .widths
            .as_ref()
            .is_some_and(|widths| widths.is_for(font, font_size))
        {
            let measure = Measure::new(text_system.clone(), font.clone(), font_size);
            self.widths = Some(Widths::Font(Box::new(measure)));
            self.params = None;
        }
        self.layout_rows(text, params, focus)
    }

    /// [`WrapMap::layout`] with the widths at hand.
    fn layout_rows(
        &mut self,
        text: &Rope,
        params: WrapParams,
        focus: std::ops::Range<usize>,
    ) -> bool {
        self.line_count = text.len_lines();
        if !self.enabled {
            return false;
        }
        if self.params != Some(params) || self.lines.len() != self.line_count {
            self.params = Some(params);
            // Until wrapped, a line keeps its rows at the old width (one row the first time).
            self.lines.resize_with(self.line_count, LineWraps::default);
            self.pending = Some(0);
        }
        let Some(next) = self.pending else {
            return false;
        };
        for line in focus.start.max(next)..focus.end.min(self.line_count) {
            self.lines[line] = self.wrap_line(text.line(line), &params);
        }
        let end = (next + LINES_PER_FRAME).min(self.line_count);
        for (line, slice) in (next..end).zip(text.lines_at(next)) {
            self.lines[line] = self.wrap_line(slice, &params);
        }
        self.pending = (end < self.line_count).then_some(end);
        self.count_rows();
        true
    }

    /// Lines are still to be wrapped at the current width: the editor asks for another frame.
    pub fn pending(&self) -> bool {
        self.enabled && self.pending.is_some()
    }

    /// Wraps every line at once (the tests' layout).
    #[cfg(test)]
    fn wrap_all(&mut self, text: &Rope) {
        let Some(params) = self.params else {
            return;
        };
        let mut lines = Vec::with_capacity(self.line_count);
        for line in text.lines() {
            lines.push(self.wrap_line(line, &params));
        }
        lines.truncate(self.line_count);
        lines.resize_with(self.line_count, LineWraps::default);
        self.lines = lines;
        self.pending = None;
        self.count_rows();
    }

    fn wrap_line(&mut self, line: RopeSlice, params: &WrapParams) -> LineWraps {
        let Some(widths) = self.widths.as_mut() else {
            return LineWraps::default();
        };
        let len = line.len_chars() - line_ending_len(line);
        // A line of ASCII without tabs that fits: one row, without measuring its characters.
        if widths.monospace()
            && len as f32 * params.space <= params.width
            && line.len_bytes() == line.len_chars()
            && !line
                .chars()
                .take(len)
                .any(|c| c == '\t' || c.is_ascii_control())
        {
            return LineWraps::default();
        }
        let mut items = std::mem::take(&mut self.scratch);
        measure_line(
            line_chars(line),
            &mut |c| widths.width(c),
            params.space,
            &mut items,
        );
        let wraps = wrap_items(&items, params);
        self.scratch = items;
        wraps
    }

    fn count_rows(&mut self) {
        self.starts.clear();
        self.starts.reserve(self.lines.len() + 1);
        let mut row = 0;
        for wraps in &self.lines {
            self.starts.push(row);
            row += wraps.breaks.len() + 1;
        }
        self.starts.push(row);
    }

    /// The document changed: the lines the edit touched are wrapped again.
    pub fn edited(&mut self, text: &Rope, changes: &[TextChange]) {
        let laid_out = self.laid_out();
        self.line_count = text.len_lines();
        if !laid_out {
            return;
        }
        let Some(params) = self.params else {
            return;
        };
        for (index, change) in changes.iter().enumerate() {
            let new = changes.get(index + 1).map_or(text, |next| &next.old_text);
            // The edited regions, as (old start, old end, new start, new end), from the last: the
            // lines before a region keep their indices while it is replaced.
            let mut regions: Vec<(usize, usize, usize, usize)> = Vec::new();
            let (mut old_pos, mut new_pos) = (0, 0);
            for op in change.changes.ops() {
                let (old_len, new_len) = match op {
                    Operation::Retain(n) => {
                        old_pos += n;
                        new_pos += n;
                        continue;
                    }
                    Operation::Delete(n) => (*n, 0),
                    Operation::Insert(text) => (0, text.chars().count()),
                };
                match regions.last_mut() {
                    Some(last) if last.1 == old_pos && last.3 == new_pos => {
                        last.1 += old_len;
                        last.3 += new_len;
                    }
                    _ => regions.push((old_pos, old_pos + old_len, new_pos, new_pos + new_len)),
                }
                old_pos += old_len;
                new_pos += new_len;
            }
            let old = &change.old_text;
            // Lines still waiting for the width: from the first one edited on, as they may have
            // moved.
            if let (Some(pending), Some(&(old_start, ..))) = (self.pending, regions.first()) {
                self.pending = Some(pending.min(old.char_to_line(old_start)));
            }
            for &(old_start, old_end, new_start, new_end) in regions.iter().rev() {
                let old_lines =
                    old.char_to_line(old_start)..old.char_to_line(old_end.min(old.len_chars())) + 1;
                let first = new.char_to_line(new_start);
                let last = new.char_to_line(new_end.min(new.len_chars()));
                let wrapped: Vec<LineWraps> = (first..=last)
                    .map(|line| self.wrap_line(new.line(line), &params))
                    .collect();
                let end = old_lines.end.min(self.lines.len());
                let start = old_lines.start.min(end);
                self.lines.splice(start..end, wrapped);
            }
        }
        // A lost track (it shouldn't happen) wraps everything again at the next layout.
        if self.lines.len() != self.line_count {
            self.params = None;
            self.pending = None;
            self.lines.clear();
            self.starts.clear();
            return;
        }
        self.count_rows();
    }

    /// Long lines wrap at the editor's width.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The rows are laid out: soft wraps are on and the lines were wrapped at a width.
    pub fn laid_out(&self) -> bool {
        self.enabled && self.params.is_some() && self.starts.len() == self.line_count + 1
    }

    /// Display rows in the document, at least 1.
    pub fn row_count(&self) -> usize {
        if self.laid_out() {
            self.starts[self.line_count].max(1)
        } else {
            self.line_count.max(1)
        }
    }

    /// The first display row of `line`.
    pub fn line_to_row(&self, line: usize) -> usize {
        if self.laid_out() {
            self.starts[line.min(self.line_count.saturating_sub(1))]
        } else {
            line.min(self.row_count() - 1)
        }
    }

    /// The line shown on display `row`; rows past the end give the last line.
    pub fn row_to_line(&self, row: usize) -> usize {
        let last = self.line_count.saturating_sub(1);
        if self.laid_out() {
            self.starts
                .partition_point(|&start| start <= row)
                .saturating_sub(1)
                .min(last)
        } else {
            row.min(last)
        }
    }

    /// How many display rows `line` takes.
    pub fn line_rows(&self, line: usize) -> usize {
        self.wraps(line).map_or(1, |wraps| wraps.breaks.len() + 1)
    }

    /// Where `line`'s rows start; `None` — one row (or soft wraps off).
    pub fn wraps(&self, line: usize) -> Option<&LineWraps> {
        self.laid_out()
            .then(|| self.lines.get(line))
            .flatten()
            .filter(|wraps| !wraps.breaks.is_empty())
    }

    /// The row of `line` (0 — its first) the caret at `column` is on. At a soft wrap the caret is
    /// at the start of the next row, as editors show it.
    pub fn row_in_line(&self, line: usize, column: usize) -> usize {
        self.wraps(line).map_or(0, |wraps| {
            wraps.breaks.partition_point(|&at| at as usize <= column)
        })
    }

    /// The display row of the caret at `pos`.
    pub fn row_of(&self, text: &Rope, pos: usize) -> usize {
        let pos = pos.min(text.len_chars());
        let line = text.char_to_line(pos);
        self.line_to_row(line) + self.row_in_line(line, pos - line_start(text, line))
    }

    /// Row `row` of `line` (0 — its first).
    pub fn row_span(&self, text: &Rope, line: usize, row: usize) -> RowSpan {
        let len = line_len(text, line);
        let Some(wraps) = self.wraps(line) else {
            return RowSpan {
                start: 0,
                end: len,
                x: 0.,
                display: 0,
                wraps: false,
            };
        };
        let row = row.min(wraps.breaks.len());
        let start = match row {
            0 => 0,
            _ => wraps.breaks[row - 1] as usize,
        };
        RowSpan {
            start,
            end: wraps.breaks.get(row).map_or(len, |&at| at as usize),
            x: if row == 0 { 0. } else { wraps.indent },
            display: match row {
                0 => 0,
                _ => wraps.display_columns[row - 1],
            },
            wraps: row < wraps.breaks.len(),
        }
    }

    /// The x of the caret at `column` of `line` on its row, from the text's left edge, as the
    /// characters' widths add up.
    pub fn x_of_column(&mut self, text: &Rope, line: usize, column: usize) -> f32 {
        let span = self.row_span(text, line, self.row_in_line(line, column));
        let Some(space) = self.params.map(|params| params.space) else {
            return 0.;
        };
        let Some(widths) = self.widths.as_mut() else {
            return 0.;
        };
        let mut x = span.x;
        let mut display = span.display;
        let column = column.clamp(span.start, span.end);
        for c in text.line(line).slice(span.start..column).chars() {
            let (width, columns) = char_advance(c, display, space, widths);
            x += width;
            display += columns;
        }
        x
    }

    /// The column of row `row` of `line` closest to `x` (from the text's left edge). A wrapped
    /// row's end is the next row's start: the caret stays before its last character.
    pub fn column_for_x(&mut self, text: &Rope, line: usize, row: usize, x: f32) -> usize {
        let span = self.row_span(text, line, row);
        let last = if span.wraps && span.end > span.start {
            span.end - 1
        } else {
            span.end
        };
        let Some(space) = self.params.map(|params| params.space) else {
            return span.start;
        };
        let Some(widths) = self.widths.as_mut() else {
            return span.start;
        };
        let mut left = span.x;
        let mut display = span.display;
        let mut column = span.start;
        for c in text.line(line).slice(span.start..last).chars() {
            let (width, columns) = char_advance(c, display, space, widths);
            // Past the middle of the character: after it.
            if x < left + width / 2. {
                return column;
            }
            left += width;
            display += columns;
            column += 1;
        }
        column
    }

    /// ↑ / ↓ by display rows (`count` of them), keeping the caret's x: a goal kept in pixels while
    /// it passes short rows.
    pub fn move_vertically(
        &mut self,
        text: &Rope,
        range: Range,
        dir: Direction,
        count: usize,
        extend: bool,
    ) -> Range {
        let head = range.head.min(text.len_chars());
        let line = text.char_to_line(head);
        let column = head - line_start(text, line);
        let row = self.line_to_row(line) + self.row_in_line(line, column);
        let goal = match range.goal_column {
            Some(goal) => goal as f32,
            None => self.x_of_column(text, line, column),
        };
        let last_row = self.row_count() - 1;
        let target = match dir {
            Direction::Backward if row == 0 => return range.put_cursor(0, extend),
            Direction::Forward if row >= last_row => {
                return range.put_cursor(text.len_chars(), extend);
            }
            Direction::Backward => row.saturating_sub(count),
            Direction::Forward => (row + count).min(last_row),
        };
        let target_line = self.row_to_line(target);
        let in_line = target - self.line_to_row(target_line);
        let column = self.column_for_x(text, target_line, in_line, goal);
        range
            .put_cursor(line_start(text, target_line) + column, extend)
            .with_goal_column(Some(goal.round().max(0.) as usize))
    }
}

/// The advance of `c` at display column `display`, and the display columns it takes.
fn char_advance(c: char, display: u32, space: f32, widths: &mut Widths) -> (f32, u32) {
    if c == '\t' {
        let columns = tab_columns(display);
        (columns as f32 * space, columns)
    } else {
        (widths.width(c), 1)
    }
}

/// Before the editor's rows are placed (each frame): soft wraps on or off as the editor wants them
/// ([`Editor::soft_wrap_wanted`]), the rows at the width of `params`. When the rows change (a new
/// width, wraps turned on or off), the line at the top of the view stays there — unless the view is
/// about to scroll to the caret anyway.
#[allow(clippy::too_many_arguments)]
pub fn update(
    editor: &mut Editor,
    params: WrapParams,
    visible_rows: usize,
    text_system: &Arc<WindowTextSystem>,
    font: &Font,
    font_size: Pixels,
    cx: &App,
) {
    let wanted = editor.soft_wrap_wanted(cx);
    let top = Anchor::of(&editor.wrap, editor.scroll.y);
    let top_line = top.line;
    // A glide on its way keeps its ends on their lines as well.
    let glide = editor
        .glide
        .filter(|glide| glide.shown == editor.scroll.y)
        .map(|glide| {
            (
                glide,
                Anchor::of(&editor.wrap, glide.from),
                Anchor::of(&editor.wrap, glide.to),
            )
        });
    let was = editor.wrap.enabled();
    editor.wrap.set_enabled(wanted);
    let text = editor.document.text().clone();
    // The lines on screen are wrapped first: from the top one, at most a line per row; and the
    // caret's, where the view may be about to scroll.
    let head = editor
        .document
        .selection()
        .primary()
        .head
        .min(text.len_chars());
    let focus = match editor.autoscroll {
        Some(_) => {
            let line = text.char_to_line(head);
            line.saturating_sub(visible_rows)..line + visible_rows
        }
        None => top_line..top_line + visible_rows + 1,
    };
    let changed = editor
        .wrap
        .layout(&text, params, focus, text_system, font, font_size)
        || was != wanted;
    if changed && editor.autoscroll.is_none() {
        editor.scroll.y = top.offset(&editor.wrap);
        if let Some((glide, from, to)) = glide {
            editor.glide = Some(crate::editor::Glide {
                from: from.offset(&editor.wrap),
                to: to.offset(&editor.wrap),
                shown: editor.scroll.y,
                ..glide
            });
        }
    }
}

/// A scroll offset as a place in the text — a line, a row of it, the pixels into that row — to put
/// it back on the same line once the rows change.
struct Anchor {
    line: usize,
    into: usize,
    fraction: f32,
}

impl Anchor {
    fn of(wrap: &WrapMap, offset: f32) -> Self {
        let lh = theme::LINE_HEIGHT;
        let offset = offset.max(0.);
        let row = (offset / lh).floor() as usize;
        let line = wrap.row_to_line(row);
        Self {
            line,
            into: row.saturating_sub(wrap.line_to_row(line)),
            fraction: offset - row as f32 * lh,
        }
    }

    fn offset(&self, wrap: &WrapMap) -> f32 {
        let rows = wrap.line_rows(self.line);
        let row = wrap.line_to_row(self.line) + self.into.min(rows - 1);
        row as f32 * theme::LINE_HEIGHT + self.fraction
    }
}

/// Whether the file `name` matches one of the masks of Settings → Editor (`*.md`, `README.*`,
/// `COMMIT_EDITMSG`): `*` — any characters, `?` — one; letter case doesn't matter.
pub fn matches_masks(masks: &[String], name: &str) -> bool {
    let name: Vec<char> = name.to_lowercase().chars().collect();
    masks.iter().any(|mask| {
        let mask: Vec<char> = mask.trim().to_lowercase().chars().collect();
        !mask.is_empty() && glob(&mask, &name)
    })
}

fn glob(mask: &[char], name: &[char]) -> bool {
    match mask.split_first() {
        None => name.is_empty(),
        Some(('*', rest)) => (0..=name.len()).any(|skip| glob(rest, &name[skip..])),
        Some(('?', rest)) => !name.is_empty() && glob(rest, &name[1..]),
        Some((c, rest)) => name.first() == Some(c) && glob(rest, &name[1..]),
    }
}

/// The masks as Settings → Editor shows them: "*.md; *.txt".
pub fn masks_text(masks: &[String]) -> String {
    masks.join("; ")
}

/// The masks typed in Settings → Editor: separated by `;` (or `,`), blanks dropped.
pub fn parse_masks(text: &str) -> Vec<String> {
    text.split([';', ','])
        .map(str::trim)
        .filter(|mask| !mask.is_empty())
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A monospace font of 1-wide characters; CJK (and ✨) 2 wide.
    pub(super) fn width_of(c: char) -> f32 {
        if c >= '\u{1100}' { 2. } else { 1. }
    }

    fn params(width: f32) -> WrapParams {
        WrapParams {
            width,
            space: 1.,
            keep_indent: false,
            extra_indent: 0,
        }
    }

    fn wrap(line: &str, params: &WrapParams) -> LineWraps {
        let mut items = Vec::new();
        measure_line(line.chars(), &mut width_of, params.space, &mut items);
        wrap_items(&items, params)
    }

    /// The rows of `line` as strings.
    fn rows(line: &str, params: &WrapParams) -> Vec<String> {
        let wraps = wrap(line, params);
        let chars: Vec<char> = line.chars().collect();
        let mut starts = vec![0];
        starts.extend(wraps.breaks.iter().map(|&at| at as usize));
        starts.push(chars.len());
        starts
            .windows(2)
            .map(|pair| chars[pair[0]..pair[1]].iter().collect())
            .collect()
    }

    #[test]
    fn a_line_that_fits_is_one_row() {
        assert_eq!(rows("short line", &params(20.)), ["short line"]);
        assert_eq!(rows("", &params(20.)), [""]);
        assert_eq!(wrap("exactly ten", &params(11.)).breaks, Vec::<u32>::new());
    }

    #[test]
    fn words_wrap_after_the_spaces() {
        assert_eq!(
            rows("the quick brown fox jumps over", &params(12.)),
            ["the quick ", "brown fox ", "jumps over"]
        );
        // Spaces hang at the end of a row, however many.
        assert_eq!(
            rows("aaaa bbbb    cccc", &params(10.)),
            ["aaaa bbbb    ", "cccc"]
        );
    }

    #[test]
    fn a_long_word_wraps_after_punctuation_or_inside() {
        assert_eq!(
            rows("see https://example.com/a/b/c", &params(14.)),
            ["see ", "https://", "example.com/a/", "b/c"]
        );
        assert_eq!(
            rows("abcdefghijklmnopqrstuvwxyz", &params(10.)),
            ["abcdefghij", "klmnopqrst", "uvwxyz"]
        );
        // A word start is preferred to punctuation before it.
        assert_eq!(
            rows("x = foo.bar(baz)", &params(12.)),
            ["x = ", "foo.bar(baz)"]
        );
    }

    #[test]
    fn wide_characters_take_two_columns_and_wrap_between_them() {
        assert_eq!(
            rows("你好世界你好世界", &params(10.)),
            ["你好世界你", "好世界"]
        );
        assert_eq!(
            rows("ab 你好世界你好", &params(10.)),
            ["ab 你好世", "界你好"]
        );
    }

    #[test]
    fn tabs_expand_to_tab_stops() {
        // "a\tb": the tab takes 3 columns; "\t" at column 4 takes 4.
        let mut items = Vec::new();
        measure_line("a\tb\tc".chars(), &mut width_of, 1., &mut items);
        let widths: Vec<f32> = items.iter().map(|item| item.width).collect();
        assert_eq!(widths, [1., 3., 1., 3., 1.]);
        let displays: Vec<u32> = items.iter().map(|item| item.display).collect();
        assert_eq!(displays, [0, 1, 4, 5, 8]);
        // The display column at a break expands the next row's tabs.
        let wraps = wrap("aaaa bbbb\tcc dd", &params(10.));
        assert_eq!(wraps.breaks, [10]);
        assert_eq!(wraps.display_columns, [12]);
    }

    #[test]
    fn wrapped_parts_keep_the_indent() {
        let indented = WrapParams {
            keep_indent: true,
            ..params(16.)
        };
        let wraps = wrap("    one two three four", &indented);
        assert_eq!(wraps.indent, 4.);
        assert_eq!(
            rows("    one two three four", &indented),
            ["    one two ", "three four"]
        );
        // The extra indent adds to it.
        let extra = WrapParams {
            extra_indent: 2,
            ..indented
        };
        assert_eq!(wrap("    one two three four", &extra).indent, 6.);
        // An indent over half the width isn't kept.
        let deep = wrap("            one two three four", &indented);
        assert_eq!(deep.indent, 0.);
        // Without the option, wrapped parts start at the edge.
        assert_eq!(wrap("    one two three four", &params(16.)).indent, 0.);
    }

    #[test]
    fn rows_are_never_narrower_than_the_minimum() {
        // A width of 3 still gives rows of ten columns.
        assert_eq!(
            rows("abcdefghijklmnopqrstuvwxyz", &params(3.)),
            ["abcdefghij", "klmnopqrst", "uvwxyz"]
        );
    }

    #[test]
    fn masks_match_file_names() {
        let masks = parse_masks("*.md; *.TXT ;README.*,  COMMIT_EDITMSG;; ");
        assert_eq!(masks, ["*.md", "*.TXT", "README.*", "COMMIT_EDITMSG"]);
        assert!(matches_masks(&masks, "guide.md"));
        assert!(matches_masks(&masks, "notes.txt"));
        assert!(matches_masks(&masks, "README.rst"));
        assert!(matches_masks(&masks, "COMMIT_EDITMSG"));
        assert!(!matches_masks(&masks, "main.rs"));
        assert!(!matches_masks(&masks, "md"));
        assert!(matches_masks(&["?.c".to_string()], "a.c"));
        assert!(!matches_masks(&["?.c".to_string()], "ab.c"));
        assert_eq!(masks_text(&masks[..2]), "*.md; *.TXT");
    }

    /// A map laid out without a window, with [`width_of`].
    fn map_of(text: &Rope, params: WrapParams) -> WrapMap {
        let mut map = WrapMap {
            enabled: true,
            widths: Some(Widths::Fixed),
            ..WrapMap::default()
        };
        map.line_count = text.len_lines();
        map.params = Some(params);
        map.wrap_all(text);
        map
    }

    #[test]
    fn rows_and_lines_map_both_ways() {
        let text = Rope::from_str("one two three four five\nshort\n\nsix seven eight nine ten");
        let map = map_of(&text, params(10.));
        // "one two ", "three four ", "five" | "short" | "" | "six seven ", "eight nine ", "ten"
        assert_eq!(map.row_count(), 8);
        assert_eq!(
            (0..4).map(|line| map.line_to_row(line)).collect::<Vec<_>>(),
            [0, 3, 4, 5]
        );
        assert_eq!(
            (0..9).map(|row| map.row_to_line(row)).collect::<Vec<_>>(),
            [0, 0, 0, 1, 2, 3, 3, 3, 3]
        );
        assert_eq!(map.line_rows(0), 3);
        assert_eq!(map.line_rows(1), 1);
        // The caret at a wrap is on the next row.
        assert_eq!(map.row_in_line(0, 7), 0);
        assert_eq!(map.row_in_line(0, 8), 1);
        assert_eq!(map.row_of(&text, 8), 1);
        assert_eq!(map.row_of(&text, text.len_chars()), 7);
        let span = map.row_span(&text, 0, 1);
        assert_eq!((span.start, span.end, span.wraps), (8, 19, true));
        let span = map.row_span(&text, 0, 2);
        assert_eq!((span.start, span.end, span.wraps), (19, 23, false));
    }

    #[test]
    fn soft_wraps_off_is_a_row_per_line() {
        let text = Rope::from_str("a\nb\nc");
        let mut map = WrapMap::default();
        map.reset(&text);
        assert!(!map.laid_out());
        assert_eq!(map.row_count(), 3);
        assert_eq!(map.line_to_row(2), 2);
        assert_eq!(map.row_to_line(7), 2);
        assert_eq!(map.row_in_line(1, 5), 0);
        assert_eq!(map.line_rows(0), 1);
    }

    /// Replaces `from..to` of `text` with `insert`; the change for [`WrapMap::edited`].
    fn edit(text: &mut Rope, edits: &[(usize, usize, &str)]) -> TextChange {
        let old_text = text.clone();
        let changes = flux_core::ChangeSet::from_changes(
            text.len_chars(),
            edits
                .iter()
                .map(|&(from, to, insert)| (from, to, Some(insert.to_string()))),
        );
        changes.apply(text);
        TextChange { old_text, changes }
    }

    /// `edited` gives the rows a fresh layout of the new text gives.
    fn check_edit(before: &str, edits: &[(usize, usize, &str)]) {
        let mut text = Rope::from_str(before);
        let mut map = map_of(&text, params(10.));
        let change = edit(&mut text, edits);
        map.edited(&text, &[change]);
        let fresh = map_of(&text, params(10.));
        assert_eq!(map.lines, fresh.lines, "{:?}", text.to_string());
        assert_eq!(map.starts, fresh.starts);
    }

    #[test]
    fn edits_wrap_again_the_lines_they_touch() {
        let text = "aaaa bbbb cccc\nshort\ndddd eeee ffff";
        // Typing makes a line wrap; deleting joins it back.
        check_edit(text, &[(20, 20, " now longer than ten")]);
        check_edit(text, &[(4, 10, "")]);
        // A new line in the middle, lines joined, everything replaced.
        check_edit(text, &[(7, 7, "\nnew line here and more")]);
        check_edit(text, &[(14, 21, " ")]);
        check_edit(text, &[(0, text.chars().count(), "x")]);
        // Several cursors far apart.
        check_edit(text, &[(0, 0, "one more "), (21, 21, "two more words ")]);
        // An insertion at the very end.
        check_edit(text, &[(35, 35, "\nlast line that wraps around")]);
    }

    #[test]
    fn several_changes_in_a_row_are_applied_in_turn() {
        let mut text = Rope::from_str("one\ntwo\nthree");
        let mut map = map_of(&text, params(10.));
        let first = edit(&mut text, &[(3, 3, " and a long tail")]);
        let second = edit(&mut text, &[(0, 0, "head\n")]);
        map.edited(&text, &[first, second]);
        let fresh = map_of(&text, params(10.));
        assert_eq!(map.lines, fresh.lines);
        assert_eq!(map.row_count(), fresh.row_count());
    }

    #[test]
    fn x_and_columns_on_wrapped_rows() {
        let text = Rope::from_str("    one two three four\nab\tc");
        let indented = WrapParams {
            keep_indent: true,
            ..params(16.)
        };
        let mut map = map_of(&text, indented);
        // "    one two " | "three four" at the indent (4).
        assert_eq!(map.x_of_column(&text, 0, 5), 5.);
        assert_eq!(map.x_of_column(&text, 0, 12), 4.);
        assert_eq!(map.x_of_column(&text, 0, 14), 6.);
        assert_eq!(map.column_for_x(&text, 0, 1, 0.), 12);
        assert_eq!(map.column_for_x(&text, 0, 1, 6.2), 14);
        assert_eq!(map.column_for_x(&text, 0, 1, 99.), 22);
        // Past the end of a wrapped row: before its last character (the space).
        assert_eq!(map.column_for_x(&text, 0, 0, 99.), 11);
        // A tab in a line that doesn't wrap.
        assert_eq!(map.x_of_column(&text, 1, 3), 4.);
        assert_eq!(map.column_for_x(&text, 1, 0, 3.9), 3);
    }

    /// Timing on a big file (`cargo test -p flux-app wrap::tests::timing -- --ignored --nocapture`):
    /// wrapping 100 000 lines of prose at a new width, and an edit in the middle.
    #[test]
    #[ignore]
    fn timing_of_a_big_file() {
        let words = [
            "lorem",
            "ipsum",
            "dolor",
            "sit",
            "amet",
            "consectetur",
            "adipiscing",
        ];
        let mut source = String::new();
        for line in 0..100_000 {
            let count = [3, 8, 15, 30, 60][line % 5];
            let words: Vec<&str> = (0..count)
                .map(|i| words[(line + i) % words.len()])
                .collect();
            source.push_str(&words.join(" "));
            source.push('\n');
        }
        let mut text = Rope::from_str(&source);
        for width in [100., 60.] {
            let started = std::time::Instant::now();
            let map = map_of(&text, params(width));
            println!(
                "wrap {} lines at {width} columns: {} rows in {:?}",
                text.len_lines(),
                map.row_count(),
                started.elapsed()
            );
        }
        let mut map = map_of(&text, params(100.));
        let middle = text.line_to_char(50_000) + 10;
        let change = edit(&mut text, &[(middle, middle, "typed ")]);
        let started = std::time::Instant::now();
        map.edited(&text, &[change]);
        println!("an edit: {:?}", started.elapsed());
    }

    #[test]
    fn a_long_document_wraps_over_frames_the_lines_on_screen_first() {
        let line = "one two three four five six seven";
        let source = vec![line; LINES_PER_FRAME * 2 + 10].join("\n");
        let text = Rope::from_str(&source);
        let mut map = WrapMap {
            enabled: true,
            widths: Some(Widths::Fixed),
            ..WrapMap::default()
        };
        // The first frame: the lines on screen (far down) and the first chunk.
        let screen = LINES_PER_FRAME + 500..LINES_PER_FRAME + 530;
        assert!(map.layout_rows(&text, params(10.), screen.clone()));
        assert!(map.pending());
        assert_eq!(map.line_rows(0), 4);
        assert_eq!(map.line_rows(screen.start), 4);
        assert_eq!(map.line_rows(LINES_PER_FRAME + 1), 1);
        // The next frames: the rest, then nothing to do.
        assert!(map.layout_rows(&text, params(10.), 0..0));
        assert!(map.layout_rows(&text, params(10.), 0..0));
        assert!(!map.pending());
        assert!(!map.layout_rows(&text, params(10.), 0..0));
        assert_eq!(map.row_count(), text.len_lines() * 4);
        let fresh = map_of(&text, params(10.));
        assert_eq!(map.lines, fresh.lines);
    }

    #[test]
    fn up_and_down_go_by_rows_and_keep_the_x() {
        let text = Rope::from_str("one two three four five\nshort\nsix seven eight");
        let mut map = map_of(&text, params(10.));
        // "one two " | "three four " | "five" | "short" | "six seven " | "eight"
        let at = |pos| Range::point(pos);
        let down = |map: &mut WrapMap, range| {
            map.move_vertically(&text, range, Direction::Forward, 1, false)
        };
        let up = |map: &mut WrapMap, range| {
            map.move_vertically(&text, range, Direction::Backward, 1, false)
        };
        // From "two" (x 5) down: "three four " → column 13 ("thr|ee"); down: "five", its end.
        let r = down(&mut map, at(5));
        assert_eq!(r.head, 13);
        let r = down(&mut map, r);
        assert_eq!(r.head, 23);
        // A short line: its end; then the goal brings the x back.
        let r = down(&mut map, r);
        assert_eq!(r.head, 24 + 5);
        let r = down(&mut map, r);
        assert_eq!(r.head, 30 + 5);
        let r = up(&mut map, r);
        assert_eq!(r.head, 24 + 5);
        // Up from the first row: the document's start; down from the last: its end.
        assert_eq!(up(&mut map, at(3)).head, 0);
        assert_eq!(down(&mut map, at(42)).head, text.len_chars());
        // Several rows at once (Page Down).
        let r = map.move_vertically(&text, at(1), Direction::Forward, 3, false);
        assert_eq!(r.head, 24 + 1);
    }
}
