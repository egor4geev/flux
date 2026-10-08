//! Terminal drawing. Each frame takes a fresh [`flux_term::Content`] snapshot of the visible rows
//! and draws, bottom to top: cell backgrounds, the selection and search matches, the text, its
//! underlines and strike-throughs, the link under the mouse, the cursor, an IME composition, and a
//! scroll thumb while the view is scrolled back. The grid follows the element's size (the program
//! gets SIGWINCH).
//!
//! Text is laid out in runs of adjacent cells with the same style, each placed at its first cell,
//! so the font's own advances never shift the grid: a character that doesn't take exactly one cell
//! in the code font (a wide CJK or emoji character, a fallback glyph such as a Nerd Font icon in a
//! prompt) gets a run of its own, and what follows starts at its own cell again.

use std::cell::RefCell;
use std::collections::HashMap;

use flux_term::{
    Content, CursorShape, GridPoint, RenderCell, Rgb, Side, TermMode, TermSize, Underline,
};
use gpui::{
    App, BorderStyle, Bounds, Element, ElementId, ElementInputHandler, Entity, Font, FontStyle,
    FontWeight, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, PaintQuad,
    Pixels, Point, ShapedLine, SharedString, StrikethroughStyle, Style, TextRun, UnderlineStyle,
    Window, fill, font, outline, point, px, relative, rgb, size,
};

use crate::terminal_view::TerminalView;
use crate::theme::{self, Theme, UiColors};

/// The grid's geometry in the last frame: the mouse and the IME map window points to cells with it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerminalLayout {
    /// Top-left corner of the grid in window coordinates.
    pub origin: Point<Pixels>,
    pub cell_width: Pixels,
    pub line_height: Pixels,
    pub columns: usize,
    pub rows: usize,
    /// How far back the view was scrolled: viewport row `r` shows grid line `r - display_offset`.
    pub display_offset: usize,
    /// The cursor cell (viewport row, column), when it is on screen.
    pub cursor: Option<(usize, usize)>,
    /// The terminal modes in that frame: whether the program takes the mouse.
    pub mode: TermMode,
}

impl TerminalLayout {
    /// The grid point under a window point and the half of the cell it falls on. Points outside the
    /// grid go to the nearest cell (dragging a selection past the edge).
    pub fn grid_point(&self, position: Point<Pixels>) -> (GridPoint, Side) {
        let x = (position.x - self.origin.x) / self.cell_width;
        let (row, column) = self.viewport_cell(position);
        let side = if x - column as f32 >= 0.5 {
            Side::Right
        } else {
            Side::Left
        };
        let line = row as i32 - self.display_offset as i32;
        (GridPoint::new(line, column), side)
    }

    /// The viewport cell (row, column) under a window point, clamped to the grid: mouse reports
    /// address cells of the screen, not of the scrollback.
    pub fn viewport_cell(&self, position: Point<Pixels>) -> (usize, usize) {
        let x = (position.x - self.origin.x) / self.cell_width;
        let y = (position.y - self.origin.y) / self.line_height;
        let column = (x.max(0.) as usize).min(self.columns.saturating_sub(1));
        let row = (y.max(0.) as usize).min(self.rows.saturating_sub(1));
        (row, column)
    }

    /// How many rows above (negative) or below (positive) the grid a window point is; 0 inside.
    /// Dragging a selection past an edge scrolls that fast.
    pub fn rows_outside(&self, position: Point<Pixels>) -> i32 {
        let top = self.origin.y;
        let bottom = top + self.line_height * self.rows as f32;
        if position.y < top {
            -(((top - position.y) / self.line_height).floor() as i32 + 1)
        } else if position.y >= bottom {
            ((position.y - bottom) / self.line_height).floor() as i32 + 1
        } else {
            0
        }
    }

    /// The rectangle of `width` cells starting at a viewport cell.
    pub fn cell_bounds(&self, row: usize, column: usize, width: usize) -> Bounds<Pixels> {
        Bounds::new(
            point(
                self.origin.x + self.cell_width * column as f32,
                self.origin.y + self.line_height * row as f32,
            ),
            size(self.cell_width * width as f32, self.line_height),
        )
    }

    /// The cursor cell: the IME puts its candidate window there.
    pub fn cursor_bounds(&self) -> Option<Bounds<Pixels>> {
        let (row, column) = self.cursor?;
        Some(self.cell_bounds(row, column, 1))
    }
}

pub struct TerminalElement {
    view: Entity<TerminalView>,
}

impl TerminalElement {
    pub fn new(view: Entity<TerminalView>) -> Self {
        Self { view }
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// A line under or through text: an underline style or a strike-through.
struct Decoration {
    origin: Point<Pixels>,
    width: Pixels,
    color: Hsla,
    kind: DecorationKind,
}

#[derive(Clone, Copy, PartialEq)]
enum DecorationKind {
    Line,
    Wavy,
    Strike,
}

/// The text of the cell under a block cursor, drawn over the cursor in the background color.
struct CursorGlyph {
    origin: Point<Pixels>,
    line: ShapedLine,
}

/// An IME composition at the cursor: an opaque backdrop, the text, and an underline.
struct Composition {
    backdrop: PaintQuad,
    origin: Point<Pixels>,
    line: ShapedLine,
    underline: Decoration,
}

pub struct PrepaintState {
    layout: TerminalLayout,
    /// Cell backgrounds, then the selection and the search matches: all under the text.
    backgrounds: Vec<PaintQuad>,
    /// Text runs with their origins.
    runs: Vec<(Point<Pixels>, ShapedLine)>,
    decorations: Vec<Decoration>,
    cursor: Option<PaintQuad>,
    cursor_glyph: Option<CursorGlyph>,
    composition: Option<Composition>,
    scroll_thumb: Option<PaintQuad>,
}

/// Font metrics of the grid.
struct Metrics {
    font: Font,
    font_size: Pixels,
    cell_width: Pixels,
    line_height: Pixels,
    /// From the top of a row: an underline and a strike-through.
    underline: Pixels,
    strike: Pixels,
}

impl Metrics {
    fn new(window: &Window) -> Self {
        let font = font(theme::code_font());
        let font_size = px(theme::TERMINAL_FONT_SIZE);
        let line_height = px(theme::TERMINAL_LINE_HEIGHT);
        let text_system = window.text_system().clone();
        let cell_width = text_system
            .em_advance(text_system.resolve_font(&font), font_size)
            .unwrap_or(px(8.));
        // The same baseline the text gets when a line is painted in a row of this height.
        let reference = text_system.shape_line("M".into(), font_size, &[run(&font, 1)], None);
        let ascent = reference.ascent;
        let descent = reference.descent;
        let baseline = (line_height - ascent - descent) / 2. + ascent;
        Self {
            font,
            font_size,
            cell_width,
            line_height,
            underline: baseline + descent * 0.618,
            strike: baseline - ascent * 0.3,
        }
    }
}

impl Element for TerminalElement {
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
        let theme = Theme::get(cx);
        let ui = theme.ui;
        let colors = theme.terminal;
        let metrics = Metrics::new(window);

        let columns = ((bounds.size.width / metrics.cell_width).floor() as usize).max(2);
        let rows = ((bounds.size.height / metrics.line_height).floor() as usize).max(1);
        let view = self.view.read(cx);
        view.terminal.resize(TermSize {
            columns: columns.min(u16::MAX as usize) as u16,
            rows: rows.min(u16::MAX as usize) as u16,
            cell_width: f32::from(metrics.cell_width).round() as u16,
            cell_height: f32::from(metrics.line_height).round() as u16,
        });
        let content = view.terminal.content();
        let focused = view.focus_handle.is_focused(window) && window.is_window_active();
        // While a composition is shown and after the shell exited, there is no cursor.
        let cursor = content
            .cursor
            .filter(|_| view.exited.is_none() && view.marked_text.is_none());

        let layout = TerminalLayout {
            origin: bounds.origin,
            cell_width: metrics.cell_width,
            line_height: metrics.line_height,
            columns: content.columns,
            rows: content.rows,
            display_offset: content.display_offset,
            cursor: content.cursor.map(|cursor| (cursor.row, cursor.column)),
            mode: content.mode,
        };

        // A solid block cursor is drawn over its cell, and the cell's text over it in the
        // background color: that cell is left out of the regular text runs.
        let solid_block = cursor
            .filter(|cursor| focused && view.cursor_visible && cursor.shape == CursorShape::Block);
        let block_cell = solid_block.map(|cursor| (cursor.row, cursor.column));

        let mut backgrounds = Vec::new();
        cell_backgrounds(&content, &layout, &mut backgrounds);
        selection_quads(&content, &layout, ui.selection, &mut backgrounds);
        let search = &view.search;
        match_quads(
            &content,
            &layout,
            &search.matches,
            search.active,
            (ui.search_match, ui.search_match_active),
            &mut backgrounds,
        );

        let runs = text_runs(&content, &layout, &metrics, block_cell, window);
        let mut decorations = text_decorations(&content, &layout, &metrics);
        if let Some(link) = &view.links.hovered {
            link_underline(
                &content,
                &layout,
                &metrics,
                link.start,
                link.end,
                ui.accent_text,
                &mut decorations,
            );
        }

        let cursor_color: Hsla = colors.cursor;
        let cursor_quad = cursor.and_then(|cursor| {
            let width = if cursor.wide { 2 } else { 1 };
            let cell = layout.cell_bounds(cursor.row, cursor.column, width);
            if !focused {
                return Some(outline(
                    cell,
                    UiColors::tint(cursor_color, 0.8),
                    BorderStyle::Solid,
                ));
            }
            if !view.cursor_visible {
                return None;
            }
            Some(match cursor.shape {
                CursorShape::Block => fill(cell, cursor_color),
                CursorShape::Beam => fill(
                    Bounds::new(cell.origin, size(px(2.), cell.size.height)),
                    cursor_color,
                ),
                CursorShape::Underline => fill(
                    Bounds::new(
                        point(cell.left(), cell.bottom() - px(2.)),
                        size(cell.size.width, px(2.)),
                    ),
                    cursor_color,
                ),
            })
        });
        let cursor_glyph = solid_block.and_then(|cursor| {
            let cell = cell_at(&content, cursor.row, cursor.column)?;
            if cell.c == ' ' && cell.zerowidth.is_none() {
                return None;
            }
            let text: String = std::iter::once(cell.c)
                .chain(cell.zerowidth.iter().flatten().copied())
                .collect();
            let style = styled_font(&metrics.font, cell.flags.bold, cell.flags.italic);
            let line = shape(window, text, &style, colors.background, &metrics);
            let origin = layout.cell_bounds(cursor.row, cursor.column, 1).origin;
            Some(CursorGlyph { origin, line })
        });

        let composition = view.marked_text.as_ref().and_then(|text| {
            let (row, column) = layout.cursor?;
            let line = shape(
                window,
                text.clone(),
                &metrics.font,
                colors.foreground,
                &metrics,
            );
            let cells = (line.width / metrics.cell_width).ceil().max(1.) as usize;
            let cell = layout.cell_bounds(row, column, cells);
            let underline = Decoration {
                origin: point(cell.left(), cell.top() + metrics.underline),
                width: line.width,
                color: colors.foreground,
                kind: DecorationKind::Line,
            };
            Some(Composition {
                backdrop: fill(cell, UiColors::tint(colors.background, 1.)),
                origin: cell.origin,
                line,
                underline,
            })
        });

        let scroll_thumb = scroll_thumb(&content, bounds, ui.foreground);

        PrepaintState {
            layout,
            backgrounds,
            runs,
            decorations,
            cursor: cursor_quad,
            cursor_glyph,
            composition,
            scroll_thumb,
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
        let focus_handle = self.view.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
        let line_height = state.layout.line_height;
        for quad in state.backgrounds.drain(..) {
            window.paint_quad(quad);
        }
        for (origin, line) in &state.runs {
            line.paint(*origin, line_height, window, cx).ok();
        }
        for decoration in &state.decorations {
            paint_decoration(decoration, window);
        }
        if let Some(cursor) = state.cursor.take() {
            window.paint_quad(cursor);
        }
        if let Some(glyph) = &state.cursor_glyph {
            glyph.line.paint(glyph.origin, line_height, window, cx).ok();
        }
        if let Some(composition) = state.composition.take() {
            window.paint_quad(composition.backdrop);
            composition
                .line
                .paint(composition.origin, line_height, window, cx)
                .ok();
            paint_decoration(&composition.underline, window);
        }
        if let Some(thumb) = state.scroll_thumb.take() {
            window.paint_quad(thumb);
        }
        let layout = state.layout;
        self.view.update(cx, |view, _| view.layout = Some(layout));
    }
}

fn paint_decoration(decoration: &Decoration, window: &mut Window) {
    match decoration.kind {
        DecorationKind::Line | DecorationKind::Wavy => window.paint_underline(
            decoration.origin,
            decoration.width,
            &UnderlineStyle {
                thickness: px(1.),
                color: Some(decoration.color),
                wavy: decoration.kind == DecorationKind::Wavy,
            },
        ),
        DecorationKind::Strike => window.paint_strikethrough(
            decoration.origin,
            decoration.width,
            &StrikethroughStyle {
                thickness: px(1.),
                color: Some(decoration.color),
            },
        ),
    }
}

pub(crate) fn color(rgb_color: Rgb) -> Hsla {
    let hex =
        (u32::from(rgb_color.r) << 16) | (u32::from(rgb_color.g) << 8) | u32::from(rgb_color.b);
    rgb(hex).into()
}

fn run(font: &Font, len: usize) -> TextRun {
    TextRun {
        len,
        font: font.clone(),
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

fn styled_font(base: &Font, bold: bool, italic: bool) -> Font {
    Font {
        weight: if bold {
            FontWeight::BOLD
        } else {
            FontWeight::NORMAL
        },
        style: if italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        },
        ..base.clone()
    }
}

fn shape(
    window: &mut Window,
    text: impl Into<SharedString>,
    font: &Font,
    color: Hsla,
    metrics: &Metrics,
) -> ShapedLine {
    let text = text.into();
    let style = TextRun {
        color,
        ..run(font, text.len())
    };
    window
        .text_system()
        .shape_line(text, metrics.font_size, &[style], None)
}

/// The cell at a viewport position, if it has anything to draw (cells come row by row, left to
/// right).
fn cell_at(content: &Content, row: usize, column: usize) -> Option<&RenderCell> {
    content
        .cells
        .binary_search_by(|cell| (cell.row, cell.column).cmp(&(row, column)))
        .ok()
        .map(|index| &content.cells[index])
}

/// Cell backgrounds, merged into one rectangle per stretch of adjacent cells of the same color.
fn cell_backgrounds(content: &Content, layout: &TerminalLayout, quads: &mut Vec<PaintQuad>) {
    // (row, first column, columns, color)
    let mut span: Option<(usize, usize, usize, Rgb)> = None;
    let flush = |span: Option<(usize, usize, usize, Rgb)>, quads: &mut Vec<PaintQuad>| {
        if let Some((row, column, width, bg)) = span {
            quads.push(fill(layout.cell_bounds(row, column, width), color(bg)));
        }
    };
    for cell in &content.cells {
        let Some(bg) = cell.bg else {
            continue;
        };
        let width = if cell.flags.wide { 2 } else { 1 };
        match &mut span {
            Some((row, column, columns, color))
                if *row == cell.row && *column + *columns == cell.column && *color == bg =>
            {
                *columns += width;
            }
            _ => {
                flush(span.take(), quads);
                span = Some((cell.row, cell.column, width, bg));
            }
        }
    }
    flush(span, quads);
}

/// The columns of viewport row `row` covered by a range from `start` to `end` (grid points,
/// inclusive) that runs through whole lines in between.
fn row_span(
    content: &Content,
    row: usize,
    start: GridPoint,
    end: GridPoint,
) -> Option<(usize, usize)> {
    let line = content.grid_line(row);
    if line < start.line || line > end.line {
        return None;
    }
    let last_column = content.columns.saturating_sub(1);
    let from = if line == start.line { start.column } else { 0 };
    let to = if line == end.line {
        end.column.min(last_column)
    } else {
        last_column
    };
    (from <= to).then_some((from, to))
}

/// The selected cells of each visible row, as one rectangle per row.
fn selection_quads(
    content: &Content,
    layout: &TerminalLayout,
    color: Hsla,
    quads: &mut Vec<PaintQuad>,
) {
    let Some(selection) = content.selection else {
        return;
    };
    for row in 0..content.rows {
        let span = if selection.block {
            let line = content.grid_line(row);
            (selection.start.line..=selection.end.line)
                .contains(&line)
                .then(|| {
                    (
                        selection.start.column.min(selection.end.column),
                        selection.start.column.max(selection.end.column),
                    )
                })
        } else {
            row_span(content, row, selection.start, selection.end)
        };
        if let Some((from, to)) = span {
            quads.push(fill(layout.cell_bounds(row, from, to - from + 1), color));
        }
    }
}

/// Search matches on the visible rows; the current one brighter and over the others.
fn match_quads(
    content: &Content,
    layout: &TerminalLayout,
    matches: &[flux_term::SearchMatch],
    active: Option<usize>,
    (color, active_color): (Hsla, Hsla),
    quads: &mut Vec<PaintQuad>,
) {
    if matches.is_empty() {
        return;
    }
    let top = content.grid_line(0);
    let bottom = content.grid_line(content.rows.saturating_sub(1));
    // Matches are in order: skip those that end above the screen, stop below it.
    let first = matches.partition_point(|m| m.end.line < top);
    let push = |m: &flux_term::SearchMatch, color: Hsla, quads: &mut Vec<PaintQuad>| {
        for line in m.start.line.max(top)..=m.end.line.min(bottom) {
            let Some(row) = content.viewport_row(line) else {
                continue;
            };
            if let Some((from, to)) = row_span(content, row, m.start, m.end) {
                quads.push(fill(layout.cell_bounds(row, from, to - from + 1), color));
            }
        }
    };
    for (index, m) in matches.iter().enumerate().skip(first) {
        if m.start.line > bottom {
            break;
        }
        if Some(index) != active {
            push(m, color, quads);
        }
    }
    if let Some(m) = active.and_then(|index| matches.get(index)) {
        push(m, active_color, quads);
    }
}

thread_local! {
    /// Whether a non-ASCII character takes exactly one cell in the code font; measured once.
    static FITS_CELL: RefCell<HashMap<char, bool>> = RefCell::new(HashMap::new());
}

/// Whether `c` takes exactly one cell in the code font, so it can share a run with its neighbors
/// without shifting them. ASCII always does; anything else is measured once: a character the font
/// lacks comes from a fallback font (a Nerd Font icon, a symbol) with an advance of its own.
fn fits_cell(c: char, metrics: &Metrics, window: &Window) -> bool {
    if c.is_ascii() {
        return true;
    }
    FITS_CELL.with(|cache| {
        *cache.borrow_mut().entry(c).or_insert_with(|| {
            let text_system = window.text_system();
            let font_id = text_system.resolve_font(&metrics.font);
            text_system
                .advance(font_id, metrics.font_size, c)
                .is_ok_and(|advance| (advance.width - metrics.cell_width).abs() < px(0.25))
        })
    })
}

/// Runs of adjacent cells with the same style, each shaped on its own and placed at its first
/// cell. Blank cells are skipped; a wide character or one that doesn't fit a cell exactly gets a
/// run of its own. `skip` is the cell under a solid block cursor, drawn separately.
fn text_runs(
    content: &Content,
    layout: &TerminalLayout,
    metrics: &Metrics,
    skip: Option<(usize, usize)>,
    window: &mut Window,
) -> Vec<(Point<Pixels>, ShapedLine)> {
    struct Run {
        row: usize,
        column: usize,
        /// The column a cell must be at to join; `None` — the run takes no more cells.
        next_column: Option<usize>,
        text: String,
        fg: Rgb,
        bold: bool,
        italic: bool,
    }
    let mut runs: Vec<Run> = Vec::new();
    for cell in &content.cells {
        if (cell.c == ' ' && cell.zerowidth.is_none()) || skip == Some((cell.row, cell.column)) {
            continue;
        }
        let alone = cell.flags.wide || !fits_cell(cell.c, metrics, window);
        let joins = !alone
            && runs.last().is_some_and(|run| {
                run.row == cell.row
                    && run.next_column == Some(cell.column)
                    && run.fg == cell.fg
                    && run.bold == cell.flags.bold
                    && run.italic == cell.flags.italic
            });
        if !joins {
            runs.push(Run {
                row: cell.row,
                column: cell.column,
                next_column: None,
                text: String::new(),
                fg: cell.fg,
                bold: cell.flags.bold,
                italic: cell.flags.italic,
            });
        }
        let run = runs.last_mut().expect("a run was just pushed");
        run.text.push(cell.c);
        run.text.extend(cell.zerowidth.iter().flatten());
        run.next_column = (!alone).then_some(cell.column + 1);
    }

    runs.into_iter()
        .map(|run| {
            let font = styled_font(&metrics.font, run.bold, run.italic);
            let line = shape(window, run.text, &font, color(run.fg), metrics);
            (layout.cell_bounds(run.row, run.column, 1).origin, line)
        })
        .collect()
}

/// Underlines and strike-throughs of the cells, one line per stretch of adjacent cells with the
/// same style and color (blank cells included: an underlined gap stays underlined).
fn text_decorations(
    content: &Content,
    layout: &TerminalLayout,
    metrics: &Metrics,
) -> Vec<Decoration> {
    // (row, first column, columns, color, style)
    type Span = (usize, usize, usize, Rgb, Underline);
    let mut underlines: Vec<Span> = Vec::new();
    let mut strikes: Vec<Span> = Vec::new();
    let extend = |spans: &mut Vec<Span>, cell: &RenderCell, style: Underline| {
        let width = if cell.flags.wide { 2 } else { 1 };
        match spans.last_mut() {
            Some((row, column, columns, fg, kind))
                if *row == cell.row
                    && *column + *columns == cell.column
                    && *fg == cell.fg
                    && *kind == style =>
            {
                *columns += width
            }
            _ => spans.push((cell.row, cell.column, width, cell.fg, style)),
        }
    };
    for cell in &content.cells {
        if cell.flags.underline != Underline::None {
            extend(&mut underlines, cell, cell.flags.underline);
        }
        if cell.flags.strikeout {
            extend(&mut strikes, cell, Underline::Single);
        }
    }

    let mut decorations = Vec::new();
    for (row, column, columns, fg, style) in underlines {
        let cell = layout.cell_bounds(row, column, columns);
        let y = cell.top() + metrics.underline;
        let line = |y: Pixels, kind: DecorationKind| Decoration {
            origin: point(cell.left(), y),
            width: cell.size.width,
            color: color(fg),
            kind,
        };
        match style {
            Underline::None => {}
            Underline::Single => decorations.push(line(y, DecorationKind::Line)),
            Underline::Curly => decorations.push(line(y - px(1.), DecorationKind::Wavy)),
            Underline::Double => {
                decorations.push(line(y - px(1.), DecorationKind::Line));
                decorations.push(line(y + px(1.), DecorationKind::Line));
            }
            // Short segments along the cells: two per cell for dots, one for dashes.
            Underline::Dotted | Underline::Dashed => {
                let pieces_per_cell = if style == Underline::Dotted { 4 } else { 2 };
                let piece = metrics.cell_width / pieces_per_cell as f32;
                for i in (0..columns * pieces_per_cell).step_by(2) {
                    decorations.push(Decoration {
                        origin: point(cell.left() + piece * i as f32, y),
                        width: piece,
                        color: color(fg),
                        kind: DecorationKind::Line,
                    });
                }
            }
        }
    }
    for (row, column, columns, fg, _) in strikes {
        let cell = layout.cell_bounds(row, column, columns);
        decorations.push(Decoration {
            origin: point(cell.left(), cell.top() + metrics.strike),
            width: cell.size.width,
            color: color(fg),
            kind: DecorationKind::Strike,
        });
    }
    decorations
}

/// The link under the mouse (⌘ held): underlined along its cells, row by row.
fn link_underline(
    content: &Content,
    layout: &TerminalLayout,
    metrics: &Metrics,
    start: GridPoint,
    end: GridPoint,
    color: Hsla,
    decorations: &mut Vec<Decoration>,
) {
    for row in 0..content.rows {
        if let Some((from, to)) = row_span(content, row, start, end) {
            let cell = layout.cell_bounds(row, from, to - from + 1);
            decorations.push(Decoration {
                origin: point(cell.left(), cell.top() + metrics.underline),
                width: cell.size.width,
                color,
                kind: DecorationKind::Line,
            });
        }
    }
}

/// While the view is scrolled back: a thin thumb at the right edge that shows where the screen is
/// in the scrollback. It sits in the padding to the right of the grid.
fn scroll_thumb(content: &Content, bounds: Bounds<Pixels>, color: Hsla) -> Option<PaintQuad> {
    if content.display_offset == 0 || content.history_size == 0 {
        return None;
    }
    const WIDTH: f32 = 3.;
    const MIN_HEIGHT: f32 = 24.;
    let height = f32::from(bounds.size.height);
    let total = (content.history_size + content.rows) as f32;
    let thumb = (height * content.rows as f32 / total)
        .max(MIN_HEIGHT)
        .min(height);
    let scrolled = (content.history_size - content.display_offset.min(content.history_size)) as f32;
    let top = (height - thumb) * scrolled / content.history_size as f32;
    let quad = fill(
        Bounds::new(
            point(bounds.right() + px(2.), bounds.top() + px(top)),
            size(px(WIDTH), px(thumb)),
        ),
        UiColors::tint(color, 0.3),
    );
    Some(quad.corner_radii(px(WIDTH / 2.)))
}
