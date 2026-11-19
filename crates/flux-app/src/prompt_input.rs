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
//!
//! The parent may style the text as it is typed ([`PromptInput::set_styling`]: a plugin
//! conversation's Markdown — bold, italic, code, links): the stretches are computed again only
//! when the text changed, and the rows are shaped with them, so the caret, the selection, the
//! mouse and the wrapping follow the styled text. The styling's marks (`**`, `[`, `](address)`)
//! are hidden, as the messengers' fields show formatting ([`conceal`]): the document stays the
//! Markdown, the rows are shaped from the shown characters (a hidden mark takes no room), the
//! motions, ⌫ ⌦, the mouse, copy and paste work on what is shown, and the caret never stops
//! between marks. Claude's field has none.

use std::ops::Range as Utf16Range;
use std::path::PathBuf;
use std::time::Instant;

use flux_core::movement::{self, Direction};
use flux_core::{Document, EditKind, Range, Rope, Selection, Transaction, edit};
use gpui::{
    App, AvailableSpace, Bounds, ClipboardEntry, ClipboardItem, ContentMask, Context, CursorStyle,
    Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, GlobalElementId, Image, InspectorElementId, IntoElement, KeyBinding, KeyContext,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, PaintQuad, Pixels, Point, Render,
    ScrollWheelEvent, SharedString, Size, StrikethroughStyle, Style, TextAlign, TextRun,
    UTF16Selection, UnderlineStyle, Window, WrappedLine, actions, div, fill, font, point,
    prelude::*, px, relative, size,
};

use std::rc::Rc;

use crate::motion;
use crate::theme::{self, Theme, UiColors};

mod conceal;

use conceal::Concealed;
pub use conceal::Span;

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
    crate::keymap::bind_keys(
        cx,
        [
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
        ],
    );
    // After the field's own keys: with a list open, these win over ↑ ↓ above.
    let menu = Some("PromptInput && showing_menu");
    crate::keymap::bind_keys(
        cx,
        [
            KeyBinding::new("down", MenuNext, menu),
            KeyBinding::new("up", MenuPrevious, menu),
            KeyBinding::new("enter", MenuAccept, menu),
            KeyBinding::new("tab", MenuAccept, menu),
            KeyBinding::new("escape", MenuDismiss, menu),
        ],
    );
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
    /// ⌘V with files copied in Finder or Explorer, in a field that takes files
    /// ([`PromptInput::takes_files`]): the parent attaches them.
    FilesPasted(Vec<PathBuf>),
    /// ⌫ with the caret at the start and nothing selected: nothing to delete (the parent leaves a
    /// mode of the field — the shell mode of Claude's field, part 10.2).
    BackspaceAtStart,
    /// ↑ in the empty field: nowhere to go (the parent may take it — a plugin's conversation edits
    /// the user's last message, as the messengers do).
    UpWhenEmpty,
}

/// How a stretch of the field's text looks: the parent's styling of what is typed
/// ([`PromptInput::set_styling`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Look {
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    /// The code font, on a faint background.
    pub code: bool,
    /// A link's text: the accent, underlined.
    pub link: bool,
    /// On the spoiler's veil (lighter than the feed's: the text stays readable).
    pub spoiler: bool,
}

/// The parent's markup of the text: the looks of its stretches — character ranges in order, not
/// overlapping; the rest is plain — and its spans, whose marks the field hides.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Markup {
    pub looks: Vec<(std::ops::Range<usize>, Look)>,
    pub spans: Vec<Span>,
}

/// The parent's styling: the markup of the text.
pub type Styling = fn(&str) -> Markup;

/// A stretch of the text (characters or drawn columns) and how it looks.
type Stretch = (std::ops::Range<usize>, Look);

/// The text drawn last, styled: made again only when the text changed.
struct Stretches {
    text: String,
    /// Its logical lines as drawn.
    lines: Vec<DrawnLine>,
    /// Its marks hidden.
    concealed: Concealed,
}

/// A logical line as drawn: its shown characters and their looks.
#[derive(Debug, PartialEq)]
struct DrawnLine {
    /// The shown characters: no hidden marks, tabs as spaces, no line break.
    display: String,
    /// The drawn column of each position of the line (`0..=` its length); empty when nothing in
    /// the line is hidden.
    columns: Vec<usize>,
    /// The looks' stretches, in drawn columns.
    looks: Vec<(std::ops::Range<usize>, Look)>,
}

impl Stretches {
    /// The lines of `text` with `markup`: its marks left out, its looks cut at the line breaks.
    fn new(text: &Rope, markup: Markup) -> Self {
        let concealed = Concealed::new(text, markup.spans);
        let looks = markup.looks;
        let mut lines = Vec::with_capacity(text.len_lines());
        let mut next = 0;
        for index in 0..text.len_lines() {
            let start = text.line_to_char(index);
            let line = text.line(index);
            let len = display_len(&line);
            let end = start + len;
            let mut display = String::with_capacity(line.len_bytes());
            let mut columns = Vec::with_capacity(len + 1);
            let mut shown = 0;
            for (offset, c) in line.chars().take(len).enumerate() {
                columns.push(shown);
                if concealed.hides() && concealed.is_hidden(start + offset) {
                    continue;
                }
                display.push(if c == '\t' { ' ' } else { c });
                shown += 1;
            }
            columns.push(shown);
            if shown == len {
                columns.clear();
            }
            while looks.get(next).is_some_and(|(range, _)| range.end <= start) {
                next += 1;
            }
            let column = |pos: usize| {
                if columns.is_empty() {
                    pos
                } else {
                    columns[pos]
                }
            };
            let line_looks = looks[next..]
                .iter()
                .take_while(|(range, _)| range.start < end)
                .filter_map(|(range, look)| {
                    let from = column(range.start.max(start) - start);
                    let to = column(range.end.min(end) - start);
                    (from < to).then_some((from..to, *look))
                })
                .collect();
            lines.push(DrawnLine {
                display,
                columns,
                looks: line_looks,
            });
        }
        Stretches {
            text: text.to_string(),
            lines,
            concealed,
        }
    }
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
    /// A change made for the user — the message sent, one from the history, a paste — glides the
    /// field's height to its new number of rows ([`motion::SMOOTH`]): the height it starts from,
    /// and since when. Typing changes the height at once.
    height_glide: Option<(Pixels, Instant)>,
    /// The height drawn last: where a glide starts from.
    drawn_height: Pixels,
    /// Files on the clipboard go to the parent ([`PromptInputEvent::FilesPasted`]), not their names
    /// as text.
    takes_files: bool,
    /// The parent's styling of the text, and its stretches for the text drawn last.
    styling: Option<Styling>,
    styled: Option<Rc<Stretches>>,
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
            height_glide: None,
            drawn_height: px(0.),
            takes_files: false,
            styling: None,
            styled: None,
        }
    }

    /// Styles the text as it is typed (none — plain): the parent's Markdown, say. The stretches
    /// are made again (functions can't be told apart).
    pub fn set_styling(&mut self, styling: Option<Styling>, cx: &mut Context<Self>) {
        if self.styling.is_none() && styling.is_none() {
            return;
        }
        self.styling = styling;
        self.styled = None;
        cx.notify();
    }

    /// The stretches of the text as it is now: made again only when the text changed.
    fn restyle(&mut self) {
        let Some(styling) = self.styling else {
            self.styled = None;
            return;
        };
        let text = self.document.text();
        if self
            .styled
            .as_ref()
            .is_some_and(|styled| *text == styled.text.as_str())
        {
            return;
        }
        let markup = styling(&text.to_string());
        self.styled = Some(Rc::new(Stretches::new(text, markup)));
    }

    /// The styling as it is now, when it hides marks: the edits and the motions work on what is
    /// shown with it.
    fn concealed(&mut self) -> Option<Rc<Stretches>> {
        self.restyle();
        self.styled
            .clone()
            .filter(|styled| styled.concealed.hides())
    }

    /// Where the caret rests at the place of `pos` (the marks hidden as drawn last).
    fn snap(&self, pos: usize) -> usize {
        match &self.styled {
            Some(styled) if styled.concealed.hides() => styled
                .concealed
                .snap(pos)
                .min(self.document.text().len_chars()),
            _ => pos,
        }
    }

    /// An empty pair the formatting put at the caret goes when the caret leaves it with nothing
    /// typed in it.
    fn drop_blank_spans(&mut self, cx: &mut Context<Self>) {
        let Some(styled) = self.concealed() else {
            return;
        };
        let changes = styled.concealed.blank_spans(self.cursor());
        if !changes.is_empty() {
            let tx = Transaction::change(self.document.text(), changes);
            self.apply(tx, EditKind::Other, cx);
        }
    }

    /// Where the character at `index` is drawn, in the window (the frame drawn last): the parent's
    /// list for a word opens by its first character.
    pub fn bounds_of(&self, index: usize) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let caret = layout.geometry.caret(index);
        Some(Bounds::new(
            layout.origin + caret,
            size(px(1.), layout.geometry.line_height),
        ))
    }

    /// The field grows up to `rows` rows, then scrolls.
    pub fn max_rows(mut self, rows: usize) -> Self {
        self.max_rows = rows.max(1);
        self
    }

    /// Files copied in Finder or Explorer are pasted as files, for the parent to attach
    /// ([`PromptInputEvent::FilesPasted`]); otherwise their names (Finder) or paths (Explorer) go
    /// in as text.
    pub fn takes_files(mut self) -> Self {
        self.takes_files = true;
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

    /// The text to send (FluxGram part 5): the field's text without the empty pairs the
    /// formatting keys put at the caret — ⌘B, then ↵ with nothing typed, sends no `****`.
    pub fn text_to_send(&mut self) -> String {
        let text = self.document.text().clone();
        let Some(styled) = self.concealed() else {
            return text.to_string();
        };
        let mut out = String::new();
        let mut last = 0;
        for (start, end, _) in styled.concealed.blank_spans(usize::MAX) {
            out.push_str(&text.slice(last..start).to_string());
            last = end;
        }
        out.push_str(&text.slice(last..).to_string());
        out
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

    /// Replaces the entire text (as one edit that can be undone), caret at the end. The field's
    /// height glides to the new text (a message sent, one from the history).
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = normalize(text);
        let len = self.document.text().len_chars();
        self.glide_height();
        self.replace(0..len, &text, cx);
    }

    /// The next change of the text glides the field's height from the height drawn last.
    fn glide_height(&mut self) {
        self.height_glide = (motion::enabled() && self.drawn_height > px(0.))
            .then(|| (self.drawn_height, Instant::now()));
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

    /// The primary selection's characters, in order; an empty range is the caret.
    pub fn selected_range(&self) -> std::ops::Range<usize> {
        let primary = self.document.selection().primary();
        primary.from()..primary.to()
    }

    /// Replaces the characters of `range` with `text` as one edit that can be undone, the
    /// characters `selected` of the new text selected after: a formatting mark put around the
    /// selection keeps it selected.
    pub fn replace_selecting(
        &mut self,
        range: std::ops::Range<usize>,
        text: &str,
        selected: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) {
        let len = self.document.text().len_chars();
        let (start, end) = (range.start.min(len), range.end.min(len));
        let text = normalize(text);
        let new_len = len - (end - start) + text.chars().count();
        let selection = Selection::single(selected.start.min(new_len), selected.end.min(new_len));
        let tx = Transaction::change(self.document.text(), [(start, end, Some(text))])
            .with_selection(selection);
        self.apply(tx, EditKind::Other, cx);
    }

    /// Types `text` at the caret (over the selection).
    pub fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = normalize(text);
        self.type_text(&text, EditKind::Other, cx);
    }

    /// Types `text` at the caret, over the selection: where the field hides marks, those the
    /// selection crosses stay balanced ([`Concealed::delete`]).
    fn type_text(&mut self, text: &str, kind: EditKind, cx: &mut Context<Self>) {
        let primary = self.document.selection().primary();
        if !primary.is_empty()
            && let Some(styled) = self.concealed()
        {
            let (changes, caret) =
                styled
                    .concealed
                    .delete(primary.from(), primary.to(), text, primary.from());
            let tx = Transaction::change(self.document.text(), changes)
                .with_selection(Selection::point(caret));
            return self.apply(tx, kind, cx);
        }
        self.edit(kind, cx, |rope, sel| edit::insert_text(rope, sel, text));
    }

    /// ⌫ ⌦ ⌥⌫ where the field hides marks: the selection, or the shown character (or word) before
    /// or after the caret — never a mark alone; the marks stay balanced and a span left empty
    /// loses them. False when nothing is hidden: the core's deletion does.
    fn delete_shown(&mut self, dir: Direction, word: bool, cx: &mut Context<Self>) -> bool {
        let Some(styled) = self.concealed() else {
            return false;
        };
        let concealed = &styled.concealed;
        let primary = self.document.selection().primary();
        let (from, to, caret) = if primary.is_empty() {
            let place = concealed.place(primary.head);
            let at = Range::point(place);
            let target = if word {
                movement::move_word(&concealed.shown, at, dir, false).head
            } else {
                movement::move_horizontally(&concealed.shown, at, dir, false).head
            };
            if target == place {
                return true;
            }
            let (a, b) = (place.min(target), place.max(target));
            let from = concealed.position_of(a);
            (from, concealed.position_of(b - 1) + 1, primary.head)
        } else {
            (primary.from(), primary.to(), primary.from())
        };
        let (changes, caret) = concealed.delete(from, to, "", caret);
        let tx = Transaction::change(self.document.text(), changes)
            .with_selection(Selection::point(caret));
        self.apply(tx, EditKind::Delete, cx);
        true
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

    /// A motion of the core; where the field hides marks, on the shown text.
    fn motion(&mut self, cx: &mut Context<Self>, f: impl Fn(&Rope, Range) -> Range) {
        self.goal_x = None;
        let concealed = self.concealed();
        let text = self.document.text().clone();
        let selection = self
            .document
            .selection()
            .transform(|range| match &concealed {
                Some(styled) => styled.concealed.moved(*range, &f),
                None => f(&text, *range),
            });
        self.set_selection(selection, cx);
        self.drop_blank_spans(cx);
    }

    /// ↑/↓: the row above or below as drawn, keeping the x the caret had on the first press.
    fn vertical(&mut self, rows: isize, extend: bool, cx: &mut Context<Self>) {
        self.restyle();
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
        let goal = self.goal_x.unwrap_or_else(|| layout.geometry.caret(from).x);
        let target = self.snap(layout.geometry.vertical(from, goal, rows));
        let range = if extend {
            primary.put_cursor(target, true)
        } else {
            Range::point(target)
        };
        self.set_selection(Selection::from_range(range), cx);
        self.drop_blank_spans(cx);
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

    /// The selection for the clipboard. Where the field hides marks: the shown text, and the
    /// Markdown that keeps its formatting as the item's metadata — a field with a styling pastes
    /// that back.
    fn clipboard_item(&mut self) -> Option<ClipboardItem> {
        let range = self.document.selection().primary();
        if range.is_empty() {
            return None;
        }
        let (from, to) = (range.from(), range.to());
        let Some(styled) = self.concealed() else {
            let text = self.document.text().slice(from..to).to_string();
            return Some(ClipboardItem::new_string(text));
        };
        let plain = styled.concealed.shown_text(from, to);
        let markdown = styled.concealed.markdown(self.document.text(), from, to);
        Some(if plain == markdown {
            ClipboardItem::new_string(plain)
        } else {
            let metadata = serde_json::json!({ MARKDOWN: markdown }).to_string();
            ClipboardItem::new_string_with_metadata(plain, metadata)
        })
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = self.clipboard_item() {
            cx.write_to_clipboard(item);
        }
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = self.clipboard_item() {
            cx.write_to_clipboard(item);
            if !self.delete_shown(Direction::Backward, false, cx) {
                self.edit(EditKind::Other, cx, |rope, sel| {
                    edit::insert_text(rope, sel, "")
                });
            }
        }
    }

    /// Text goes in at the caret; a picture (a screenshot copied to the clipboard) goes to the
    /// parent, and so do files copied in Finder or Explorer where the field takes them.
    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        if self.takes_files
            && let Some(paths) = item.paths()
        {
            return cx.emit(PromptInputEvent::FilesPasted(paths.to_vec()));
        }
        if let Some(text) = item.text() {
            // Copied from a field that hides marks: its Markdown, where this one is styled too.
            let markdown = self
                .styling
                .and_then(|_| item.metadata())
                .and_then(|metadata| serde_json::from_str::<serde_json::Value>(metadata).ok())
                .and_then(|metadata| Some(metadata.get(MARKDOWN)?.as_str()?.to_string()));
            let text = normalize(markdown.as_deref().unwrap_or(&text));
            self.glide_height();
            return self.type_text(&text, EditKind::Other, cx);
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
        let to = range.to();
        if let Some(styled) = self.concealed() {
            let (changes, caret) = styled.concealed.delete(from, to, "", from);
            let tx = Transaction::change(self.document.text(), changes)
                .with_selection(Selection::point(caret));
            return self.apply(tx, EditKind::Delete, cx);
        }
        let tx = Transaction::change(self.document.text(), [(from, to, None)])
            .with_selection(Selection::point(from));
        self.apply(tx, EditKind::Delete, cx);
    }

    // --- Mouse ---

    /// The position for a point in the window: where the caret rests at the place drawn there.
    fn position_for_mouse(&self, position: Point<Pixels>) -> usize {
        match &self.layout {
            Some(layout) => self.snap(layout.geometry.index_at(position - layout.origin)),
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
        let concealed = self.concealed();
        let pos = self.position_for_mouse(event.position);
        let text = self.document.text();
        let selection = match event.click_count {
            // The word as shown: its marks with it.
            2 => match &concealed {
                Some(styled) => {
                    let concealed = &styled.concealed;
                    let word = movement::word_range_at(&concealed.shown, concealed.place(pos));
                    Selection::single(concealed.rest(word.anchor), concealed.rest(word.head))
                }
                None => Selection::from_range(movement::word_range_at(text, pos)),
            },
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
        self.drop_blank_spans(cx);
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

/// The clipboard metadata's key for the Markdown of a copy from a field that hides marks.
const MARKDOWN: &str = "markdown";

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
            None => return self.type_text(&new_text, EditKind::Insert, cx),
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
        self.restyle();
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
            .on_action(cx.listener(|this, _: &MoveUp, _, cx| {
                if this.is_empty() {
                    cx.emit(PromptInputEvent::UpWhenEmpty);
                    return;
                }
                this.vertical(-1, false, cx)
            }))
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
                if this.cursor() == 0 && !this.has_selection() {
                    cx.emit(PromptInputEvent::BackspaceAtStart);
                    return;
                }
                if !this.delete_shown(Backward, false, cx) {
                    this.edit(EditKind::Delete, cx, edit::delete_backward)
                }
            }))
            .on_action(cx.listener(|this, _: &Delete, _, cx| {
                if !this.delete_shown(Forward, false, cx) {
                    this.edit(EditKind::Delete, cx, edit::delete_forward)
                }
            }))
            .on_action(cx.listener(|this, _: &DeleteWordBackward, _, cx| {
                if !this.delete_shown(Backward, true, cx) {
                    this.edit(EditKind::Other, cx, edit::delete_word_backward)
                }
            }))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(|this, _: &InsertNewline, _, cx| {
                this.type_text("\n", EditKind::Insert, cx)
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

/// The number of characters a logical line draws (without its line break).
fn display_len(line: &flux_core::RopeSlice) -> usize {
    let mut len = line.len_chars();
    while len > 0 && matches!(line.char(len - 1), '\n' | '\r') {
        len -= 1;
    }
    len
}

/// The runs of a logical line's drawn text: its stretches' looks, and the IME composition
/// (`marked`, in the line's characters) underlined. `offsets` are the text's character → byte
/// offsets.
fn line_runs(
    display: &str,
    offsets: &[usize],
    looks: &[(std::ops::Range<usize>, Look)],
    marked: Option<std::ops::Range<usize>>,
    ui: &UiColors,
) -> Vec<TextRun> {
    let chars = offsets.len() - 1;
    // Where a run may change: the stretches' ends and the composition's.
    let mut cuts = vec![0, chars];
    for (range, _) in looks {
        cuts.extend([range.start.min(chars), range.end.min(chars)]);
    }
    if let Some(marked) = &marked {
        cuts.extend([marked.start.min(chars), marked.end.min(chars)]);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut runs = Vec::with_capacity(cuts.len());
    let mut next = 0;
    for pair in cuts.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        while looks.get(next).is_some_and(|(range, _)| range.end <= from) {
            next += 1;
        }
        let look = looks
            .get(next)
            .filter(|(range, _)| range.start <= from)
            .map(|(_, look)| *look)
            .unwrap_or_default();
        let underline = marked
            .as_ref()
            .is_some_and(|marked| marked.start <= from && to <= marked.end);
        runs.push(styled_run(offsets[to] - offsets[from], look, underline, ui));
    }
    if runs.is_empty() {
        runs.push(styled_run(display.len(), Look::default(), false, ui));
    }
    runs
}

/// A run of `len` bytes that looks so.
fn styled_run(len: usize, look: Look, underline: bool, ui: &UiColors) -> TextRun {
    let mut run_font = font(if look.code {
        theme::code_font()
    } else {
        theme::UI_FONT
    });
    if look.bold {
        run_font.weight = gpui::FontWeight::SEMIBOLD;
    }
    if look.italic {
        run_font.style = gpui::FontStyle::Italic;
    }
    let color = if look.link {
        ui.accent_text
    } else {
        ui.foreground
    };
    TextRun {
        len,
        font: run_font,
        color,
        background_color: if look.spoiler {
            Some(UiColors::tint(ui.text_muted, 0.25))
        } else if look.code {
            Some(UiColors::tint(ui.foreground, 0.08))
        } else {
            None
        },
        underline: (underline || look.link).then_some(UnderlineStyle {
            thickness: px(1.),
            color: Some(color),
            wavy: false,
        }),
        strikethrough: look.strikethrough.then_some(StrikethroughStyle {
            thickness: px(1.),
            color: Some(color),
        }),
    }
}

/// Character → byte offsets of `text`, one more than characters.
fn char_to_byte(text: &str) -> Vec<usize> {
    let mut offsets: Vec<usize> = text.char_indices().map(|(byte, _)| byte).collect();
    offsets.push(text.len());
    offsets
}

/// A logical line as drawn: the styling's line (its marks left out), or the plain text's.
fn drawn<'a>(
    text: &Rope,
    index: usize,
    styled: Option<&'a Stretches>,
    plain: &'a mut String,
) -> (&'a str, &'a [usize], &'a [Stretch]) {
    match styled.and_then(|styled| styled.lines.get(index)) {
        Some(line) => (&line.display, &line.columns, &line.looks),
        None => {
            *plain = display_text(&text.line(index).to_string());
            (plain.as_str(), &[], &[])
        }
    }
}

/// A logical line shaped for the frame.
struct ShapedLine {
    wrapped: WrappedLine,
    /// Top of the line's first row in the content.
    top: Pixels,
}

/// Shapes the text for `wrap_width`: the lines to paint and the rows' geometry. `styled` — the
/// parent's stretches for this text.
#[allow(clippy::too_many_arguments)]
fn shape(
    text: &Rope,
    styled: Option<&Stretches>,
    marked: Option<&std::ops::Range<usize>>,
    wrap_width: Pixels,
    ui: &UiColors,
    font_size: Pixels,
    line_height: Pixels,
    window: &mut Window,
) -> (Vec<ShapedLine>, Geometry) {
    let text_system = window.text_system().clone();
    let mut lines = Vec::new();
    let mut rows = Vec::new();
    let line_count = text.len_lines();
    let mut plain = String::new();
    for index in 0..line_count {
        let start = text.line_to_char(index);
        let (display, columns, looks) = drawn(text, index, styled, &mut plain);
        let offsets = char_to_byte(display);
        let chars = offsets.len() - 1;
        // The line's positions: its drawn columns, and back (a hidden mark's positions draw at
        // the shown character after it).
        let len = columns.len().checked_sub(1).unwrap_or(chars);
        let column = |pos: usize| columns.get(pos).copied().unwrap_or(pos.min(chars));
        let position = |column: usize| match columns {
            [] => column,
            columns => columns.partition_point(|&at| at < column),
        };
        // The IME composition is underlined.
        let marked = marked
            .filter(|_| !display.is_empty())
            .map(|marked| {
                column(marked.start.saturating_sub(start).min(len))
                    ..column(marked.end.saturating_sub(start).min(len))
            })
            .filter(|marked| !marked.is_empty());
        let runs = line_runs(display, &offsets, looks, marked, ui);
        let wrapped = text_system
            .shape_text(
                display.to_string().into(),
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
            let first = position(byte_to_char(row_start));
            let last = match starts.get(row + 1) {
                Some(&next) => position(byte_to_char(next)),
                None => len,
            };
            let origin = unwrapped.x_for_index(row_start);
            let xs = (first..=last)
                .map(|pos| unwrapped.x_for_index(offsets[column(pos)]) - origin)
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
    (lines, Geometry { rows, line_height })
}

/// The number of rows the text takes at `wrap_width` (the field's height), styled as drawn.
fn count_rows(
    text: &Rope,
    styled: Option<&Stretches>,
    wrap_width: Pixels,
    ui: &UiColors,
    window: &mut Window,
) -> usize {
    let text_system = window.text_system().clone();
    let mut rows = 0;
    let mut plain = String::new();
    for index in 0..text.len_lines() {
        let (display, _, looks) = drawn(text, index, styled, &mut plain);
        let runs = line_runs(display, &char_to_byte(display), looks, None, ui);
        let wrapped = text_system
            .shape_text(
                display.to_string().into(),
                px(TEXT_SIZE),
                &runs,
                Some(wrap_width),
                None,
            )
            .ok()
            .and_then(|lines| lines.into_iter().next());
        rows += wrapped.map_or(1, |line| line.wrap_boundaries().len() + 1);
    }
    rows.max(1)
}

// --- The element ---

/// The field's height `since` a glide started from `from` toward `to` ([`motion::SMOOTH`]).
fn glided(from: Pixels, to: Pixels, since: Instant) -> Pixels {
    let (t, done) = motion::SMOOTH.at(since.elapsed());
    if done { to } else { from + (to - from) * t }
}

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
            let (text, styled, max_rows, glide) = {
                let input = input.read(cx);
                (
                    input.document.text().clone(),
                    input.styled.clone(),
                    input.max_rows,
                    input.height_glide,
                )
            };
            let ui = Theme::ui(cx);
            let rows = match width {
                Some(width) => count_rows(
                    &text,
                    styled.as_deref(),
                    width - px(CURSOR_WIDTH),
                    &ui,
                    window,
                ),
                None => text.len_lines(),
            };
            let height = px(LINE_HEIGHT) * rows.clamp(1, max_rows) as f32;
            Size {
                width: width.unwrap_or(px(0.)),
                height: glide.map_or(height, |(from, since)| glided(from, height, since)),
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
        let styled = input.styled.clone();
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
            styled.as_deref(),
            marked.as_ref(),
            wrap_width,
            &ui,
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
                    .shape_text(
                        placeholder_text,
                        font_size,
                        &runs,
                        Some(wrap_width),
                        Some(1),
                    )
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
            .map(|rect| fill(Bounds::new(origin + rect.origin, rect.size), ui.selection))
            .collect();
        let cursor = focused.then(|| {
            fill(
                Bounds::new(origin + caret, size(px(CURSOR_WIDTH), line_height)),
                ui.cursor,
            )
        });
        let gliding = self.input.update(cx, |input, _| {
            input.scroll_y = scroll_y;
            input.autoscroll = false;
            input.drawn_height = bounds.size.height;
            // The glide is over, or the rows didn't change: no more frames.
            let rest = geometry
                .height()
                .clamp(line_height, line_height * input.max_rows as f32);
            input.height_glide.take_if(|(_, since)| {
                (bounds.size.height - rest).abs() < px(0.5)
                    || !motion::running(*since, motion::SMOOTH)
            });
            input.height_glide.is_some()
        });
        if gliding {
            window.request_animation_frame();
        }
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
        // Lines out of view aren't drawn.
        let shown: Vec<&ShapedLine> = state
            .lines
            .iter()
            .filter(|line| {
                let top = origin.y + line.top;
                let rows = line.wrapped.wrap_boundaries().len() + 1;
                top <= bounds.bottom() && top + line_height * (rows as f32) >= bounds.top()
            })
            .collect();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            // The styled text's backgrounds (code, a spoiler's veil) under the selection.
            if state.placeholder.is_none() {
                for line in &shown {
                    let at = point(origin.x, origin.y + line.top);
                    line.wrapped
                        .paint_background(at, line_height, TextAlign::Left, None, window, cx)
                        .ok();
                }
            }
            for quad in state.selection.drain(..) {
                window.paint_quad(quad);
            }
            match &state.placeholder {
                Some(placeholder) => {
                    placeholder
                        .paint(
                            bounds.origin,
                            line_height,
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        )
                        .ok();
                }
                None => {
                    for line in &shown {
                        let at = point(origin.x, origin.y + line.top);
                        line.wrapped
                            .paint(at, line_height, TextAlign::Left, None, window, cx)
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
    fn styled_lines_leave_the_marks_out() {
        let bold = Look {
            bold: true,
            ..Look::default()
        };
        // The bold span's marks hidden, its looks cut at the line break, in drawn columns.
        let text = Rope::from_str("**one\ntwo** x\n");
        let markup = Markup {
            looks: vec![(2..9, bold)],
            spans: vec![Span {
                open: 0..2,
                close: 9..11,
            }],
        };
        let line = |display: &str, columns: Vec<usize>, looks| DrawnLine {
            display: display.to_string(),
            columns,
            looks,
        };
        assert_eq!(
            Stretches::new(&text, markup).lines,
            vec![
                line("one", vec![0, 0, 0, 1, 2, 3], vec![(0..3, bold)]),
                line("two x", vec![0, 1, 2, 3, 3, 3, 4, 5], vec![(0..3, bold)]),
                line("", vec![], vec![]),
            ]
        );
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
        assert_eq!(
            rects[0],
            Bounds::from_corners(point(px(30.), px(0.)), point(px(60.), px(20.)))
        );
        // "world" and its line break.
        assert_eq!(
            rects[1],
            Bounds::from_corners(
                point(px(0.), px(20.)),
                point(px(50. + NEWLINE_WIDTH), px(40.))
            )
        );
        // "a".
        assert_eq!(
            rects[2],
            Bounds::from_corners(point(px(0.), px(40.)), point(px(10.), px(60.)))
        );
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
