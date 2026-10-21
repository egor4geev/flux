//! Selecting the text of a message in the Claude chat with the mouse, and copying it (⌘C): Claude's
//! answers — paragraphs, headings, list items, quotes, table cells, code blocks — and the user's
//! own messages (the author, after 10.8: "can't copy part of the answer").
//!
//! A selection lives in one message (an entry). Its ends are [`Point`]s: a text of the message (the
//! texts in the order they are drawn) and a byte in it. While the message is drawn, every text
//! takes its index from [`Collect`], gets the selected stretch on a background and, once laid out,
//! leaves its layout in the transcript's [`Drawn`] store; the mouse finds its point in those
//! layouts. The list draws only the messages in view, so nothing else is kept.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, DispatchPhase, Element, ElementId,
    GlobalElementId, Hsla, InspectorElementId, IntoElement, KeyBinding, KeyDownEvent, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, SharedString, Styled,
    TextLayout, TextRun, WeakEntity, Window, actions, canvas, prelude::*,
};

use flux_claude::EntryId;

use crate::claude_chat::ClaudeChat;

actions!(
    claude_chat,
    [
        /// ⌘C: the selected text of a message.
        CopySelection,
        /// Esc: the selection goes (only then — otherwise Esc does what it did).
        ClearSelection,
    ]
);

pub fn init(cx: &mut App) {
    let context = Some("ClaudeChat");
    cx.bind_keys([
        KeyBinding::new("cmd-c", CopySelection, context),
        KeyBinding::new("escape", ClearSelection, context),
    ]);
}

/// A place in a message's text: the text's index in drawing order, a byte in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub text: usize,
    pub byte: usize,
}

/// How a text joins the one before it in what's copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Joint {
    /// A new block: an empty line between (paragraphs, headings, code).
    Block,
    /// A new line (the next list item, the next row of a table).
    Line,
    /// The next cell of a table's row: a tab.
    Cell,
}

/// The selection of a transcript: the message, where the mouse went down and where it is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub entry: EntryId,
    pub anchor: Point,
    pub head: Point,
    /// The mouse is still down: moves extend the selection.
    pub dragging: bool,
}

impl Selection {
    /// The selection's start and end, in order.
    pub fn range(&self) -> (Point, Point) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// The selected bytes of the text `index` (of `len` bytes) under the selection `start..end`.
pub fn range_in(start: Point, end: Point, index: usize, len: usize) -> Option<Range<usize>> {
    if index < start.text || index > end.text {
        return None;
    }
    let from = if index == start.text { start.byte } else { 0 };
    let to = if index == end.text { end.byte } else { len };
    let range = from.min(len)..to.min(len);
    (!range.is_empty()).then_some(range)
}

/// One drawn text of a message: what it says, how it joins the one before, the list marker in
/// front of it, its layout once laid out this frame.
pub struct Text {
    pub text: SharedString,
    pub joint: Joint,
    pub marker: Option<SharedString>,
    layout: Option<TextLayout>,
}

/// The texts of a message as drawn last, by the frame (the transcript's draw count) they were
/// drawn in: a new frame starts the list over.
#[derive(Default)]
pub struct Texts {
    frame: u64,
    texts: Vec<Text>,
}

/// The drawn texts of the messages in view, by entry.
pub type Drawn = Rc<RefCell<HashMap<EntryId, Texts>>>;

/// Forgets the texts of messages not drawn in the frame before `frame` (scrolled away), but those
/// of the selection's message.
pub fn prune(drawn: &Drawn, frame: u64, selection: Option<&Selection>) {
    drawn.borrow_mut().retain(|entry, texts| {
        texts.frame + 1 >= frame || selection.is_some_and(|selection| selection.entry == *entry)
    });
}

/// What a message's drawing needs to make its texts selectable: hands out the texts' indices,
/// marks the selected stretches, records the layouts.
pub struct Collect {
    entry: EntryId,
    frame: u64,
    selected: Option<(Point, Point)>,
    color: Hsla,
    next: Cell<usize>,
    drawn: Drawn,
}

impl Collect {
    pub fn new(
        entry: EntryId,
        frame: u64,
        selection: Option<&Selection>,
        color: Hsla,
        drawn: Drawn,
    ) -> Self {
        let selected = selection
            .filter(|selection| selection.entry == entry && !selection.is_empty())
            .map(Selection::range);
        Self {
            entry,
            frame,
            selected,
            color,
            next: Cell::new(0),
            drawn,
        }
    }

    /// The next text of the message: its index, and `runs` (covering `text`) with the selected
    /// stretch on the selection's background.
    pub fn text(
        &self,
        text: &str,
        runs: Vec<TextRun>,
        joint: Joint,
        marker: Option<&str>,
    ) -> (usize, Vec<TextRun>) {
        let index = self.next.get();
        self.next.set(index + 1);
        {
            let mut drawn = self.drawn.borrow_mut();
            let texts = drawn.entry(self.entry).or_default();
            if texts.frame != self.frame {
                texts.frame = self.frame;
                texts.texts.clear();
            }
            let slot = Text {
                text: SharedString::from(text.to_string()),
                joint,
                marker: marker.map(|marker| SharedString::from(marker.to_string())),
                layout: None,
            };
            if index < texts.texts.len() {
                texts.texts[index] = slot;
            } else {
                texts.texts.push(slot);
            }
        }
        let runs = match self
            .selected
            .and_then(|(start, end)| range_in(start, end, index, text.len()))
        {
            Some(range) => paint(runs, range, self.color),
            None => runs,
        };
        (index, runs)
    }

    /// `element` (the text `index`, drawn with `layout`) leaving its layout for the mouse once it
    /// is laid out.
    pub fn wrap(&self, index: usize, layout: &TextLayout, element: AnyElement) -> AnyElement {
        Registered {
            element,
            layout: layout.clone(),
            entry: self.entry,
            frame: self.frame,
            index,
            drawn: self.drawn.clone(),
        }
        .into_any_element()
    }
}

/// `runs` with `range` on `color`'s background.
fn paint(runs: Vec<TextRun>, range: Range<usize>, color: Hsla) -> Vec<TextRun> {
    let mut out = Vec::with_capacity(runs.len() + 2);
    let mut start = 0;
    for run in runs {
        let end = start + run.len;
        let mut at = start;
        for (from, to, inside) in [
            (start, range.start.clamp(start, end), false),
            (
                range.start.clamp(start, end),
                range.end.clamp(start, end),
                true,
            ),
            (range.end.clamp(start, end), end, false),
        ] {
            if to > from {
                let mut piece = run.clone();
                piece.len = to - from;
                if inside {
                    piece.background_color = Some(color);
                }
                out.push(piece);
                at = to;
            }
        }
        debug_assert_eq!(at, end);
        start = end;
    }
    out
}

/// A text's element that records its layout in the store after it is laid out.
struct Registered {
    element: AnyElement,
    layout: TextLayout,
    entry: EntryId,
    frame: u64,
    index: usize,
    drawn: Drawn,
}

impl IntoElement for Registered {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Registered {
    type RequestLayoutState = ();
    type PrepaintState = ();

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
        (self.element.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.element.prepaint(window, cx);
        let mut drawn = self.drawn.borrow_mut();
        if let Some(text) = drawn
            .get_mut(&self.entry)
            .filter(|texts| texts.frame == self.frame)
            .and_then(|texts| texts.texts.get_mut(self.index))
        {
            text.layout = Some(self.layout.clone());
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.element.paint(window, cx);
    }
}

// --- The mouse and the keys ---

/// The mouse down on a message's text: a selection starts there (a double click takes the word, a
/// triple one the whole paragraph, ⇧ extends the selection); the chat takes the keyboard for ⌘C.
pub fn mouse_down(
    chat: WeakEntity<ClaudeChat>,
    entry: EntryId,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    move |event, window, cx| {
        let started = chat
            .update(cx, |chat, cx| {
                let focus = chat.focus_handle.clone();
                let state = chat.shown_transcript_mut();
                let point = point_at(&state.drawn, entry, event.position)?;
                let (anchor, head) = match event.click_count {
                    2 => word_at(&state.drawn, entry, point),
                    count if count >= 3 => text_at(&state.drawn, entry, point),
                    _ => match state
                        .selection
                        .filter(|old| event.modifiers.shift && old.entry == entry)
                    {
                        Some(old) => (old.anchor, point),
                        None => (point, point),
                    },
                };
                state.selection = Some(Selection {
                    entry,
                    anchor,
                    head,
                    dragging: event.click_count <= 1,
                });
                window.focus(&focus);
                cx.notify();
                Some(())
            })
            .ok()
            .flatten();
        // Not to the chat's own mouse down (it clears the selection) nor to the card under it.
        if started.is_some() {
            cx.stop_propagation();
        }
    }
}

/// An invisible element of the conversation that follows the mouse while a selection is dragged —
/// over other messages and out of the list too — and ends it when the button goes up.
pub fn drag_listener(chat: WeakEntity<ClaudeChat>) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            let moved = chat.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble || event.pressed_button != Some(MouseButton::Left)
                {
                    return;
                }
                moved
                    .update(cx, |chat, cx| {
                        let state = chat.shown_transcript_mut();
                        let Some(selection) = state.selection.filter(|s| s.dragging) else {
                            return;
                        };
                        let point = point_at(&state.drawn, selection.entry, event.position);
                        if let Some(point) = point.filter(|&point| point != selection.head) {
                            state.selection = Some(Selection {
                                head: point,
                                ..selection
                            });
                            cx.notify();
                        }
                    })
                    .ok();
            });
            let released = chat.clone();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                    return;
                }
                released
                    .update(cx, |chat, cx| {
                        let state = chat.shown_transcript_mut();
                        let Some(selection) = state.selection.filter(|s| s.dragging) else {
                            return;
                        };
                        // A click without a drag selects nothing.
                        state.selection = (!selection.is_empty()).then_some(Selection {
                            dragging: false,
                            ..selection
                        });
                        cx.notify();
                    })
                    .ok();
            });
        },
    )
    .absolute()
    .size_0()
    .into_any_element()
}

/// The chat's handlers: ⌘C and Esc for the selection, a click elsewhere clears it, typing while
/// the conversation has the keyboard goes on in the message field.
pub fn actions(root: gpui::Div, cx: &mut Context<ClaudeChat>) -> gpui::Div {
    root.on_action(cx.listener(|chat, _: &CopySelection, _, cx| {
        let state = chat.shown_transcript();
        let text = state
            .selection
            .as_ref()
            .and_then(|selection| selected_text(&state.drawn, selection));
        match text {
            Some(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            None => cx.propagate(),
        }
    }))
    .on_action(cx.listener(|chat, _: &ClearSelection, _, cx| {
        if chat.shown_transcript_mut().selection.take().is_some() {
            cx.notify();
        } else {
            cx.propagate();
        }
    }))
    .on_mouse_down(
        MouseButton::Left,
        cx.listener(|chat, _: &MouseDownEvent, _, cx| {
            if chat.shown_transcript_mut().selection.take().is_some() {
                cx.notify();
            }
        }),
    )
    .on_key_down(cx.listener(|chat, event: &KeyDownEvent, window, cx| {
        let keys = &event.keystroke;
        let typed = keys
            .key_char
            .as_ref()
            .filter(|text| !text.is_empty() && !text.chars().any(char::is_control));
        let Some(typed) = typed.cloned() else {
            return;
        };
        if keys.modifiers.platform
            || keys.modifiers.control
            || keys.modifiers.function
            || !chat.agents.is_empty()
            || !chat.focus_handle.is_focused(window)
        {
            return;
        }
        chat.shown_transcript_mut().selection = None;
        chat.composer()
            .clone()
            .update(cx, |composer, cx| composer.type_text(&typed, window, cx));
        cx.stop_propagation();
        cx.notify();
    }))
}

/// The point of the message `entry` nearest to `position`: in the text under it, else in the
/// nearest text (above the first — its start, below the last — its end). `None` while the
/// message isn't drawn.
pub fn point_at(drawn: &Drawn, entry: EntryId, position: gpui::Point<Pixels>) -> Option<Point> {
    let drawn = drawn.borrow();
    let texts = drawn.get(&entry)?;
    let laid: Vec<(usize, &TextLayout, Bounds<Pixels>)> = texts
        .texts
        .iter()
        .enumerate()
        .filter_map(|(index, text)| {
            let layout = text.layout.as_ref()?;
            Some((index, layout, layout.bounds()))
        })
        .collect();
    // The text whose rows hold the position, the nearest by its distance across; else the
    // nearest row of texts above or below.
    let distance_x = |bounds: &Bounds<Pixels>| {
        if position.x < bounds.left() {
            bounds.left() - position.x
        } else if position.x > bounds.right() {
            position.x - bounds.right()
        } else {
            Pixels::ZERO
        }
    };
    let distance_y = |bounds: &Bounds<Pixels>| {
        if position.y < bounds.top() {
            bounds.top() - position.y
        } else if position.y > bounds.bottom() {
            position.y - bounds.bottom()
        } else {
            Pixels::ZERO
        }
    };
    let (index, layout, bounds) = laid
        .iter()
        .min_by(|a, b| {
            (distance_y(&a.2), distance_x(&a.2))
                .partial_cmp(&(distance_y(&b.2), distance_x(&b.2)))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .copied()?;
    let len = texts.texts[index].text.len();
    // Beside a text in its rows: the nearest place of that row.
    let byte = if position.y >= bounds.top() && position.y <= bounds.bottom() {
        boundary_at(layout, &texts.texts[index].text, position)
    } else if position.y < bounds.top() {
        0
    } else {
        len
    };
    Some(Point {
        text: index,
        byte: byte.min(len),
    })
}

/// The character boundary of `text` (drawn with `layout`) nearest to `position`: past the middle
/// of a character, after it (gpui gives the character under the position).
fn boundary_at(layout: &TextLayout, text: &str, position: gpui::Point<Pixels>) -> usize {
    let byte = match layout.index_for_position(position) {
        Ok(byte) => byte,
        Err(byte) => return byte.min(text.len()),
    };
    let byte = byte.min(text.len());
    let Some(c) = text[byte..].chars().next().filter(|&c| c != '\n') else {
        return byte;
    };
    let next = byte + c.len_utf8();
    match (
        layout.position_for_index(byte),
        layout.position_for_index(next),
    ) {
        // The next character on the same row: compare with the middle.
        (Some(left), Some(right)) if right.y == left.y && right.x > left.x => {
            if position.x > left.x + (right.x - left.x) / 2. {
                next
            } else {
                byte
            }
        }
        _ => byte,
    }
}

/// The word around `point` (letters, digits, `_`), or the character there.
pub fn word_at(drawn: &Drawn, entry: EntryId, point: Point) -> (Point, Point) {
    let drawn = drawn.borrow();
    let Some(text) = drawn
        .get(&entry)
        .and_then(|texts| texts.texts.get(point.text))
    else {
        return (point, point);
    };
    let range = word_range(&text.text, point.byte);
    (
        Point {
            text: point.text,
            byte: range.start,
        },
        Point {
            text: point.text,
            byte: range.end,
        },
    )
}

/// The whole text `point` is in (a triple click).
pub fn text_at(drawn: &Drawn, entry: EntryId, point: Point) -> (Point, Point) {
    let len = drawn
        .borrow()
        .get(&entry)
        .and_then(|texts| texts.texts.get(point.text))
        .map_or(point.byte, |text| text.text.len());
    (
        Point {
            text: point.text,
            byte: 0,
        },
        Point {
            text: point.text,
            byte: len,
        },
    )
}

/// The byte range of the word at `byte` of `text`; on a space or a sign — that character.
pub fn word_range(text: &str, byte: usize) -> Range<usize> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let byte = byte.min(text.len());
    let at = text[byte..].chars().next();
    let before = text[..byte].chars().next_back();
    let word = match (at, before) {
        (Some(c), _) if is_word(c) => true,
        (_, Some(c)) if is_word(c) => true,
        _ => false,
    };
    if !word {
        return match at {
            Some(c) => byte..byte + c.len_utf8(),
            None => byte..byte,
        };
    }
    let start = text[..byte]
        .char_indices()
        .rev()
        .take_while(|&(_, c)| is_word(c))
        .last()
        .map_or(byte, |(index, _)| index);
    let end = text[byte..]
        .char_indices()
        .find(|&(_, c)| !is_word(c))
        .map_or(text.len(), |(index, _)| byte + index);
    start..end
}

/// The selected text of the message, as it would be copied: texts joined by their joints, list
/// items with their markers when taken from their start.
pub fn selected_text(drawn: &Drawn, selection: &Selection) -> Option<String> {
    if selection.is_empty() {
        return None;
    }
    let drawn = drawn.borrow();
    let texts = drawn.get(&selection.entry)?;
    let (start, end) = selection.range();
    let parts: Vec<Part> = texts
        .texts
        .iter()
        .enumerate()
        .map(|(index, text)| Part {
            text: &text.text,
            joint: text.joint,
            marker: text.marker.as_ref().map(|marker| marker.as_ref()),
            range: range_in(start, end, index, text.text.len()),
        })
        .collect();
    let copied = join(&parts, start.text != end.text);
    (!copied.is_empty()).then_some(copied)
}

/// A text's part in a copy.
struct Part<'a> {
    text: &'a str,
    joint: Joint,
    marker: Option<&'a str>,
    range: Option<Range<usize>>,
}

/// The selected stretches of `parts` joined; `several` — the selection spans texts, so list
/// items taken whole keep their markers.
fn join(parts: &[Part], several: bool) -> String {
    let mut out = String::new();
    let mut first = true;
    for part in parts {
        let Some(range) = part.range.clone() else {
            continue;
        };
        if !first {
            out.push_str(match part.joint {
                Joint::Block => "\n\n",
                Joint::Line => "\n",
                Joint::Cell => "\t",
            });
        }
        first = false;
        if let Some(marker) = part.marker.filter(|_| several && range.start == 0) {
            out.push_str(marker);
            out.push(' ');
        }
        out.push_str(&part.text[range]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(text: usize, byte: usize) -> Point {
        Point { text, byte }
    }

    #[test]
    fn a_selection_covers_the_texts_between_its_ends() {
        let (start, end) = (point(1, 3), point(3, 2));
        assert_eq!(range_in(start, end, 0, 10), None);
        assert_eq!(range_in(start, end, 1, 10), Some(3..10));
        assert_eq!(range_in(start, end, 2, 5), Some(0..5));
        assert_eq!(range_in(start, end, 3, 10), Some(0..2));
        assert_eq!(range_in(start, end, 4, 10), None);
        assert_eq!(range_in(point(0, 2), point(0, 6), 0, 4), Some(2..4));
        assert_eq!(range_in(point(0, 2), point(0, 2), 0, 4), None);
    }

    #[test]
    fn a_selection_backwards_has_the_same_range() {
        let selection = Selection {
            entry: 1,
            anchor: point(2, 1),
            head: point(0, 4),
            dragging: false,
        };
        assert_eq!(selection.range(), (point(0, 4), point(2, 1)));
    }

    #[test]
    fn words_are_letters_digits_and_underscores() {
        let text = "let foo_bar = 42; привет!";
        assert_eq!(&text[word_range(text, 5)], "foo_bar");
        assert_eq!(&text[word_range(text, 4)], "foo_bar");
        assert_eq!(&text[word_range(text, 11)], "foo_bar");
        assert_eq!(&text[word_range(text, 12)], "=");
        assert_eq!(&text[word_range(text, 14)], "42");
        let cyrillic = text.find('п').unwrap() + 2;
        assert_eq!(&text[word_range(text, cyrillic)], "привет");
        assert_eq!(word_range(text, text.len()), text.len()..text.len());
    }

    #[test]
    fn runs_split_around_the_selection() {
        let run = |len| TextRun {
            len,
            font: gpui::font("Inter"),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let color = gpui::red();
        let painted = paint(vec![run(4), run(6)], 2..7, color);
        let shape: Vec<_> = painted
            .iter()
            .map(|run| (run.len, run.background_color.is_some()))
            .collect();
        assert_eq!(
            shape,
            [(2, false), (2, true), (3, true), (3, false)].to_vec()
        );
    }

    #[test]
    fn copies_join_blocks_lines_and_cells() {
        let part = |text, joint, marker, range| Part {
            text,
            joint,
            marker,
            range,
        };
        let parts = [
            part("Intro text", Joint::Block, None, Some(6..10)),
            part("first", Joint::Block, Some("•"), Some(0..5)),
            part("second", Joint::Line, Some("•"), Some(0..6)),
            part("a", Joint::Block, None, Some(0..1)),
            part("b", Joint::Cell, None, Some(0..1)),
            part("c", Joint::Line, None, Some(0..1)),
            part("tail", Joint::Block, None, None),
        ];
        assert_eq!(join(&parts, true), "text\n\n• first\n• second\n\na\tb\nc");
        // Within one item: no marker.
        let one = [part("second", Joint::Line, Some("•"), Some(0..3))];
        assert_eq!(join(&one, false), "sec");
    }
}
