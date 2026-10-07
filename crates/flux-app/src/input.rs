//! Однострочное поле ввода: запрос палитры команд, поиска файла, строки поиска и замены.
//!
//! Текст, выделение и undo — `flux_core::Document`, движения и правки — функции ядра, как
//! у редактора: графемы, слова и умный Home работают так же. Enter, Esc, ↑/↓ и Tab поле
//! не обрабатывает — они всплывают к родителю (список выбора, строка поиска).

use std::ops::Range as Utf16Range;

use flux_core::movement::{self, Direction};
use flux_core::{Document, EditKind, Range, Rope, Selection, Transaction, edit};
use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, Element, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    GlobalElementId, InspectorElementId, IntoElement, KeyBinding, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, PaintQuad, Pixels, Point, Render, ShapedLine, SharedString,
    Style, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, fill, font, point,
    prelude::*, px, relative, size,
};

use crate::display::display_line;
use crate::element::LineLayout;
use crate::theme::{self, Theme};

/// Кегль и высота строки поля.
pub const INPUT_TEXT_SIZE: f32 = 13.;
const INPUT_LINE_HEIGHT: f32 = 20.;
const CURSOR_WIDTH: f32 = 2.;

actions!(
    input,
    [
        MoveLeft,
        MoveRight,
        SelectLeft,
        SelectRight,
        MoveWordLeft,
        MoveWordRight,
        SelectWordLeft,
        SelectWordRight,
        MoveLineStart,
        MoveLineEnd,
        SelectLineStart,
        SelectLineEnd,
        Backspace,
        Delete,
        DeleteWordBackward,
        DeleteToLineStart,
        SelectAll,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
    ]
);

pub fn init(cx: &mut App) {
    let context = Some("TextInput");
    cx.bind_keys([
        KeyBinding::new("left", MoveLeft, context),
        KeyBinding::new("right", MoveRight, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("alt-left", MoveWordLeft, context),
        KeyBinding::new("alt-right", MoveWordRight, context),
        KeyBinding::new("alt-shift-left", SelectWordLeft, context),
        KeyBinding::new("alt-shift-right", SelectWordRight, context),
        KeyBinding::new("cmd-left", MoveLineStart, context),
        KeyBinding::new("cmd-right", MoveLineEnd, context),
        KeyBinding::new("home", MoveLineStart, context),
        KeyBinding::new("end", MoveLineEnd, context),
        KeyBinding::new("cmd-shift-left", SelectLineStart, context),
        KeyBinding::new("cmd-shift-right", SelectLineEnd, context),
        KeyBinding::new("shift-home", SelectLineStart, context),
        KeyBinding::new("shift-end", SelectLineEnd, context),
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new("shift-backspace", Backspace, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new("alt-backspace", DeleteWordBackward, context),
        KeyBinding::new("cmd-backspace", DeleteToLineStart, context),
        KeyBinding::new("cmd-a", SelectAll, context),
        KeyBinding::new("cmd-c", Copy, context),
        KeyBinding::new("cmd-x", Cut, context),
        KeyBinding::new("cmd-v", Paste, context),
        KeyBinding::new("cmd-z", Undo, context),
        KeyBinding::new("cmd-shift-z", Redo, context),
    ]);
}

/// События поля для родителя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    /// Текст изменился: ввод, вставка, удаление, undo/redo, `set_text`.
    Changed,
}

pub struct TextInput {
    document: Document,
    focus_handle: FocusHandle,
    placeholder: SharedString,
    /// Текст, который сейчас набирается через IME (ещё не подтверждён).
    marked_range: Option<std::ops::Range<usize>>,
    /// Горизонтальный сдвиг текста: курсор всегда в пределах поля.
    scroll_x: Pixels,
    /// Раскладка прошлого кадра: по ней мышь и IME переводят пиксели в позиции.
    layout: Option<InputLayout>,
    selecting: bool,
}

struct InputLayout {
    line: LineLayout,
    /// Экранная точка начала текста (с учётом сдвига).
    origin: Point<Pixels>,
    bounds: Bounds<Pixels>,
}

impl EventEmitter<InputEvent> for TextInput {}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            document: Document::from_text(""),
            focus_handle: cx.focus_handle(),
            placeholder: placeholder.into(),
            marked_range: None,
            scroll_x: px(0.),
            layout: None,
            selecting: false,
        }
    }

    pub fn text(&self) -> String {
        self.document.text().to_string()
    }

    pub fn is_empty(&self) -> bool {
        self.document.text().len_chars() == 0
    }

    /// Заменяет текст целиком (одной правкой — её можно отменить), курсор — в конец.
    /// `Changed` — только если текст стал другим.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = single_line(text);
        if *self.document.text() == text.as_str() {
            let end = self.document.text().len_chars();
            return self.set_selection(Selection::point(end), cx);
        }
        let rope = self.document.text();
        let end = text.chars().count();
        let tx = Transaction::change(rope, [(0, rope.len_chars(), Some(text))])
            .with_selection(Selection::point(end));
        self.apply(tx, EditKind::Other, cx);
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        let end = self.document.text().len_chars();
        self.set_selection(Selection::single(0, end), cx);
    }

    fn set_selection(&mut self, selection: Selection, cx: &mut Context<Self>) {
        self.document.set_selection(selection);
        self.marked_range = None;
        cx.notify();
    }

    fn motion(&mut self, cx: &mut Context<Self>, f: impl Fn(&Rope, Range) -> Range) {
        let text = self.document.text().clone();
        let selection = self
            .document
            .selection()
            .transform(|range| f(&text, *range));
        self.set_selection(selection, cx);
    }

    fn apply(&mut self, tx: Transaction, kind: EditKind, cx: &mut Context<Self>) {
        let changed = self.document.apply(tx, kind).is_some();
        self.marked_range = None;
        if changed {
            cx.emit(InputEvent::Changed);
        }
        cx.notify();
    }

    fn edit(
        &mut self,
        kind: EditKind,
        cx: &mut Context<Self>,
        f: impl FnOnce(&Rope, &Selection) -> Transaction,
    ) {
        let tx = f(self.document.text(), self.document.selection());
        self.apply(tx, kind, cx);
    }

    fn history_step(
        &mut self,
        cx: &mut Context<Self>,
        step: impl FnOnce(&mut Document) -> Option<Vec<flux_core::TextChange>>,
    ) {
        if let Some(changes) = step(&mut self.document) {
            self.marked_range = None;
            if !changes.is_empty() {
                cx.emit(InputEvent::Changed);
            }
            cx.notify();
        }
    }

    fn selected_text(&self) -> Option<String> {
        let range = self.document.selection().primary();
        (!range.is_empty()).then(|| {
            self.document
                .text()
                .slice(range.from()..range.to())
                .to_string()
        })
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.edit(EditKind::Other, cx, |rope, sel| {
                edit::insert_text(rope, sel, "")
            });
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let text = single_line(&text);
        self.edit(EditKind::Other, cx, |rope, sel| {
            edit::insert_text(rope, sel, &text)
        });
    }

    /// Cmd+Backspace: всё от начала строки до курсора (или выделение).
    fn delete_to_line_start(
        &mut self,
        _: &DeleteToLineStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.document.selection().primary();
        let from = if range.is_empty() { 0 } else { range.from() };
        let tx = Transaction::change(self.document.text(), [(from, range.to(), None)])
            .with_selection(Selection::point(from));
        self.apply(tx, EditKind::Other, cx);
    }

    // --- Мышь ---

    fn position_for_mouse(&self, position: Point<Pixels>) -> usize {
        let len = self.document.text().len_chars();
        match &self.layout {
            Some(layout) => layout
                .line
                .column_for_x(position.x - layout.origin.x)
                .min(len),
            None => len,
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        let pos = self.position_for_mouse(event.position);
        let text = self.document.text();
        let selection = match event.click_count {
            2 => Selection::from_range(movement::word_range_at(text, pos)),
            3.. => Selection::single(0, text.len_chars()),
            _ if event.modifiers.shift => {
                Selection::from_range(self.document.selection().primary().put_cursor(pos, true))
            }
            _ => Selection::point(pos),
        };
        self.selecting = true;
        self.set_selection(selection, cx);
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            self.selecting = false;
            return;
        }
        let pos = self.position_for_mouse(event.position);
        let primary = self.document.selection().primary().put_cursor(pos, true);
        self.set_selection(Selection::from_range(primary), cx);
    }

    // --- UTF-16 ↔ символы: IME и macOS считают позиции в UTF-16 ---

    fn utf16_range(&self, range: &std::ops::Range<usize>) -> Utf16Range<usize> {
        let text = self.document.text();
        text.char_to_utf16_cu(range.start)..text.char_to_utf16_cu(range.end)
    }

    fn char_range(&self, range: &Utf16Range<usize>) -> std::ops::Range<usize> {
        let text = self.document.text();
        let len = text.len_utf16_cu();
        text.utf16_cu_to_char(range.start.min(len))..text.utf16_cu_to_char(range.end.min(len))
    }

    /// Диапазон, который заменяет IME: явно заданный или текущая композиция.
    fn input_range(
        &self,
        range_utf16: Option<Utf16Range<usize>>,
    ) -> Option<std::ops::Range<usize>> {
        range_utf16
            .map(|r| self.char_range(&r))
            .or_else(|| self.marked_range.clone())
    }
}

/// Текст для однострочного поля: переводы строк и табы — пробелы.
pub fn single_line(text: &str) -> String {
    text.replace("\r\n", " ").replace(['\n', '\r', '\t'], " ")
}

/// Нажатие Enter или Tab, которое не перехватил никто выше, приходит в поле как текст —
/// его не вставляем.
fn is_only_breaks(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| matches!(c, '\n' | '\r' | '\t'))
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Utf16Range<usize>,
        actual_range: &mut Option<Utf16Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.char_range(&range_utf16);
        actual_range.replace(self.utf16_range(&range));
        Some(self.document.text().slice(range).to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let primary = self.document.selection().primary();
        Some(UTF16Selection {
            range: self.utf16_range(&(primary.from()..primary.to())),
            reversed: primary.head < primary.anchor,
        })
    }

    fn marked_text_range(
        &self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Utf16Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.utf16_range(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    /// Обычный ввод символов приходит сюда, а не через действия.
    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Utf16Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if is_only_breaks(new_text) && self.marked_range.is_none() {
            return;
        }
        let new_text = single_line(new_text);
        let tx = match self.input_range(range_utf16) {
            Some(range) => {
                let end = range.start + new_text.chars().count();
                Transaction::change(
                    self.document.text(),
                    [(range.start, range.end, Some(new_text))],
                )
                .with_selection(Selection::point(end))
            }
            None => edit::insert_text(self.document.text(), self.document.selection(), &new_text),
        };
        self.apply(tx, EditKind::Insert, cx);
    }

    /// Промежуточный текст IME (иероглифы, «ё» через долгое нажатие).
    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Utf16Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Utf16Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let new_text = single_line(new_text);
        let primary = self.document.selection().primary();
        let range = self
            .input_range(range_utf16)
            .unwrap_or(primary.from()..primary.to());
        let len = new_text.chars().count();
        // Выделение внутри композиции IME передаёт в UTF-16 относительно её начала.
        let selected = new_selected_range_utf16
            .map(|r| {
                let starts: Vec<usize> = new_text
                    .chars()
                    .scan(0, |utf16, c| {
                        let at = *utf16;
                        *utf16 += c.len_utf16();
                        Some(at)
                    })
                    .collect();
                let to_char = |u: usize| starts.partition_point(|&at| at < u);
                range.start + to_char(r.start)..range.start + to_char(r.end)
            })
            .unwrap_or(range.start + len..range.start + len);
        let tx = Transaction::change(
            self.document.text(),
            [(range.start, range.end, Some(new_text))],
        )
        .with_selection(Selection::single(selected.start, selected.end));
        self.apply(tx, EditKind::Insert, cx);
        self.marked_range = (len > 0).then(|| range.start..range.start + len);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Utf16Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let range = self.char_range(&range_utf16);
        let start = layout.origin.x + layout.line.x_for_column(range.start);
        let end = layout.origin.x + layout.line.x_for_column(range.end);
        Some(Bounds::from_corners(
            point(start, layout.bounds.top()),
            point(end.max(start + px(1.)), layout.bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        self.layout.as_ref()?;
        let pos = self.position_for_mouse(point);
        Some(self.document.text().char_to_utf16_cu(pos))
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use Direction::{Backward, Forward};
        let ui = Theme::ui(cx);
        let focused = self.focus_handle.is_focused(window);
        div()
            .key_context("TextInput")
            .track_focus(&self.focus_handle)
            .w_full()
            .px_2()
            .py_1()
            .bg(ui.input_background)
            .border_1()
            .border_color(if focused {
                ui.focus_border
            } else {
                ui.input_border
            })
            .rounded_md()
            .text_color(ui.foreground)
            .font_family(theme::FONT_FAMILY)
            .text_size(px(INPUT_TEXT_SIZE))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(|this, _: &MoveLeft, _, cx| {
                this.motion(cx, |t, r| {
                    movement::move_horizontally(t, r, Backward, false)
                })
            }))
            .on_action(cx.listener(|this, _: &MoveRight, _, cx| {
                this.motion(cx, |t, r| movement::move_horizontally(t, r, Forward, false))
            }))
            .on_action(cx.listener(|this, _: &SelectLeft, _, cx| {
                this.motion(cx, |t, r| movement::move_horizontally(t, r, Backward, true))
            }))
            .on_action(cx.listener(|this, _: &SelectRight, _, cx| {
                this.motion(cx, |t, r| movement::move_horizontally(t, r, Forward, true))
            }))
            .on_action(cx.listener(|this, _: &MoveWordLeft, _, cx| {
                this.motion(cx, |t, r| movement::move_word(t, r, Backward, false))
            }))
            .on_action(cx.listener(|this, _: &MoveWordRight, _, cx| {
                this.motion(cx, |t, r| movement::move_word(t, r, Forward, false))
            }))
            .on_action(cx.listener(|this, _: &SelectWordLeft, _, cx| {
                this.motion(cx, |t, r| movement::move_word(t, r, Backward, true))
            }))
            .on_action(cx.listener(|this, _: &SelectWordRight, _, cx| {
                this.motion(cx, |t, r| movement::move_word(t, r, Forward, true))
            }))
            .on_action(cx.listener(|this, _: &MoveLineStart, _, cx| {
                this.motion(cx, |t, r| movement::move_line_start(t, r, false))
            }))
            .on_action(cx.listener(|this, _: &MoveLineEnd, _, cx| {
                this.motion(cx, |t, r| movement::move_line_end(t, r, false))
            }))
            .on_action(cx.listener(|this, _: &SelectLineStart, _, cx| {
                this.motion(cx, |t, r| movement::move_line_start(t, r, true))
            }))
            .on_action(cx.listener(|this, _: &SelectLineEnd, _, cx| {
                this.motion(cx, |t, r| movement::move_line_end(t, r, true))
            }))
            .on_action(cx.listener(|this, _: &Backspace, _, cx| {
                this.edit(EditKind::Delete, cx, edit::delete_backward)
            }))
            .on_action(cx.listener(|this, _: &Delete, _, cx| {
                this.edit(EditKind::Delete, cx, edit::delete_forward)
            }))
            .on_action(cx.listener(|this, _: &DeleteWordBackward, _, cx| {
                this.edit(EditKind::Other, cx, edit::delete_word_backward)
            }))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| this.select_all(cx)))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(|this, _: &Undo, _, cx| this.history_step(cx, Document::undo)))
            .on_action(cx.listener(|this, _: &Redo, _, cx| this.history_step(cx, Document::redo)))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.selecting = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.selecting = false),
            )
            .child(InputElement { input: cx.entity() })
    }
}

/// Строка поля: текст (или подсказка), выделение, курсор.
struct InputElement {
    input: Entity<TextInput>,
}

struct InputPrepaint {
    line: LineLayout,
    /// Подсказка вместо пустого текста.
    placeholder: Option<ShapedLine>,
    origin: Point<Pixels>,
    selection: Option<PaintQuad>,
    cursor: Option<PaintQuad>,
}

impl IntoElement for InputElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for InputElement {
    type RequestLayoutState = ();
    type PrepaintState = InputPrepaint;

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
        style.size.height = px(INPUT_LINE_HEIGHT).into();
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
    ) -> InputPrepaint {
        let ui = Theme::ui(cx);
        let input = self.input.read(cx);
        let text = input.document.text().clone();
        let primary = input.document.selection().primary();
        let marked = input.marked_range.clone();
        let focused = input.focus_handle.is_focused(window);
        let placeholder_text = input.placeholder.clone();
        let mut scroll_x = input.scroll_x;

        let font_size = px(INPUT_TEXT_SIZE);
        let run = |len: usize, color| TextRun {
            len,
            font: font(theme::FONT_FAMILY),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let (display, char_to_byte) = display_line(&text, 0);
        // Композиция IME подчёркивается.
        let runs = match &marked {
            Some(marked) if !display.is_empty() => {
                let start = char_to_byte[marked.start.min(char_to_byte.len() - 1)];
                let end = char_to_byte[marked.end.min(char_to_byte.len() - 1)];
                let mut underlined = run(end - start, ui.foreground);
                underlined.underline = Some(UnderlineStyle {
                    thickness: px(1.),
                    color: Some(ui.foreground),
                    wavy: false,
                });
                [
                    run(start, ui.foreground),
                    underlined,
                    run(display.len() - end, ui.foreground),
                ]
                .into_iter()
                .filter(|run| run.len > 0)
                .collect()
            }
            _ => vec![run(display.len(), ui.foreground)],
        };
        let text_system = window.text_system().clone();
        let shaped = text_system.shape_line(display.into(), font_size, &runs, None);
        let line = LineLayout {
            shaped,
            char_to_byte,
        };
        let placeholder = (text.len_chars() == 0 && !placeholder_text.is_empty()).then(|| {
            let runs = [run(placeholder_text.len(), ui.dim)];
            text_system.shape_line(placeholder_text, font_size, &runs, None)
        });

        // Сдвиг: курсор в пределах поля, без пустоты справа, когда текст укоротился.
        let width = bounds.size.width;
        let cursor_width = px(CURSOR_WIDTH);
        let cursor_x = line.x_for_column(primary.head);
        if cursor_x - scroll_x > width - cursor_width {
            scroll_x = cursor_x - width + cursor_width;
        }
        if cursor_x < scroll_x {
            scroll_x = cursor_x;
        }
        let max_scroll = (line.shaped.width + cursor_width - width).max(px(0.));
        scroll_x = scroll_x.clamp(px(0.), max_scroll);
        let origin = point(bounds.left() - scroll_x, bounds.top());

        let selection = (!primary.is_empty()).then(|| {
            fill(
                Bounds::from_corners(
                    point(origin.x + line.x_for_column(primary.from()), bounds.top()),
                    point(origin.x + line.x_for_column(primary.to()), bounds.bottom()),
                ),
                ui.selection,
            )
        });
        let cursor = focused.then(|| {
            fill(
                Bounds::new(
                    point(origin.x + cursor_x, bounds.top()),
                    size(cursor_width, bounds.size.height),
                ),
                ui.cursor,
            )
        });

        self.input.update(cx, |input, _| input.scroll_x = scroll_x);
        InputPrepaint {
            line,
            placeholder,
            origin,
            selection,
            cursor,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        state: &mut InputPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let line_height = bounds.size.height;
        let origin = state.origin;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let Some(selection) = state.selection.take() {
                window.paint_quad(selection);
            }
            match &state.placeholder {
                Some(placeholder) => placeholder.paint(bounds.origin, line_height, window, cx),
                None => state.line.shaped.paint(origin, line_height, window, cx),
            }
            .ok();
            if let Some(cursor) = state.cursor.take() {
                window.paint_quad(cursor);
            }
        });

        let line = std::mem::replace(
            &mut state.line,
            LineLayout {
                shaped: ShapedLine::default(),
                char_to_byte: vec![0],
            },
        );
        self.input.update(cx, |input, _| {
            input.layout = Some(InputLayout {
                line,
                origin,
                bounds,
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_turns_breaks_and_tabs_into_spaces() {
        assert_eq!(single_line("a\r\nb\nc\rd\te"), "a b c d e");
        assert_eq!(single_line("привет"), "привет");
        assert_eq!(single_line(""), "");
    }

    #[test]
    fn lone_enter_or_tab_is_not_text() {
        assert!(is_only_breaks("\n"));
        assert!(is_only_breaks("\r\n"));
        assert!(is_only_breaks("\t"));
        assert!(!is_only_breaks(""));
        assert!(!is_only_breaks("a\n"));
        assert!(!is_only_breaks(" "));
    }
}
