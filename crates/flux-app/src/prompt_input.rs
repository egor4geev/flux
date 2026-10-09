//! A multi-line text field that wraps long lines and grows with its text up to a number of rows,
//! then scrolls: the Claude message. Text, selection and undo come from `flux_core::Document`;
//! movements and edits are the core's, as in the editor and the single-line field
//! ([`crate::input::TextInput`]); IME through `EntityInputHandler`.
//!
//! The field doesn't handle ↵: it bubbles up to the parent (the composer sends). ⇧↵ and ⌥↵ start a
//! new line. ↑ and ↓ move between the rows as they are drawn (a wrapped line has several). While
//! the parent shows a list for the text at the caret ([`PromptInput::set_menu_open`]: mentions,
//! commands), ↑ ↓ ↵ ⇥ Esc become the `Menu*` actions, which bubble up to the parent.
//!
//! The geometry of the drawn rows ([`Geometry`]) is plain data built from gpui's shaped lines each
//! frame: the caret, the mouse, the selection and ↑/↓ use it, and the tests check it without gpui.

use std::ops::Range as Utf16Range;

use flux_core::movement::{self, Direction};
use flux_core::{Document, EditKind, Range, Rope, Selection, Transaction, edit};
use gpui::{
    App, AvailableSpace, Bounds, ClipboardEntry, ClipboardItem, ContentMask, Context, CursorStyle,
    Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, GlobalElementId, Hsla, Image, InspectorElementId, IntoElement, KeyBinding,
    KeyContext, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, PaintQuad, Pixels, Point,
    Render, ScrollWheelEvent, SharedString, Size, Style, TextAlign, TextRun, UTF16Selection,
    UnderlineStyle, Window, WrappedLine, actions, div, fill, font, point, prelude::*, px,
    relative, size,
};

use crate::theme::{self, Theme};

/// The message's font size and line height (the interface font).
pub const TEXT_SIZE: f32 = 13.;
pub const LINE_HEIGHT: f32 = 20.;
/// The field grows up to this many rows, then scrolls.
const MAX_ROWS: usize = 10;
const CURSOR_WIDTH: f32 = 2.;
/// A selected line break shows as this much selection past the end of its line.
const NEWLINE_WIDTH: f32 = 5.;

actions!(
    prompt_input,
    [
        MoveLeft,
        MoveRight,
        SelectLeft,
        SelectRight,
        MoveWordLeft,
        MoveWordRight,
        SelectWordLeft,
        SelectWordRight,
        MoveUp,
        MoveDown,
        SelectUp,
        SelectDown,
        MoveLineStart,
        MoveLineEnd,
        SelectLineStart,
        SelectLineEnd,
        MoveToStart,
        MoveToEnd,
        SelectToStart,
        SelectToEnd,
        Backspace,
        Delete,
        DeleteWordBackward,
        DeleteToLineStart,
        /// ⇧↵, ⌥↵: a line break (↵ alone goes to the parent).
        InsertNewline,
        SelectAll,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
        /// The parent's list for the text at the caret: ↓, ↑, ↵ or ⇥, Esc.
        MenuNext,
        MenuPrevious,
        MenuAccept,
        MenuDismiss,
    ]
);

const CONTEXT: &str = "PromptInput";

pub fn init(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("left", MoveLeft, context),
        KeyBinding::new("right", MoveRight, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("alt-left", MoveWordLeft, context),
        KeyBinding::new("alt-right", MoveWordRight, context),
        KeyBinding::new("alt-shift-left", SelectWordLeft, context),
        KeyBinding::new("alt-shift-right", SelectWordRight, context),
        KeyBinding::new("up", MoveUp, context),
        KeyBinding::new("down", MoveDown, context),
        KeyBinding::new("shift-up", SelectUp, context),
        KeyBinding::new("shift-down", SelectDown, context),
        KeyBinding::new("cmd-left", MoveLineStart, context),
        KeyBinding::new("cmd-right", MoveLineEnd, context),
        KeyBinding::new("home", MoveLineStart, context),
        KeyBinding::new("end", MoveLineEnd, context),
        KeyBinding::new("cmd-shift-left", SelectLineStart, context),
        KeyBinding::new("cmd-shift-right", SelectLineEnd, context),
        KeyBinding::new("shift-home", SelectLineStart, context),
        KeyBinding::new("shift-end", SelectLineEnd, context),
        KeyBinding::new("cmd-up", MoveToStart, context),
        KeyBinding::new("cmd-down", MoveToEnd, context),
        KeyBinding::new("cmd-shift-up", SelectToStart, context),
        KeyBinding::new("cmd-shift-down", SelectToEnd, context),
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new("shift-backspace", Backspace, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new("alt-backspace", DeleteWordBackward, context),
        KeyBinding::new("cmd-backspace", DeleteToLineStart, context),
        KeyBinding::new("shift-enter", InsertNewline, context),
        KeyBinding::new("alt-enter", InsertNewline, context),
        KeyBinding::new("cmd-a", SelectAll, context),
        KeyBinding::new("cmd-c", Copy, context),
        KeyBinding::new("cmd-x", Cut, context),
        KeyBinding::new("cmd-v", Paste, context),
        KeyBinding::new("cmd-z", Undo, context),
        KeyBinding::new("cmd-shift-z", Redo, context),
    ]);
    // After the field's own keys: with a list open, these win over ↑ ↓ above.
    let menu = Some("PromptInput && showing_menu");
    cx.bind_keys([
        KeyBinding::new("down", MenuNext, menu),
        KeyBinding::new("up", MenuPrevious, menu),
        KeyBinding::new("enter", MenuAccept, menu),
        KeyBinding::new("tab", MenuAccept, menu),
        KeyBinding::new("escape", MenuDismiss, menu),
    ]);
}

/// Field events, for the parent.
#[derive(Debug, Clone, PartialEq)]
pub enum PromptInputEvent {
    /// The text changed: typing, paste, deletion, undo/redo, `set_text`.
    Changed,
    /// The caret or the selection moved (a list for the text at the caret follows it).
    SelectionChanged,
    /// ⌘V with a picture on the clipboard: the parent attaches it.
    ImagePasted(Image),
}

pub struct PromptInput {
    document: Document,
    focus_handle: FocusHandle,
    placeholder: SharedString,
    /// Text currently being typed through the IME (not yet committed).
    marked_range: Option<std::ops::Range<usize>>,
    max_rows: usize,
    /// The vertical scroll of a field taller than `max_rows`.
    scroll_y: Pixels,
    /// The caret moved or the text changed: the next frame scrolls the caret into view.
    autoscroll: bool,
    /// The x the caret keeps while ↑/↓ cross shorter rows.
    goal_x: Option<Pixels>,
    /// The previous frame's layout: the mouse and the IME convert pixels to positions with it.
    layout: Option<PromptLayout>,
    selecting: bool,
    /// The parent shows a list for the text at the caret: ↑ ↓ ↵ ⇥ Esc drive it.
    menu_open: bool,
}

struct PromptLayout {
    geometry: Geometry,
    /// Window point of the content's top-left corner (the scroll applied).
    origin: Point<Pixels>,
    bounds: Bounds<Pixels>,
}

impl EventEmitter<PromptInputEvent> for PromptInput {}

impl PromptInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            document: Document::from_text(""),
            focus_handle: cx.focus_handle(),
            placeholder: placeholder.into(),
            marked_range: None,
            max_rows: MAX_ROWS,
            scroll_y: px(0.),
            autoscroll: false,
            goal_x: None,
            layout: None,
            selecting: false,
            menu_open: false,
        }
    }

    /// The field grows up to `rows` rows, then scrolls.
    pub fn max_rows(mut self, rows: usize) -> Self {
        self.max_rows = rows.max(1);
        self
    }

    /// The hint shown while the field is empty (no redraw of its own: the parent sets it while it
    /// renders).
    pub fn set_placeholder(&mut self, placeholder: impl Into<SharedString>) {
        self.placeholder = placeholder.into();
    }

    pub fn text(&self) -> String {
        self.document.text().to_string()
    }

    pub fn is_empty(&self) -> bool {
        self.document.text().len_chars() == 0
    }

    /// The caret: the character index of the primary selection's head.
    pub fn cursor(&self) -> usize {
        self.document.selection().primary().head
    }

    /// No selection, only a caret.
    pub fn has_selection(&self) -> bool {
        !self.document.selection().primary().is_empty()
    }

    /// Replaces the entire text (as one edit that can be undone), caret at the end.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = normalize(text);
        let len = self.document.text().len_chars();
        self.replace(0..len, &text, cx);
    }

    /// Replaces the characters of `range` with `text`, caret after it: a mention or a command
    /// chosen from the parent's list.
    pub fn replace(&mut self, range: std::ops::Range<usize>, text: &str, cx: &mut Context<Self>) {
        let len = self.document.text().len_chars();
        let (start, end) = (range.start.min(len), range.end.min(len));
        let text = normalize(text);
        let caret = start + text.chars().count();
        if self.document.text().slice(start..end) == text.as_str() {
            return self.set_selection(Selection::point(caret), cx);
        }
        let tx = Transaction::change(self.document.text(), [(start, end, Some(text))])
            .with_selection(Selection::point(caret));
        self.apply(tx, EditKind::Other, cx);
    }

    /// Types `text` at the caret (over the selection).
    pub fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = normalize(text);
        self.edit(EditKind::Other, cx, |rope, sel| edit::insert_text(rope, sel, &text));
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        let end = self.document.text().len_chars();
        self.set_selection(Selection::single(0, end), cx);
    }

    /// The parent opened or closed its list for the text at the caret.
    pub fn set_menu_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.menu_open != open {
            self.menu_open = open;
            cx.notify();
        }
    }

    fn line_height(&self) -> Pixels {
        px(LINE_HEIGHT)
    }

    fn set_selection(&mut self, selection: Selection, cx: &mut Context<Self>) {
        let moved = *self.document.selection() != selection;
        self.document.set_selection(selection);
        self.marked_range = None;
        self.autoscroll = true;
        if moved {
            cx.emit(PromptInputEvent::SelectionChanged);
        }
        cx.notify();
    }

    fn motion(&mut self, cx: &mut Context<Self>, f: impl Fn(&Rope, Range) -> Range) {
        self.goal_x = None;
        let text = self.document.text().clone();
        let selection = self
            .document
            .selection()
            .transform(|range| f(&text, *range));
        self.set_selection(selection, cx);
    }

    /// ↑/↓: the row above or below as drawn, keeping the x the caret had on the first press.
    fn vertical(&mut self, rows: isize, extend: bool, cx: &mut Context<Self>) {
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        let primary = self.document.selection().primary();
        // Without extending, a selection collapses to its edge first.
        let from = if extend || primary.is_empty() {
            primary.head
        } else if rows < 0 {
            primary.from()
        } else {
            primary.to()
        };
        let goal = self
            .goal_x
            .unwrap_or_else(|| layout.geometry.caret(from).x);
        let target = layout.geometry.vertical(from, goal, rows);
        let range = if extend {
            primary.put_cursor(target, true)
        } else {
            Range::point(target)
        };
        self.set_selection(Selection::from_range(range), cx);
        self.goal_x = Some(goal);
    }

    fn apply(&mut self, tx: Transaction, kind: EditKind, cx: &mut Context<Self>) {
        let changed = self.document.apply(tx, kind).is_some();
        self.marked_range = None;
        self.goal_x = None;
        self.autoscroll = true;
        if changed {
            cx.emit(PromptInputEvent::Changed);
        }
        cx.emit(PromptInputEvent::SelectionChanged);
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
            self.goal_x = None;
            self.autoscroll = true;
            if !changes.is_empty() {
                cx.emit(PromptInputEvent::Changed);
            }
            cx.emit(PromptInputEvent::SelectionChanged);
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

    /// Text goes in at the caret; a picture (a screenshot copied to the clipboard) goes to the
    /// parent.
    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text() {
            let text = normalize(&text);
            return self.edit(EditKind::Other, cx, |rope, sel| {
                edit::insert_text(rope, sel, &text)
            });
        }
        for entry in item.into_entries() {
            if let ClipboardEntry::Image(image) = entry {
                cx.emit(PromptInputEvent::ImagePasted(image));
            }
        }
    }

    /// ⌘⌫: everything from the start of the line to the caret (or the selection).
    fn delete_to_line_start(
        &mut self,
        _: &DeleteToLineStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.document.selection().primary();
        let text = self.document.text();
        let from = if range.is_empty() {
            text.line_to_char(text.char_to_line(range.head))
        } else {
            range.from()
        };
        let from = if from == range.to() && from > 0 {
            // At the start of a line: the line break before it.
            from - 1
        } else {
            from
        };
        let tx = Transaction::change(text, [(from, range.to(), None)])
            .with_selection(Selection::point(from));
        self.apply(tx, EditKind::Delete, cx);
    }

    // --- Mouse ---

    fn position_for_mouse(&self, position: Point<Pixels>) -> usize {
        match &self.layout {
            Some(layout) => layout.geometry.index_at(position - layout.origin),
            None => self.document.text().len_chars(),
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.goal_x = None;
        let pos = self.position_for_mouse(event.position);
        let text = self.document.text();
        let selection = match event.click_count {
            2 => Selection::from_range(movement::word_range_at(text, pos)),
            3.. => {
                // The paragraph: the whole line, its break included.
                let line = text.char_to_line(pos);
                let start = text.line_to_char(line);
                let end = if line + 1 < text.len_lines() {
                    text.line_to_char(line + 1)
                } else {
                    text.len_chars()
                };
                Selection::single(start, end)
            }
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

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(layout) = &self.layout else {
            return;
        };
        let overflow = layout.geometry.height() - layout.bounds.size.height;
        if overflow <= px(0.) {
            return;
        }
        let delta = event.delta.pixel_delta(self.line_height());
        self.scroll_y = (self.scroll_y - delta.y).clamp(px(0.), overflow);
        cx.stop_propagation();
        cx.notify();
    }

    // --- UTF-16 ↔ characters: the IME and macOS count positions in UTF-16 ---

    fn utf16_range(&self, range: &std::ops::Range<usize>) -> Utf16Range<usize> {
        let text = self.document.text();
        text.char_to_utf16_cu(range.start)..text.char_to_utf16_cu(range.end)
    }

    fn char_range(&self, range: &Utf16Range<usize>) -> std::ops::Range<usize> {
        let text = self.document.text();
        let len = text.len_utf16_cu();
        text.utf16_cu_to_char(range.start.min(len))..text.utf16_cu_to_char(range.end.min(len))
    }

    /// The range the IME replaces: an explicitly given one or the current composition.
    fn input_range(
        &self,
        range_utf16: Option<Utf16Range<usize>>,
    ) -> Option<std::ops::Range<usize>> {
        range_utf16
            .map(|r| self.char_range(&r))
            .or_else(|| self.marked_range.clone())
    }

    fn key_context(&self) -> KeyContext {
        let mut context = KeyContext::new_with_defaults();
        context.add(CONTEXT);
        if self.menu_open {
            context.add("showing_menu");
        }
        context
    }
}

/// Text for the field: line breaks become `\n`.
pub fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// A keystroke nobody bound (↵ or ⇥ that the parent let through) arrives as text; it isn't typed.
fn is_only_breaks(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| matches!(c, '\n' | '\r' | '\t'))
}

impl EntityInputHandler for PromptInput {
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

    /// Regular character input arrives here rather than through actions.
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
        let new_text = normalize(new_text);
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

    /// Intermediate IME text (CJK characters, "ё" via long press).
    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Utf16Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Utf16Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let new_text = normalize(new_text);
        let primary = self.document.selection().primary();
        let range = self
            .input_range(range_utf16)
            .unwrap_or(primary.from()..primary.to());
        let len = new_text.chars().count();
        // The selection inside the IME composition comes in UTF-16, relative to its start.
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
        let start = layout.geometry.caret(range.start);
        let end = layout.geometry.caret(range.end);
        let right = if end.y == start.y {
            end.x.max(start.x + px(1.))
        } else {
            start.x + px(1.)
        };
        Some(Bounds::from_corners(
            layout.origin + start,
            layout.origin + point(right, start.y + layout.geometry.line_height),
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

impl Focusable for PromptInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PromptInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use Direction::{Backward, Forward};
        let ui = Theme::ui(cx);
        div()
            .key_context(self.key_context())
            .track_focus(&self.focus_handle)
            .w_full()
            .text_color(ui.foreground)
            .font_family(theme::UI_FONT)
            .text_size(px(TEXT_SIZE))
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
            .on_action(cx.listener(|this, _: &MoveUp, _, cx| this.vertical(-1, false, cx)))
            .on_action(cx.listener(|this, _: &MoveDown, _, cx| this.vertical(1, false, cx)))
            .on_action(cx.listener(|this, _: &SelectUp, _, cx| this.vertical(-1, true, cx)))
            .on_action(cx.listener(|this, _: &SelectDown, _, cx| this.vertical(1, true, cx)))
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
            .on_action(cx.listener(|this, _: &MoveToStart, _, cx| {
                this.motion(cx, |_, r| movement::move_document_start(r, false))
            }))
            .on_action(cx.listener(|this, _: &MoveToEnd, _, cx| {
                this.motion(cx, |t, r| movement::move_document_end(t, r, false))
            }))
            .on_action(cx.listener(|this, _: &SelectToStart, _, cx| {
                this.motion(cx, |_, r| movement::move_document_start(r, true))
            }))
            .on_action(cx.listener(|this, _: &SelectToEnd, _, cx| {
                this.motion(cx, |t, r| movement::move_document_end(t, r, true))
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
            .on_action(cx.listener(|this, _: &InsertNewline, _, cx| {
                this.edit(EditKind::Insert, cx, |rope, sel| {
                    edit::insert_text(rope, sel, "\n")
                })
            }))
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
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(PromptElement { input: cx.entity() })
    }
}

// --- Geometry of the drawn rows ---

/// One drawn row: a logical line, or a part of it between soft wraps.
#[derive(Debug, Clone, PartialEq)]
struct Row {
    /// The row's first character and the one past its last (a line break isn't in it).
    start: usize,
    end: usize,
    /// The x of every character boundary from `start` to `end`, from the row's left edge.
    xs: Vec<Pixels>,
    /// A line break follows the row (it is the last row of a line that isn't the last line).
    newline: bool,
    /// More rows of the same line follow (the line wraps here).
    wraps: bool,
}

/// The rows as drawn, top to bottom: the caret, the mouse, the selection and ↑/↓ work on it.
#[derive(Debug, Clone, PartialEq)]
struct Geometry {
    rows: Vec<Row>,
    line_height: Pixels,
}

impl Geometry {
    fn height(&self) -> Pixels {
        self.line_height * self.rows.len().max(1) as f32
    }

    /// The row the caret at `pos` is on. At a soft wrap the caret is at the start of the next row,
    /// as editors show it.
    fn row_of(&self, pos: usize) -> usize {
        self.rows
            .iter()
            .rposition(|row| row.start <= pos)
            .unwrap_or(0)
    }

    /// The caret's top-left corner for `pos`, in the content.
    fn caret(&self, pos: usize) -> Point<Pixels> {
        let index = self.row_of(pos);
        let Some(row) = self.rows.get(index) else {
            return point(px(0.), px(0.));
        };
        let column = pos.clamp(row.start, row.end) - row.start;
        let x = row.xs.get(column).copied().unwrap_or(px(0.));
        point(x, self.line_height * index as f32)
    }

    /// The characters a click on a row can land before: a wrapped row's end is the next row's
    /// start, so a click past it stays before the last character (the space the line wrapped at).
    fn reachable(row: &Row) -> std::ops::RangeInclusive<usize> {
        let last = if row.wraps && row.end > row.start {
            row.end - 1
        } else {
            row.end
        };
        row.start..=last
    }

    /// The character boundary of row `index` closest to `x`.
    fn closest_in_row(&self, index: usize, x: Pixels) -> usize {
        let Some(row) = self.rows.get(index) else {
            return 0;
        };
        Self::reachable(row)
            .min_by(|a, b| {
                let distance = |pos: &usize| (row.xs[pos - row.start] - x).abs();
                distance(a)
                    .partial_cmp(&distance(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or(row.start)
    }

    /// The position for a point in the content (a click, the IME).
    fn index_at(&self, point: Point<Pixels>) -> usize {
        if self.rows.is_empty() {
            return 0;
        }
        let row = if point.y < px(0.) {
            0
        } else {
            ((point.y / self.line_height) as usize).min(self.rows.len() - 1)
        };
        self.closest_in_row(row, point.x)
    }

    /// `rows` rows up (negative) or down from `pos`, at `goal_x`; past the first row — the start of
    /// the text, past the last — its end.
    fn vertical(&self, pos: usize, goal_x: Pixels, rows: isize) -> usize {
        let Some(last) = self.rows.last() else {
            return pos;
        };
        let target = self.row_of(pos) as isize + rows;
        if target < 0 {
            return 0;
        }
        if target as usize >= self.rows.len() {
            return last.end;
        }
        self.closest_in_row(target as usize, goal_x)
    }

    /// The rectangles of the selection `from..to`, in the content: a part of every row it covers,
    /// a little past the end of a line whose break it takes.
    fn selection_rects(&self, from: usize, to: usize) -> Vec<Bounds<Pixels>> {
        let mut rects = Vec::new();
        if from >= to {
            return rects;
        }
        for (index, row) in self.rows.iter().enumerate() {
            let a = from.max(row.start);
            let b = to.min(row.end);
            let top = self.line_height * index as f32;
            let mut left = None;
            let mut right = px(0.);
            if a < b {
                left = Some(row.xs[a - row.start]);
                right = row.xs[b - row.start];
            }
            // The line break after the row is selected.
            if row.newline && from <= row.end && to > row.end {
                let end = row.xs[row.end - row.start];
                left = Some(left.unwrap_or(end));
                right = end + px(NEWLINE_WIDTH);
            }
            if let Some(left) = left {
                rects.push(Bounds::from_corners(
                    point(left, top),
                    point(right, top + self.line_height),
                ));
            }
        }
        rects
    }
}

/// A logical line's text as drawn: tabs as spaces (one character for one, so the positions
/// stay), no line break.
fn display_text(line: &str) -> String {
    line.trim_end_matches(['\n', '\r']).replace('\t', " ")
}

/// Character → byte offsets of `text`, one more than characters.
fn char_to_byte(text: &str) -> Vec<usize> {
    let mut offsets: Vec<usize> = text.char_indices().map(|(byte, _)| byte).collect();
    offsets.push(text.len());
    offsets
}

/// A logical line shaped for the frame.
struct ShapedLine {
    wrapped: WrappedLine,
    /// Top of the line's first row in the content.
    top: Pixels,
}

/// Shapes the text for `wrap_width`: the lines to paint and the rows' geometry.
#[allow(clippy::too_many_arguments)]
fn shape(
    text: &Rope,
    marked: Option<&std::ops::Range<usize>>,
    wrap_width: Pixels,
    color: Hsla,
    font_size: Pixels,
    line_height: Pixels,
    window: &mut Window,
) -> (Vec<ShapedLine>, Geometry) {
    let text_system = window.text_system().clone();
    let mut lines = Vec::new();
    let mut rows = Vec::new();
    let line_count = text.len_lines();
    for index in 0..line_count {
        let start = text.line_to_char(index);
        let display = display_text(&text.line(index).to_string());
        let offsets = char_to_byte(&display);
        let run = |len: usize, underline: bool| TextRun {
            len,
            font: font(theme::UI_FONT),
            color,
            background_color: None,
            underline: underline.then_some(UnderlineStyle {
                thickness: px(1.),
                color: Some(color),
                wavy: false,
            }),
            strikethrough: None,
        };
        // The IME composition is underlined.
        let runs: Vec<TextRun> = match marked {
            Some(marked) if !display.is_empty() => {
                let chars = offsets.len() - 1;
                let from = marked.start.saturating_sub(start).min(chars);
                let to = marked.end.saturating_sub(start).min(chars);
                let (a, b) = (offsets[from], offsets[to]);
                [
                    run(a, false),
                    run(b - a, true),
                    run(display.len() - b, false),
                ]
                .into_iter()
                .filter(|run| run.len > 0)
                .collect()
            }
            _ => vec![run(display.len(), false)],
        };
        let wrapped = text_system
            .shape_text(
                display.clone().into(),
                font_size,
                &runs,
                Some(wrap_width),
                None,
            )
            .ok()
            .and_then(|lines| lines.into_iter().next())
            .unwrap_or_default();
        let top = line_height * rows.len() as f32;
        // The rows' byte starts: the line's start and every soft wrap.
        let unwrapped = &wrapped.unwrapped_layout;
        let mut starts = vec![0];
        for boundary in wrapped.wrap_boundaries() {
            if let Some(glyph) = unwrapped
                .runs
                .get(boundary.run_ix)
                .and_then(|run| run.glyphs.get(boundary.glyph_ix))
            {
                starts.push(glyph.index);
            }
        }
        let byte_to_char = |byte: usize| offsets.partition_point(|&offset| offset < byte);
        for (row, &row_start) in starts.iter().enumerate() {
            let row_end = starts.get(row + 1).copied().unwrap_or(display.len());
            let (first, last) = (byte_to_char(row_start), byte_to_char(row_end));
            let origin = unwrapped.x_for_index(row_start);
            let xs = (first..=last)
                .map(|column| unwrapped.x_for_index(offsets[column]) - origin)
                .collect();
            let wraps = row + 1 < starts.len();
            rows.push(Row {
                start: start + first,
                end: start + last,
                xs,
                newline: !wraps && index + 1 < line_count,
                wraps,
            });
        }
        lines.push(ShapedLine { wrapped, top });
    }
    (
        lines,
        Geometry {
            rows,
            line_height,
        },
    )
}

/// The number of rows the text takes at `wrap_width` (the field's height).
fn count_rows(text: &Rope, wrap_width: Pixels, window: &mut Window) -> usize {
    let text_system = window.text_system().clone();
    let mut rows = 0;
    for index in 0..text.len_lines() {
        let display = display_text(&text.line(index).to_string());
        let runs = [TextRun {
            len: display.len(),
            font: font(theme::UI_FONT),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }];
        let wrapped = text_system
            .shape_text(display.into(), px(TEXT_SIZE), &runs, Some(wrap_width), None)
            .ok()
            .and_then(|lines| lines.into_iter().next());
        rows += wrapped.map_or(1, |line| line.wrap_boundaries().len() + 1);
    }
    rows.max(1)
}

// --- The element ---

/// The field's text (or placeholder), selection and caret.
struct PromptElement {
    input: Entity<PromptInput>,
}

struct PromptPrepaint {
    lines: Vec<ShapedLine>,
    geometry: Geometry,
    /// The placeholder instead of empty text.
    placeholder: Option<WrappedLine>,
    origin: Point<Pixels>,
    selection: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
}

impl IntoElement for PromptElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for PromptElement {
    type RequestLayoutState = ();
    type PrepaintState = PromptPrepaint;

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
        _cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let input = self.input.clone();
        let layout = window.request_measured_layout(style, move |known, available, window, cx| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(width) => Some(width),
                _ => None,
            });
            let (text, max_rows) = {
                let input = input.read(cx);
                (input.document.text().clone(), input.max_rows)
            };
            let rows = match width {
                Some(width) => count_rows(&text, width - px(CURSOR_WIDTH), window),
                None => text.len_lines(),
            };
            Size {
                width: width.unwrap_or(px(0.)),
                height: px(LINE_HEIGHT) * rows.clamp(1, max_rows) as f32,
            }
        });
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> PromptPrepaint {
        let ui = Theme::ui(cx);
        let input = self.input.read(cx);
        let text = input.document.text().clone();
        let primary = input.document.selection().primary();
        let marked = input.marked_range.clone();
        let focused = input.focus_handle.is_focused(window);
        let placeholder_text = input.placeholder.clone();
        let line_height = input.line_height();
        let mut scroll_y = input.scroll_y;
        let autoscroll = input.autoscroll;
        let font_size = px(TEXT_SIZE);
        let wrap_width = (bounds.size.width - px(CURSOR_WIDTH)).max(px(1.));

        let (lines, geometry) = shape(
            &text,
            marked.as_ref(),
            wrap_width,
            ui.foreground,
            font_size,
            line_height,
            window,
        );
        let placeholder = (text.len_chars() == 0 && !placeholder_text.is_empty())
            .then(|| {
                let runs = [TextRun {
                    len: placeholder_text.len(),
                    font: font(theme::UI_FONT),
                    color: ui.dim,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }];
                window
                    .text_system()
                    .shape_text(placeholder_text, font_size, &runs, Some(wrap_width), Some(1))
                    .ok()
                    .and_then(|lines| lines.into_iter().next())
            })
            .flatten();

        // The caret stays in view; the scroll never goes past the text.
        let caret = geometry.caret(primary.head);
        let visible = bounds.size.height;
        if autoscroll {
            if caret.y < scroll_y {
                scroll_y = caret.y;
            }
            if caret.y + line_height > scroll_y + visible {
                scroll_y = caret.y + line_height - visible;
            }
        }
        scroll_y = scroll_y.clamp(px(0.), (geometry.height() - visible).max(px(0.)));
        let origin = point(bounds.left(), bounds.top() - scroll_y);

        let selection = geometry
            .selection_rects(primary.from(), primary.to())
            .into_iter()
            .map(|rect| {
                fill(
                    Bounds::new(origin + rect.origin, rect.size),
                    ui.selection,
                )
            })
            .collect();
        let cursor = focused.then(|| {
            fill(
                Bounds::new(origin + caret, size(px(CURSOR_WIDTH), line_height)),
                ui.cursor,
            )
        });
        self.input.update(cx, |input, _| {
            input.scroll_y = scroll_y;
            input.autoscroll = false;
        });
        PromptPrepaint {
            lines,
            geometry,
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
        state: &mut PromptPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let line_height = state.geometry.line_height;
        let origin = state.origin;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for quad in state.selection.drain(..) {
                window.paint_quad(quad);
            }
            match &state.placeholder {
                Some(placeholder) => {
                    placeholder
                        .paint(bounds.origin, line_height, TextAlign::Left, None, window, cx)
                        .ok();
                }
                None => {
                    for line in &state.lines {
                        let top = origin.y + line.top;
                        // Lines out of view aren't drawn.
                        let rows = line.wrapped.wrap_boundaries().len() + 1;
                        if top > bounds.bottom() || top + line_height * (rows as f32) < bounds.top()
                        {
                            continue;
                        }
                        line.wrapped
                            .paint(
                                point(origin.x, top),
                                line_height,
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            )
                            .ok();
                    }
                }
            }
            if let Some(cursor) = state.cursor.take() {
                window.paint_quad(cursor);
            }
        });
        let geometry = std::mem::replace(
            &mut state.geometry,
            Geometry {
                rows: Vec::new(),
                line_height,
            },
        );
        self.input.update(cx, |input, _| {
            input.layout = Some(PromptLayout {
                geometry,
                origin,
                bounds,
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line of a monospace font: every character 10 px wide.
    fn row(start: usize, end: usize, newline: bool, wraps: bool) -> Row {
        Row {
            start,
            end,
            xs: (0..=end - start).map(|i| px(10. * i as f32)).collect(),
            newline,
            wraps,
        }
    }

    /// "hello world\nab" wrapped after "hello ": rows "hello " | "world" | "ab".
    fn geometry() -> Geometry {
        Geometry {
            rows: vec![
                row(0, 6, false, true),
                row(6, 11, true, false),
                row(12, 14, false, false),
            ],
            line_height: px(20.),
        }
    }

    #[test]
    fn the_caret_at_a_soft_wrap_starts_the_next_row() {
        let g = geometry();
        assert_eq!(g.caret(0), point(px(0.), px(0.)));
        assert_eq!(g.caret(5), point(px(50.), px(0.)));
        // "w" begins the second row.
        assert_eq!(g.caret(6), point(px(0.), px(20.)));
        // The end of the first logical line, then the second line.
        assert_eq!(g.caret(11), point(px(50.), px(20.)));
        assert_eq!(g.caret(12), point(px(0.), px(40.)));
        assert_eq!(g.caret(14), point(px(20.), px(40.)));
        assert_eq!(g.height(), px(60.));
    }

    #[test]
    fn clicks_find_the_closest_boundary_of_their_row() {
        let g = geometry();
        assert_eq!(g.index_at(point(px(24.), px(5.))), 2);
        assert_eq!(g.index_at(point(px(26.), px(5.))), 3);
        // Past the end of a wrapped row: before the space it wrapped at, not on the next row.
        assert_eq!(g.index_at(point(px(500.), px(5.))), 5);
        // Past the end of a line: its end.
        assert_eq!(g.index_at(point(px(500.), px(25.))), 11);
        // Above and below the text.
        assert_eq!(g.index_at(point(px(15.), px(-30.))), 1);
        assert_eq!(g.index_at(point(px(500.), px(900.))), 14);
    }

    #[test]
    fn up_and_down_move_between_drawn_rows() {
        let g = geometry();
        // From "o" of "world" up to the same x on the first row.
        assert_eq!(g.vertical(10, px(40.), -1), 4);
        // Down from the first row keeps the x.
        assert_eq!(g.vertical(2, px(20.), 1), 8);
        // Down into a shorter row: its end.
        assert_eq!(g.vertical(10, px(40.), 1), 14);
        // Past the first and the last rows: the start and the end of the text.
        assert_eq!(g.vertical(3, px(30.), -1), 0);
        assert_eq!(g.vertical(13, px(10.), 1), 14);
    }

    #[test]
    fn a_selection_covers_its_rows_and_line_breaks() {
        let g = geometry();
        let rects = g.selection_rects(3, 13);
        assert_eq!(rects.len(), 3);
        // "lo " on the first row.
        assert_eq!(rects[0], Bounds::from_corners(point(px(30.), px(0.)), point(px(60.), px(20.))));
        // "world" and its line break.
        assert_eq!(
            rects[1],
            Bounds::from_corners(point(px(0.), px(20.)), point(px(50. + NEWLINE_WIDTH), px(40.)))
        );
        // "a".
        assert_eq!(rects[2], Bounds::from_corners(point(px(0.), px(40.)), point(px(10.), px(60.))));
        assert!(g.selection_rects(4, 4).is_empty());
    }

    #[test]
    fn an_empty_line_in_a_selection_shows_its_break() {
        // "a\n\nb": three lines, the middle one empty.
        let g = Geometry {
            rows: vec![
                row(0, 1, true, false),
                row(2, 2, true, false),
                row(3, 4, false, false),
            ],
            line_height: px(20.),
        };
        let rects = g.selection_rects(0, 4);
        assert_eq!(rects.len(), 3);
        assert_eq!(
            rects[1],
            Bounds::from_corners(point(px(0.), px(20.)), point(px(NEWLINE_WIDTH), px(40.)))
        );
    }

    #[test]
    fn text_is_drawn_without_breaks_and_tabs_keep_positions() {
        assert_eq!(display_text("a\tb\n"), "a b");
        assert_eq!(display_text("x\r\n"), "x");
        assert_eq!(char_to_byte("aж"), vec![0, 1, 3]);
        assert_eq!(normalize("a\r\nb\rc"), "a\nb\nc");
        assert!(is_only_breaks("\n") && is_only_breaks("\t") && !is_only_breaks("a\n"));
    }
}
