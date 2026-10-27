//! Editor rendering. Each frame draws only the visible rows, so the cost of a frame doesn't depend
//! on the file size. With soft wraps (part 10.5) a line takes several display rows
//! ([`crate::wrap`]): the layout is by rows, and whatever is drawn over the text (selections,
//! matches, problems, the caret) finds its rows here.

use std::time::Instant;

use flux_core::Rope;
use flux_core::text::{line_len, line_start};
use gpui::{
    App, Bounds, ContentMask, Element, ElementId, ElementInputHandler, Entity, GlobalElementId,
    Hsla, InspectorElementId, IntoElement, LayoutId, PaintQuad, Pixels, Point, ShapedLine, Style,
    TextRun, Window, fill, font, point, px, relative, size,
};

use crate::display::{display_line, display_range, row_spans, text_runs};
use crate::editor::{Autoscroll, Editor, Glide};
use crate::motion;
use crate::theme::{self, Theme};
use crate::wrap::WrapParams;

/// Layout of the visible part from the previous frame: the mouse and IME use it to convert pixels
/// to text positions and back.
pub struct LayoutCache {
    pub text_bounds: Bounds<Pixels>,
    pub line_height: Pixels,
    /// Screen coordinates of the start of display row 0 (accounting for scroll).
    pub origin: Point<Pixels>,
    /// The first line with a row on screen (its first row may be above it).
    pub first_line: usize,
    /// The first display row on screen.
    pub first_row: usize,
    /// The rows on screen, top to bottom, from `first_row`.
    pub rows: Vec<RowLayout>,
    /// The lines with rows on screen, from `first_line`.
    lines: Vec<LineRows>,
}

/// A line with rows on screen.
#[derive(Debug, Clone, Copy)]
struct LineRows {
    /// The line's first display row (it may be above the screen).
    row: usize,
    /// The display rows the line takes.
    count: usize,
    /// Its rows on screen: `rows[first..first + shown]`.
    first: usize,
    shown: usize,
}

/// A shaped single line with its column mapping: the text field ([`crate::input`]) lays its one
/// line out with it.
pub struct LineLayout {
    pub shaped: ShapedLine,
    /// Byte offset within the displayed line for each column (+ the end). The displayed line
    /// differs from the source line: tabs are expanded to spaces.
    pub char_to_byte: Vec<usize>,
}

impl LineLayout {
    pub fn x_for_column(&self, column: usize) -> Pixels {
        let column = column.min(self.char_to_byte.len() - 1);
        self.shaped.x_for_index(self.char_to_byte[column])
    }

    pub fn column_for_x(&self, x: Pixels) -> usize {
        let byte = self.shaped.closest_index_for_x(x);
        let next = self.char_to_byte.partition_point(|&b| b < byte);
        if next == self.char_to_byte.len() {
            return next - 1;
        }
        // Inside an expanded tab, snap to the nearest edge.
        if next > 0 && self.char_to_byte[next] != byte {
            let prev = next - 1;
            if byte - self.char_to_byte[prev] < self.char_to_byte[next] - byte {
                return prev;
            }
        }
        next
    }
}

/// A display row on screen: a line, or a part of it between soft wraps.
pub struct RowLayout {
    pub line: usize,
    /// The row's place in its line: 0 — the line's first row.
    pub index: usize,
    /// The columns of the line on the row: `start..end` (`end` — the next row's start, or the
    /// line's end).
    pub start: usize,
    pub end: usize,
    /// More rows of the same line follow: the line wraps after this one.
    pub wraps: bool,
    /// Where the row's text starts, from `origin.x`: the indent of a wrapped part.
    pub x: Pixels,
    pub shaped: ShapedLine,
    /// Byte offset in the row's displayed text for each of its columns (+ the end). The displayed
    /// text differs from the source: tabs are expanded to spaces.
    pub char_to_byte: Vec<usize>,
}

impl RowLayout {
    /// The x of the caret at `column` of the line (clamped to the row), from `origin.x`.
    pub fn x_for_column(&self, column: usize) -> Pixels {
        let local =
            (column.clamp(self.start, self.end) - self.start).min(self.char_to_byte.len() - 1);
        self.x + self.shaped.x_for_index(self.char_to_byte[local])
    }

    /// The right edge of the row's text, from `origin.x`.
    pub fn right(&self) -> Pixels {
        self.x + self.shaped.width
    }

    /// The column of the line closest to `x` (from `origin.x`) on this row. A wrapped row's end is
    /// the next row's start: the caret stays before its last character.
    pub fn column_for_x(&self, x: Pixels) -> usize {
        let byte = self.shaped.closest_index_for_x(x - self.x);
        let next = self.char_to_byte.partition_point(|&b| b < byte);
        let local = if next == self.char_to_byte.len() {
            next - 1
        } else if next > 0 && self.char_to_byte[next] != byte {
            // Inside an expanded tab, snap to the nearest edge.
            let prev = next - 1;
            if byte - self.char_to_byte[prev] < self.char_to_byte[next] - byte {
                prev
            } else {
                next
            }
        } else {
            next
        };
        let column = self.start + local;
        if self.wraps && self.end > self.start {
            column.min(self.end - 1)
        } else {
            column
        }
    }
}

/// A part of a text range on one row, in window coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub top: Pixels,
    pub left: Pixels,
    pub right: Pixels,
}

impl LayoutCache {
    /// The top of display row `row`.
    pub fn row_top(&self, row: usize) -> Pixels {
        self.origin.y + self.line_height * row as f32
    }

    fn line_rows(&self, line: usize) -> Option<&LineRows> {
        self.lines.get(line.checked_sub(self.first_line)?)
    }

    /// The top of `line`'s first row. A line off screen gets a top past the screen's edge on its
    /// side, counting a row per line between: enough for what spans to it (a Git change).
    pub fn line_top(&self, line: usize) -> Pixels {
        if let Some(rows) = self.line_rows(line) {
            return self.row_top(rows.row);
        }
        let lh = self.line_height;
        match (self.lines.first(), self.lines.last()) {
            (Some(first), _) if line < self.first_line => {
                self.row_top(first.row) - lh * (self.first_line - line) as f32
            }
            (_, Some(last)) if line > self.first_line => {
                let last_line = self.first_line + self.lines.len() - 1;
                self.row_top(last.row + last.count) + lh * (line - last_line - 1) as f32
            }
            _ => self.row_top(self.first_row),
        }
    }

    /// The bottom of `line`'s last row.
    pub fn line_bottom(&self, line: usize) -> Pixels {
        match self.line_rows(line) {
            Some(rows) => self.row_top(rows.row + rows.count),
            None => self.line_top(line) + self.line_height,
        }
    }

    /// The rows that fit on screen.
    pub fn visible_lines(&self) -> usize {
        (self.text_bounds.size.height / self.line_height)
            .floor()
            .max(1.) as usize
    }

    /// The row on screen the caret at `pos` is on (its index in `rows`); `None` off screen. At a
    /// soft wrap the caret is at the start of the next row.
    pub fn caret_row(&self, text: &Rope, pos: usize) -> Option<usize> {
        let pos = pos.min(text.len_chars());
        let line = text.char_to_line(pos);
        let column = pos - line_start(text, line);
        let rows = self.line_rows(line)?;
        let shown = &self.rows[rows.first..rows.first + rows.shown];
        let index = shown
            .partition_point(|row| row.start <= column)
            .checked_sub(1)?;
        let row = &shown[index];
        // Past this row: on a row below the screen.
        if row.wraps && column >= row.end {
            return None;
        }
        Some(rows.first + index)
    }

    /// Where the caret at `pos` is: the left edge of its character and the top of its row, in
    /// window coordinates.
    pub fn caret_point(&self, text: &Rope, pos: usize) -> Option<Point<Pixels>> {
        let index = self.caret_row(text, pos)?;
        let row = &self.rows[index];
        let column = pos.min(text.len_chars()) - line_start(text, row.line);
        Some(point(
            self.origin.x + row.x_for_column(column),
            self.row_top(self.first_row + index),
        ))
    }

    /// The row on screen at window `y` (its index in `rows`).
    pub fn row_at_y(&self, y: Pixels) -> Option<usize> {
        let row = ((y - self.origin.y) / self.line_height).floor();
        if row < 0. {
            return None;
        }
        (row as usize)
            .checked_sub(self.first_row)
            .filter(|&index| index < self.rows.len())
    }

    /// The line of the row on screen at window `y`.
    pub fn line_at_y(&self, y: Pixels) -> Option<usize> {
        self.row_at_y(y).map(|index| self.rows[index].line)
    }

    /// The text position under a screen point.
    pub fn position_for_point(&self, text: &Rope, p: Point<Pixels>) -> usize {
        let x = p.x - self.origin.x;
        if let Some(index) = self.row_at_y(p.y) {
            let row = &self.rows[index];
            return line_start(text, row.line) + row.column_for_x(x);
        }
        let row = ((p.y - self.origin.y) / self.line_height).floor();
        let last_line = text.len_lines() - 1;
        // Above the screen (the selection is being dragged past the edge): the start of the line
        // there, counting a row per line above the first one.
        if row < self.first_row as f32 {
            let Some(first) = self.lines.first() else {
                return 0;
            };
            if row >= first.row as f32 {
                return line_start(text, self.first_line);
            }
            let above = (first.row as f32 - row).ceil() as usize;
            return line_start(text, self.first_line.saturating_sub(above));
        }
        // Below the rows on screen. Under the end of the text: on its last row, as a click there.
        let (Some(last), Some(last_row)) = (self.lines.last(), self.rows.last()) else {
            return text.len_chars();
        };
        let lines_end = self.first_line + self.lines.len() - 1;
        if lines_end == last_line && !last_row.wraps && last_row.line == last_line {
            return line_start(text, last_line) + last_row.column_for_x(x);
        }
        // Otherwise the end of the line there.
        let end_row = (last.row + last.count) as f32;
        let line = if row < end_row {
            lines_end
        } else {
            (lines_end + 1 + (row - end_row) as usize).min(last_line)
        };
        line_start(text, line) + line_len(text, line)
    }

    /// The rectangle of the character at `pos`, if it is on screen.
    pub fn bounds_for_position(&self, text: &Rope, pos: usize) -> Option<Bounds<Pixels>> {
        let origin = self.caret_point(text, pos)?;
        Some(Bounds::new(
            origin,
            size(px(theme::FONT_SIZE * 0.6), self.line_height),
        ))
    }

    /// The character under a point of the window (the mouse resting there): over a row's text, not
    /// past its end.
    pub fn char_at_point(&self, text: &Rope, p: Point<Pixels>) -> Option<usize> {
        if !self.text_bounds.contains(&p) {
            return None;
        }
        let row = &self.rows[self.row_at_y(p.y)?];
        let x = p.x - self.origin.x - row.x;
        if x < px(0.) {
            return None;
        }
        let byte = row.shaped.index_for_x(x)?;
        let local = row
            .char_to_byte
            .partition_point(|&b| b <= byte)
            .checked_sub(1)?;
        let column = row.start + local;
        (column < row.end).then(|| line_start(text, row.line) + column)
    }

    /// The parts of the text `from..to` on the rows on screen, in window coordinates. A range that
    /// goes past a line's end gets `tail` more after the line's text: the line break selected. A
    /// part that would be empty is left out.
    pub fn segments(&self, text: &Rope, from: usize, to: usize, tail: Pixels) -> Vec<Segment> {
        let mut segments = Vec::new();
        if self.lines.is_empty() {
            return segments;
        }
        let len = text.len_chars();
        let (from, to) = (from.min(len), to.min(len));
        let from_line = text.char_to_line(from);
        let to_line = text.char_to_line(to);
        let lines_end = self.first_line + self.lines.len() - 1;
        for line in from_line.max(self.first_line)..=to_line.min(lines_end) {
            let start = line_start(text, line);
            let Some(rows) = self.line_rows(line) else {
                continue;
            };
            for index in rows.first..rows.first + rows.shown {
                let row = &self.rows[index];
                let (row_from, row_to) = (start + row.start, start + row.end);
                // Ends before the row, or starts after it (a wrapped row's end is the next row's
                // start).
                if to <= row_from || from > row_to || (from == row_to && row.wraps) {
                    continue;
                }
                let left = if from >= row_from {
                    row.x_for_column(from - start)
                } else {
                    row.x
                };
                let right = if to <= row_to {
                    row.x_for_column(to - start)
                } else if row.wraps {
                    row.right()
                } else {
                    row.right() + tail
                };
                if right <= left {
                    continue;
                }
                segments.push(Segment {
                    top: self.row_top(self.first_row + index),
                    left: self.origin.x + left,
                    right: self.origin.x + right,
                });
            }
        }
        segments
    }
}

pub struct EditorElement {
    editor: Entity<Editor>,
}

impl EditorElement {
    pub fn new(editor: Entity<Editor>) -> Self {
        Self { editor }
    }
}

pub struct PrepaintState {
    layout: Option<LayoutCache>,
    gutter: Vec<(ShapedLine, Point<Pixels>)>,
    current_line: Option<PaintQuad>,
    /// Search matches, selections, and the IME underline are drawn under the text, within the text
    /// area.
    highlights: Vec<PaintQuad>,
    cursors: Vec<PaintQuad>,
    /// Language server diagnostics: underlines and line number colors.
    diagnostics: crate::diagnostics::DiagnosticsPaint,
    /// Git change markers in the gutter.
    git: crate::git_gutter::GutterPaint,
    /// Annotations (blame): a column left of the line numbers.
    blame: crate::blame::BlamePaint,
    /// A host view's decorations (the diff viewer): line backgrounds under everything, changed
    /// words under the text.
    decorations: crate::diff_view::DecorationsPaint,
    /// The placeholder of an empty commit message field.
    placeholder: Option<(ShapedLine, Point<Pixels>)>,
    /// Soft-wrap signs (part 10.5): at the end of a wrapped row and before the part that goes on.
    wrap_signs: Vec<(ShapedLine, Point<Pixels>)>,
}

impl IntoElement for EditorElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// The soft-wrap signs, as JetBrains IDEs draw them: before the wrap, and where the line goes on.
const WRAP_SIGN: &str = "\u{2936}";
const CONTINUATION_SIGN: &str = "\u{2937}";

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> PrepaintState {
        let font = font(theme::code_font());
        let font_size = px(theme::FONT_SIZE);
        let line_height = px(theme::LINE_HEIGHT);
        let text_system = window.text_system().clone();
        let em = text_system
            .em_advance(text_system.resolve_font(&font), font_size)
            .unwrap_or(px(8.));

        // The text area: past the gutter (a commit message field has none), before the
        // scrollbar's lane.
        let (gutter_width, blame_width, text_bounds) = {
            let editor = self.editor.read(cx);
            let digits = editor.document.text().len_lines().to_string().len().max(3);
            let blame_width = crate::blame::column_width(&editor.blame, em);
            let gutter_width = if editor.message.is_some() {
                px(0.)
            } else {
                em * (digits + 2) as f32 + blame_width
            };
            let text_bounds = Bounds::from_corners(
                point(bounds.left() + gutter_width, bounds.top()),
                point(
                    bounds.right() - crate::scrollbar::lane_width(editor),
                    bounds.bottom(),
                ),
            );
            (gutter_width, blame_width, text_bounds)
        };

        // Soft wraps: the rows at this width, before anything is placed on them. The text ends a
        // character before the edge: room for the caret and the wrap sign.
        let settings = crate::settings::editor(cx);
        let params = WrapParams {
            width: f32::from(text_bounds.size.width) - theme::TEXT_PADDING - f32::from(em) * 1.5,
            space: f32::from(em),
            keep_indent: settings.soft_wrap_indent,
            extra_indent: settings.soft_wrap_extra_indent,
        };
        let visible_rows = (bounds.size.height / line_height).ceil() as usize + 1;
        let pending = self.editor.update(cx, |editor, cx| {
            crate::wrap::update(
                editor,
                params,
                visible_rows,
                &text_system,
                &font,
                font_size,
                cx,
            );
            editor.wrap.pending()
        });
        // A long document still wrapping at a new width: more of it in the next frame.
        if pending {
            window.request_animation_frame();
        }

        let theme = Theme::get(cx);
        let ui = theme.ui;
        let editor = self.editor.read(cx);
        let text = editor.document.text().clone();
        let selection = editor.document.selection().clone();
        let marked_range = editor.marked_range.clone();
        let mut scroll = editor.scroll;
        let autoscroll = editor.autoscroll;
        let wrap = &editor.wrap;
        let wrapped = wrap.laid_out();

        let total_lines = text.len_lines();
        let total_rows = wrap.row_count();
        let height = f32::from(bounds.size.height);
        let lh = theme::LINE_HEIGHT;

        let primary = selection.primary();
        let head_line = text.char_to_line(primary.head);
        let head_column = primary.head - line_start(&text, head_line);
        let head_row = wrap.line_to_row(head_line) + wrap.row_in_line(head_line, head_column);

        // A glide (a jump on its way) counts from where the view is going; one that something else
        // interrupted (the wheel, the scrollbar, a host) is over.
        let max_scroll = (total_rows - 1) as f32 * lh;
        let shown = scroll.y.clamp(0., max_scroll);
        let glide = editor.glide.filter(|glide| glide.shown == scroll.y);
        let destination = glide.map_or(shown, |glide| glide.to.clamp(0., max_scroll));
        scroll.y = destination;

        // Vertical autoscroll: keep the cursor in the viewport with a margin of a few rows, and
        // when jumping to a match, put a row that is out of view in the middle.
        let top = head_row as f32 * lh;
        match autoscroll {
            Some(Autoscroll::Fit) => {
                let margin = (theme::SCROLL_MARGIN_LINES as f32 * lh)
                    .min((height - lh) / 2.)
                    .max(0.);
                if top - margin < scroll.y {
                    scroll.y = top - margin;
                } else if top + lh + margin > scroll.y + height {
                    scroll.y = top + lh + margin - height;
                }
            }
            Some(Autoscroll::Center) if top < scroll.y || top + lh > scroll.y + height => {
                scroll.y = top - (height - lh) / 2.;
            }
            // In the middle, but with a whole top row: the preview doesn't start with a clipped
            // row.
            Some(Autoscroll::Middle) => scroll.y = ((top - (height - lh) / 2.) / lh).round() * lh,
            Some(Autoscroll::Center) | None => {}
        }
        scroll.y = scroll.y.clamp(0., max_scroll);
        // A jump glides from what is on screen to its place; anything else that moved the view
        // (the caret, typing) takes it there at once; otherwise a glide goes on. Not on the first
        // frame: a file just opened shows its place at once.
        let jump = autoscroll.is_some()
            && editor.glide_next
            && editor.layout.is_some()
            && motion::enabled();
        let glide = Glide::next(
            glide,
            destination,
            scroll.y,
            jump,
            shown,
            height,
            Instant::now(),
        );
        if let Some(glide) = &glide {
            scroll.y = glide.shown.clamp(0., max_scroll);
            window.request_animation_frame();
        }

        let first_row = (scroll.y / lh).floor() as usize;
        let last_row = (first_row + (height / lh).ceil() as usize + 1).min(total_rows);
        let first_line = wrap.row_to_line(first_row);
        let last_line = (wrap.row_to_line(last_row.saturating_sub(1).max(first_row)) + 1)
            .min(total_lines)
            .max(first_line + 1);

        let run = |len: usize, color| TextRun {
            len,
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };

        // The rows on screen: highlighting of their lines in character columns → bytes of the
        // displayed text of each row.
        let highlights = editor
            .highlighter
            .highlight_lines(&text, first_line..last_line);
        let base = run(0, ui.foreground);
        let mut rows = Vec::with_capacity(last_row.saturating_sub(first_row));
        let mut lines = Vec::with_capacity(last_line - first_line);
        let mut max_width = px(0.);
        for line in first_line..last_line {
            let spans = highlights
                .get(line - first_line)
                .map_or(&[][..], Vec::as_slice);
            let line_row = wrap.line_to_row(line);
            let count = wrap.line_rows(line);
            let mut entry = LineRows {
                row: line_row,
                count,
                first: rows.len(),
                shown: 0,
            };
            for index in 0..count {
                let row = line_row + index;
                if row < first_row || row >= last_row {
                    continue;
                }
                let span = wrap.row_span(&text, line, index);
                let (display, char_to_byte, runs) = if count == 1 {
                    let (display, char_to_byte) = display_line(&text, line);
                    let runs = text_runs(spans, &char_to_byte, &base, |h| theme.syntax_style(h));
                    (display, char_to_byte, runs)
                } else {
                    let (display, char_to_byte) =
                        display_range(&text, line, span.start, span.end, span.display as usize);
                    let spans = row_spans(spans, span.start, span.end);
                    let runs = text_runs(&spans, &char_to_byte, &base, |h| theme.syntax_style(h));
                    (display, char_to_byte, runs)
                };
                let shaped = text_system.shape_line(display.into(), font_size, &runs, None);
                max_width = max_width.max(shaped.width);
                rows.push(RowLayout {
                    line,
                    index,
                    start: span.start,
                    end: span.end,
                    wraps: span.wraps,
                    x: px(span.x),
                    shaped,
                    char_to_byte,
                });
                entry.shown += 1;
            }
            lines.push(entry);
        }

        // Horizontal autoscroll: after the rows are laid out, when the cursor's x is known. With
        // soft wraps the text fits the width: no horizontal scroll.
        let visible_width = f32::from(text_bounds.size.width) - theme::TEXT_PADDING * 2.;
        if wrapped {
            scroll.x = 0.;
        } else {
            if autoscroll.is_some()
                && let Some(entry) = lines.get(head_line.wrapping_sub(first_line))
                && entry.shown > 0
            {
                let x = f32::from(rows[entry.first].x_for_column(head_column));
                let margin = f32::from(em) * 4.;
                if x < scroll.x {
                    scroll.x = x - margin;
                } else if x > scroll.x + visible_width - f32::from(em) {
                    scroll.x = x - visible_width + margin;
                }
            }
            scroll.x = scroll
                .x
                .clamp(0., (f32::from(max_width) - visible_width / 2.).max(0.));
        }

        let layout = LayoutCache {
            text_bounds,
            line_height,
            origin: point(
                text_bounds.left() + px(theme::TEXT_PADDING - scroll.x),
                bounds.top() - px(scroll.y),
            ),
            first_line,
            first_row,
            rows,
            lines,
        };

        // The caret's line, all its rows.
        let current_line = (selection.len() == 1 && primary.is_empty() && editor.message.is_none())
            .then(|| {
                let top = layout.line_top(head_line);
                fill(
                    Bounds::from_corners(
                        point(bounds.left(), top),
                        point(bounds.right(), layout.line_bottom(head_line)),
                    ),
                    ui.current_line,
                )
            });

        let mut highlights = Vec::new();
        let newline_width = em * 0.5;
        // Quads for the range `from..to` over the rows on screen; a line break inside the range is
        // a "tail" past the end of the line.
        let mut range_quads = |from: usize, to: usize, color: Hsla| {
            for segment in layout.segments(&text, from, to, newline_width) {
                highlights.push(fill(
                    Bounds::from_corners(
                        point(segment.left, segment.top),
                        point(segment.right, segment.top + line_height),
                    ),
                    color,
                ));
            }
        };

        // Search matches: only those that intersect the visible lines. The current match goes on
        // top of the selection (it is usually the selected one): otherwise the selection's blue
        // blends with its color.
        let search = &editor.search;
        let visible_start = line_start(&text, first_line);
        let visible_end = if last_line < total_lines {
            line_start(&text, last_line)
        } else {
            text.len_chars()
        };
        let first_match = search.matches.partition_point(|m| m.end <= visible_start);
        for (i, m) in search.matches.iter().enumerate().skip(first_match) {
            if m.start >= visible_end {
                break;
            }
            if search.active != Some(i) {
                range_quads(m.start, m.end, ui.search_match);
            }
        }
        for range in selection.iter().filter(|r| !r.is_empty()) {
            range_quads(range.from(), range.to(), ui.selection);
        }
        if let Some(m) = search.active.and_then(|i| search.matches.get(i)) {
            range_quads(m.start, m.end, ui.search_match_active);
        }
        if let Some(marked) = marked_range {
            for segment in layout.segments(&text, marked.start, marked.end, px(0.)) {
                highlights.push(fill(
                    Bounds::from_corners(
                        point(segment.left, segment.top + line_height - px(2.)),
                        point(segment.right, segment.top + line_height - px(1.)),
                    ),
                    ui.foreground,
                ));
            }
        }

        let cursors = selection
            .iter()
            .filter_map(|range| {
                let origin = layout.caret_point(&text, range.head)?;
                Some(fill(
                    Bounds::new(
                        point(origin.x - px(1.), origin.y),
                        size(px(2.), line_height),
                    ),
                    ui.cursor,
                ))
            })
            .collect();

        // Soft-wrap signs: on the caret's lines only, unless Settings → Editor shows them all
        // (JetBrains: "Only show soft-wrap indicators for the current line").
        let mut wrap_signs = Vec::new();
        if wrapped && editor.message.is_none() {
            let caret_lines: Vec<usize> = selection
                .iter()
                .map(|range| text.char_to_line(range.head.min(text.len_chars())))
                .collect();
            let shown = |line: usize| {
                !settings.soft_wrap_signs_current_line_only || caret_lines.contains(&line)
            };
            let sign = |sign: &'static str| {
                text_system.shape_line(sign.into(), font_size, &[run(sign.len(), ui.dim)], None)
            };
            let (end_sign, go_on_sign) = (sign(WRAP_SIGN), sign(CONTINUATION_SIGN));
            let gap = em * 0.2;
            for (index, row) in layout.rows.iter().enumerate() {
                if !shown(row.line) {
                    continue;
                }
                let top = layout.row_top(first_row + index);
                if row.wraps {
                    let x = layout.origin.x + row.right() + gap;
                    wrap_signs.push((end_sign.clone(), point(x, top)));
                }
                if row.index > 0 && row.x >= go_on_sign.width + gap {
                    let x = layout.origin.x + row.x - go_on_sign.width - gap;
                    wrap_signs.push((go_on_sign.clone(), point(x, top)));
                }
            }
        }

        let diagnostics =
            crate::diagnostics::prepaint(&editor.diagnostics, &text, &layout, last_line, em, &ui);
        let gutter_bounds = Bounds::from_corners(
            bounds.origin,
            point(bounds.left() + gutter_width, bounds.bottom()),
        );
        let git = crate::git_gutter::prepaint(
            &editor.git,
            &layout,
            last_line,
            gutter_bounds,
            em,
            &ui,
            window,
        );
        let blame_column = Bounds::new(bounds.origin, size(blame_width, bounds.size.height));
        let blame =
            crate::blame::prepaint(&editor.blame, &layout, last_line, blame_column, &ui, window);
        let placeholder = editor
            .message
            .as_ref()
            .filter(|_| text.len_chars() == 0)
            .map(|placeholder| {
                let runs = [run(placeholder.len(), ui.dim)];
                let shaped = text_system.shape_line(placeholder.clone(), font_size, &runs, None);
                (shaped, layout.origin)
            });

        // Line numbers: on the first row of each line (its wrapped parts have none, as in
        // JetBrains IDEs).
        let gutter = if gutter_width > px(0.) {
            (first_line..last_line)
                .filter_map(|line| {
                    let rows = layout.line_rows(line)?;
                    if rows.row < first_row {
                        return None;
                    }
                    let number = (line + 1).to_string();
                    let color = match diagnostics.number_color(line) {
                        Some(color) => color,
                        None if line == head_line => ui.foreground,
                        None => ui.dim,
                    };
                    let runs = [run(number.len(), color)];
                    let shaped = text_system.shape_line(number.into(), font_size, &runs, None);
                    let x = bounds.left() + gutter_width - em - shaped.width;
                    Some((shaped, point(x, layout.row_top(rows.row))))
                })
                .collect()
        } else {
            Vec::new()
        };

        let decorations = self.editor.update(cx, |editor, _| {
            editor.scroll = scroll;
            editor.glide = glide.map(|glide| Glide {
                shown: scroll.y,
                ..glide
            });
            editor.glide_next = false;
            editor.autoscroll = None;
            editor.blame.column = (blame_width > px(0.)).then_some(blame_column);
            editor.frame_decorations.take()
        });
        let decorations = crate::diff_view::prepaint_decorations(
            decorations.as_deref(),
            &text,
            &layout,
            last_line,
            bounds,
            &ui,
        );

        PrepaintState {
            layout: Some(layout),
            gutter,
            current_line,
            highlights,
            cursors,
            diagnostics,
            git,
            blame,
            decorations,
            placeholder,
            wrap_signs,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        state: &mut PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let editor = self.editor.read(cx);
        let focus_handle = editor.focus_handle.clone();
        // Cursors: only when focused and in the "visible" phase of the blink.
        let show_cursors = editor.cursor_visible && focus_handle.is_focused(window);
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );

        let layout = state
            .layout
            .take()
            .expect("prepaint always produces a layout");
        let line_height = layout.line_height;

        state.decorations.paint_lines(window);
        if let Some(quad) = state.current_line.take() {
            window.paint_quad(quad);
        }
        for (number, origin) in &state.gutter {
            number.paint(*origin, line_height, window, cx).ok();
        }
        state.git.paint(window);
        state.blame.paint(line_height, window, cx);

        let mask = ContentMask {
            bounds: layout.text_bounds,
        };
        window.with_content_mask(Some(mask), |window| {
            state.decorations.paint_words(window);
            if let Some((placeholder, origin)) = &state.placeholder {
                placeholder.paint(*origin, line_height, window, cx).ok();
            }
            for quad in state.highlights.drain(..) {
                window.paint_quad(quad);
            }
            for (index, row) in layout.rows.iter().enumerate() {
                let origin = point(
                    layout.origin.x + row.x,
                    layout.row_top(layout.first_row + index),
                );
                row.shaped.paint(origin, line_height, window, cx).ok();
            }
            for (sign, origin) in &state.wrap_signs {
                sign.paint(*origin, line_height, window, cx).ok();
            }
            state.diagnostics.paint_underlines(window);
            if show_cursors {
                for cursor in state.cursors.drain(..) {
                    window.paint_quad(cursor);
                }
            }
        });

        self.editor
            .update(cx, |editor, _| editor.layout = Some(layout));
    }
}
