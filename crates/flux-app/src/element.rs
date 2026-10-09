//! Editor rendering. Each frame draws only the visible lines, so the cost of a frame doesn't depend
//! on the file size.

use flux_core::Rope;
use flux_core::text::{line_len, line_start};
use gpui::{
    App, Bounds, ContentMask, Element, ElementId, ElementInputHandler, Entity, GlobalElementId,
    Hsla, InspectorElementId, IntoElement, LayoutId, PaintQuad, Pixels, Point, ShapedLine, Style,
    TextRun, Window, fill, font, point, px, relative, size,
};

use crate::display::{display_line, text_runs};
use crate::editor::{Autoscroll, Editor};
use crate::theme::{self, Theme};

/// Layout of the visible part from the previous frame: the mouse and IME use it to convert pixels
/// to text positions and back.
pub struct LayoutCache {
    pub text_bounds: Bounds<Pixels>,
    pub line_height: Pixels,
    /// Screen coordinates of the start of line 0 (accounting for scroll).
    pub origin: Point<Pixels>,
    pub first_line: usize,
    pub lines: Vec<LineLayout>,
}

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

impl LayoutCache {
    pub fn line_top(&self, line: usize) -> Pixels {
        self.origin.y + self.line_height * line as f32
    }

    pub fn line(&self, line: usize) -> Option<&LineLayout> {
        self.lines.get(line.checked_sub(self.first_line)?)
    }

    pub fn visible_lines(&self) -> usize {
        (self.text_bounds.size.height / self.line_height)
            .floor()
            .max(1.) as usize
    }

    /// The text position under a screen point.
    pub fn position_for_point(&self, text: &Rope, p: Point<Pixels>) -> usize {
        let last_line = text.len_lines() - 1;
        let y = (p.y - self.origin.y) / self.line_height;
        let line = (y.max(0.) as usize).min(last_line);
        let column = match self.line(line) {
            Some(layout) => layout.column_for_x(p.x - self.origin.x),
            // The line is off screen (the selection is being dragged past the edge).
            None if line < self.first_line => 0,
            None => line_len(text, line),
        };
        line_start(text, line) + column.min(line_len(text, line))
    }

    /// The rectangle of the character at `pos`, if it is on screen.
    pub fn bounds_for_position(&self, text: &Rope, pos: usize) -> Option<Bounds<Pixels>> {
        let line = text.char_to_line(pos);
        let layout = self.line(line)?;
        let x = self.origin.x + layout.x_for_column(pos - line_start(text, line));
        Some(Bounds::new(
            point(x, self.line_top(line)),
            size(px(theme::FONT_SIZE * 0.6), self.line_height),
        ))
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
}

impl IntoElement for EditorElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

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
        let theme = Theme::get(cx);
        let ui = theme.ui;
        let editor = self.editor.read(cx);
        let text = editor.document.text().clone();
        let selection = editor.document.selection().clone();
        let marked_range = editor.marked_range.clone();
        let mut scroll = editor.scroll;
        let autoscroll = editor.autoscroll;

        let font = font(theme::code_font());
        let font_size = px(theme::FONT_SIZE);
        let line_height = px(theme::LINE_HEIGHT);
        let text_system = window.text_system().clone();
        let em = text_system
            .em_advance(text_system.resolve_font(&font), font_size)
            .unwrap_or(px(8.));

        let total_lines = text.len_lines();
        let digits = total_lines.to_string().len().max(3);
        // A commit message field has no gutter.
        let blame_width = crate::blame::column_width(&editor.blame, em);
        let gutter_width = if editor.message.is_some() {
            px(0.)
        } else {
            em * (digits + 2) as f32 + blame_width
        };
        let text_bounds = Bounds::from_corners(
            point(bounds.left() + gutter_width, bounds.top()),
            bounds.bottom_right(),
        );
        let height = f32::from(bounds.size.height);
        let lh = theme::LINE_HEIGHT;

        let primary = selection.primary();
        let head_line = text.char_to_line(primary.head);

        // Vertical autoscroll: keep the cursor in the viewport with a margin of a few lines, and
        // when jumping to a match, put a line that is out of view in the middle.
        let top = head_line as f32 * lh;
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
            // In the middle, but with a whole top line: the preview doesn't start with a clipped
            // line.
            Some(Autoscroll::Middle) => scroll.y = ((top - (height - lh) / 2.) / lh).round() * lh,
            Some(Autoscroll::Center) | None => {}
        }
        scroll.y = scroll.y.clamp(0., (total_lines - 1) as f32 * lh);

        let first_line = (scroll.y / lh).floor() as usize;
        let last_line = (first_line + (height / lh).ceil() as usize + 1).min(total_lines);

        let run = |len: usize, color| TextRun {
            len,
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };

        // Highlighting of the visible lines: character columns → bytes of the displayed line.
        let highlights = editor
            .highlighter
            .highlight_lines(&text, first_line..last_line);
        let base = run(0, ui.foreground);
        let mut lines = Vec::with_capacity(last_line - first_line);
        let mut max_width = px(0.);
        for (i, line) in (first_line..last_line).enumerate() {
            let (display, char_to_byte) = display_line(&text, line);
            let spans = highlights.get(i).map_or(&[][..], Vec::as_slice);
            let runs = text_runs(spans, &char_to_byte, &base, |h| theme.syntax_style(h));
            let shaped = text_system.shape_line(display.into(), font_size, &runs, None);
            max_width = max_width.max(shaped.width);
            lines.push(LineLayout {
                shaped,
                char_to_byte,
            });
        }

        // Horizontal autoscroll: after the line is laid out, when the cursor's x is known.
        let visible_width = f32::from(text_bounds.size.width) - theme::TEXT_PADDING * 2.;
        if autoscroll.is_some()
            && let Some(layout) = lines.get(head_line.wrapping_sub(first_line))
        {
            let x = f32::from(layout.x_for_column(primary.head - line_start(&text, head_line)));
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

        let layout = LayoutCache {
            text_bounds,
            line_height,
            origin: point(
                text_bounds.left() + px(theme::TEXT_PADDING - scroll.x),
                bounds.top() - px(scroll.y),
            ),
            first_line,
            lines,
        };

        let current_line = (selection.len() == 1 && primary.is_empty() && editor.message.is_none())
            .then(|| {
                fill(
                    Bounds::new(
                        point(bounds.left(), layout.line_top(head_line)),
                        size(bounds.size.width, line_height),
                    ),
                    ui.current_line,
                )
            });

        let mut highlights = Vec::new();
        let newline_width = em * 0.5;
        // Quads for the range `from..to` over the visible lines; a line break inside the range is a
        // "tail" past the end of the line.
        let mut range_quads = |from: usize, to: usize, color: Hsla| {
            let from_line = text.char_to_line(from);
            let to_line = text.char_to_line(to);
            for line in from_line.max(first_line)..=to_line.min(last_line.saturating_sub(1)) {
                let Some(line_layout) = layout.line(line) else {
                    continue;
                };
                let start = line_start(&text, line);
                let x0 = if line == from_line {
                    line_layout.x_for_column(from - start)
                } else {
                    px(0.)
                };
                let x1 = if line == to_line {
                    line_layout.x_for_column(to - start)
                } else {
                    line_layout.shaped.width + newline_width
                };
                let top = layout.line_top(line);
                highlights.push(fill(
                    Bounds::from_corners(
                        point(layout.origin.x + x0, top),
                        point(layout.origin.x + x1, top + line_height),
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
        if let Some(marked) = marked_range
            && let (Some(start), Some(end)) = (
                layout.bounds_for_position(&text, marked.start),
                layout.bounds_for_position(&text, marked.end),
            )
        {
            highlights.push(fill(
                Bounds::from_corners(
                    point(start.left(), start.bottom() - px(2.)),
                    point(end.left(), end.bottom() - px(1.)),
                ),
                ui.foreground,
            ));
        }

        let cursors = selection
            .iter()
            .filter_map(|range| {
                let line = text.char_to_line(range.head);
                let line_layout = layout.line(line)?;
                let x = line_layout.x_for_column(range.head - line_start(&text, line));
                Some(fill(
                    Bounds::new(
                        point(layout.origin.x + x - px(1.), layout.line_top(line)),
                        size(px(2.), line_height),
                    ),
                    ui.cursor,
                ))
            })
            .collect();

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

        let gutter = (first_line..last_line)
            .map(|line| {
                let number = (line + 1).to_string();
                let color = match diagnostics.number_color(line) {
                    Some(color) => color,
                    None if line == head_line => ui.foreground,
                    None => ui.dim,
                };
                let runs = [run(number.len(), color)];
                let shaped = text_system.shape_line(number.into(), font_size, &runs, None);
                let x = bounds.left() + gutter_width - em - shaped.width;
                (shaped, point(x, layout.line_top(line)))
            })
            .collect();

        let decorations = self.editor.update(cx, |editor, _| {
            editor.scroll = scroll;
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
            for (i, line) in layout.lines.iter().enumerate() {
                let origin = point(layout.origin.x, layout.line_top(layout.first_line + i));
                line.shaped.paint(origin, line_height, window, cx).ok();
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
