//! Отрисовка редактора. Каждый кадр рисует только видимые строки:
//! стоимость кадра не зависит от размера файла.

use flux_core::Rope;
use flux_core::text::{line_len, line_start};
use gpui::{
    App, Bounds, ContentMask, Element, ElementId, ElementInputHandler, Entity, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, PaintQuad, Pixels, Point, ShapedLine, Style,
    TextRun, Window, fill, font, point, px, relative, size,
};

use crate::display::{display_line, text_runs};
use crate::editor::Editor;
use crate::theme::{self, Theme};

/// Раскладка видимой части с прошлого кадра: по ней мышь и IME
/// переводят пиксели в позиции текста и обратно.
pub struct LayoutCache {
    pub text_bounds: Bounds<Pixels>,
    pub line_height: Pixels,
    /// Экранные координаты начала строки 0 (с учётом скролла).
    pub origin: Point<Pixels>,
    pub first_line: usize,
    pub lines: Vec<LineLayout>,
}

pub struct LineLayout {
    pub shaped: ShapedLine,
    /// Байтовое смещение в отображаемой строке для каждой колонки (+ конец).
    /// Отображаемая строка отличается от исходной: табы развёрнуты в пробелы.
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
        // Внутри развёрнутого таба — к ближайшему краю.
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
        (self.text_bounds.size.height / self.line_height).floor().max(1.) as usize
    }

    /// Позиция в тексте под точкой экрана.
    pub fn position_for_point(&self, text: &Rope, p: Point<Pixels>) -> usize {
        let last_line = text.len_lines() - 1;
        let y = (p.y - self.origin.y) / self.line_height;
        let line = (y.max(0.) as usize).min(last_line);
        let column = match self.line(line) {
            Some(layout) => layout.column_for_x(p.x - self.origin.x),
            // Строка вне экрана (тянем выделение за край).
            None if line < self.first_line => 0,
            None => line_len(text, line),
        };
        line_start(text, line) + column.min(line_len(text, line))
    }

    /// Прямоугольник символа в позиции `pos`, если он на экране.
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
    /// Выделения и подчёркивание IME — рисуются под текстом, в пределах области текста.
    highlights: Vec<PaintQuad>,
    cursors: Vec<PaintQuad>,
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

        let font = font(theme::FONT_FAMILY);
        let font_size = px(theme::FONT_SIZE);
        let line_height = px(theme::LINE_HEIGHT);
        let text_system = window.text_system().clone();
        let em = text_system
            .em_advance(text_system.resolve_font(&font), font_size)
            .unwrap_or(px(8.));

        let total_lines = text.len_lines();
        let digits = total_lines.to_string().len().max(3);
        let gutter_width = em * (digits + 2) as f32;
        let text_bounds = Bounds::from_corners(
            point(bounds.left() + gutter_width, bounds.top()),
            bounds.bottom_right(),
        );
        let height = f32::from(bounds.size.height);
        let lh = theme::LINE_HEIGHT;

        let primary = selection.primary();
        let head_line = text.char_to_line(primary.head);

        // Вертикальный автоскролл: держим курсор в окне с запасом в несколько строк.
        if autoscroll {
            let margin = (theme::SCROLL_MARGIN_LINES as f32 * lh).min((height - lh) / 2.).max(0.);
            let top = head_line as f32 * lh;
            if top - margin < scroll.y {
                scroll.y = top - margin;
            } else if top + lh + margin > scroll.y + height {
                scroll.y = top + lh + margin - height;
            }
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

        // Подсветка видимых строк: символьные колонки → байты отображаемой строки.
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

        // Горизонтальный автоскролл — после раскладки строки, когда известен x курсора.
        let visible_width = f32::from(text_bounds.size.width) - theme::TEXT_PADDING * 2.;
        if autoscroll && let Some(layout) = lines.get(head_line.wrapping_sub(first_line)) {
            let x = f32::from(layout.x_for_column(primary.head - line_start(&text, head_line)));
            let margin = f32::from(em) * 4.;
            if x < scroll.x {
                scroll.x = x - margin;
            } else if x > scroll.x + visible_width - f32::from(em) {
                scroll.x = x - visible_width + margin;
            }
        }
        scroll.x = scroll.x.clamp(0., (f32::from(max_width) - visible_width / 2.).max(0.));

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

        let current_line = (selection.len() == 1 && primary.is_empty()).then(|| {
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
        for range in selection.iter().filter(|r| !r.is_empty()) {
            let (from, to) = (range.from(), range.to());
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
                    ui.selection,
                ));
            }
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

        let gutter = (first_line..last_line)
            .map(|line| {
                let number = (line + 1).to_string();
                let color = if line == head_line {
                    ui.foreground
                } else {
                    ui.dim
                };
                let runs = [run(number.len(), color)];
                let shaped = text_system.shape_line(number.into(), font_size, &runs, None);
                let x = bounds.left() + gutter_width - em - shaped.width;
                (shaped, point(x, layout.line_top(line)))
            })
            .collect();

        self.editor.update(cx, |editor, _| {
            editor.scroll = scroll;
            editor.autoscroll = false;
        });

        PrepaintState {
            layout: Some(layout),
            gutter,
            current_line,
            highlights,
            cursors,
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
        // Курсоры — только в фокусе и в «видимой» фазе мигания.
        let show_cursors = editor.cursor_visible && focus_handle.is_focused(window);
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );

        let layout = state.layout.take().expect("prepaint always produces a layout");
        let line_height = layout.line_height;

        if let Some(quad) = state.current_line.take() {
            window.paint_quad(quad);
        }
        for (number, origin) in &state.gutter {
            number.paint(*origin, line_height, window, cx).ok();
        }

        let mask = ContentMask {
            bounds: layout.text_bounds,
        };
        window.with_content_mask(Some(mask), |window| {
            for quad in state.highlights.drain(..) {
                window.paint_quad(quad);
            }
            for (i, line) in layout.lines.iter().enumerate() {
                let origin = point(layout.origin.x, layout.line_top(layout.first_line + i));
                line.shaped.paint(origin, line_height, window, cx).ok();
            }
            if show_cursors {
                for cursor in state.cursors.drain(..) {
                    window.paint_quad(cursor);
                }
            }
        });

        self.editor.update(cx, |editor, _| editor.layout = Some(layout));
    }
}
