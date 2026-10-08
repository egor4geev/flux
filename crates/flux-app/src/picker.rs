//! A selection list with a query field: the foundation of the command palette and file search.
//!
//! [`Picker`] owns the field ([`TextInput`]), the selected row, scrolling, and the keys; what to
//! search for and how to draw a row is up to the [`PickerDelegate`]. Shown as an overlay window:
//! `Workspace::toggle_modal(window, cx, |window, cx| Picker::new(..))`.

use std::ops::Range;

use gpui::{
    AnyElement, App, ClickEvent, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, HighlightStyle, Hsla, KeyBinding, Render, ScrollStrategy, SharedString, StyledText,
    Subscription, UniformListScrollHandle, Window, actions, div, prelude::*, px, uniform_list,
};

use crate::i18n::tr;
use crate::icons::{IconName, icon};
use crate::input::{InputEvent, TextInput};
use crate::theme::{self, Theme};
use crate::ui::{self, RADIUS_MD};

/// Height of a list row.
const ROW_HEIGHT: f32 = 34.;
/// How many rows are visible without scrolling.
const MAX_VISIBLE_ROWS: usize = 10;
const PICKER_WIDTH: f32 = 640.;
/// Header with the query and footer with hints.
const HEADER_HEIGHT: f32 = 50.;
const FOOTER_HEIGHT: f32 = 36.;
/// Inset of the rows from the panel edges, and of the list from the header and footer.
const LIST_INSET: f32 = 6.;

actions!(
    picker,
    [
        SelectNext,
        SelectPrevious,
        SelectNextPage,
        SelectPreviousPage,
        Confirm,
        Dismiss,
    ]
);

pub fn init(cx: &mut App) {
    let context = Some("Picker");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("ctrl-n", SelectNext, context),
        KeyBinding::new("ctrl-p", SelectPrevious, context),
        KeyBinding::new("pagedown", SelectNextPage, context),
        KeyBinding::new("pageup", SelectPreviousPage, context),
        KeyBinding::new("enter", Confirm, context),
        KeyBinding::new("escape", Dismiss, context),
    ]);
}

/// What the selection list displays. The delegate knows nothing about keys, selection, or
/// scrolling.
pub trait PickerDelegate: Sized + 'static {
    /// Placeholder text in the empty query field.
    fn placeholder(&self) -> SharedString;

    fn match_count(&self) -> usize;

    /// The query changed (and once at creation, with an empty query): recompute the matches. May be
    /// asynchronous: when ready, redraw the Picker (`cx.notify()` on it); the selected row stays
    /// within `match_count` on its own.
    fn update_matches(&mut self, query: &str, window: &mut Window, cx: &mut Context<Picker<Self>>);

    /// Enter or a click on row `index` (< `match_count`). The delegate itself closes the window
    /// with `cx.emit(DismissEvent)`, when and if needed: the palette first has to restore focus and
    /// dispatch the action.
    fn confirm(&mut self, index: usize, window: &mut Window, cx: &mut Context<Picker<Self>>);

    /// Content of row `index`; the Picker draws the background of the selected row and of the row
    /// under the mouse. Called only for visible rows, so anything needed for rendering can be
    /// computed lazily here (for example, the positions of matched characters).
    fn render_match(
        &mut self,
        index: usize,
        selected: bool,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement;

    /// Left part of the footer: for example, "1234 files · indexing…".
    fn render_footer(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<AnyElement> {
        None
    }

    /// Text shown instead of an empty list.
    fn empty_message(&self) -> SharedString {
        tr("No matches").into()
    }

    /// What Enter does: the label in the footer hint ("open", "run").
    fn confirm_label(&self) -> &'static str {
        tr("open")
    }
}

pub struct Picker<D: PickerDelegate> {
    pub delegate: D,
    query: Entity<TextInput>,
    selected: usize,
    scroll: UniformListScrollHandle,
    _subscription: Subscription,
}

impl<D: PickerDelegate> EventEmitter<DismissEvent> for Picker<D> {}

impl<D: PickerDelegate> Picker<D> {
    pub fn new(mut delegate: D, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            TextInput::new(delegate.placeholder(), cx)
                .borderless()
                .large()
        });
        let subscription = cx.subscribe_in(&query, window, Self::on_query_event);
        delegate.update_matches("", window, cx);
        Self {
            delegate,
            query,
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            _subscription: subscription,
        }
    }

    fn on_query_event(
        &mut self,
        query: &Entity<TextInput>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Changed => {
                let text = query.read(cx).text();
                self.delegate.update_matches(&text, window, cx);
                self.selected = 0;
                self.scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            }
        }
    }

    fn clamp_selection(&mut self) {
        let count = self.delegate.match_count();
        self.selected = self.selected.min(count.saturating_sub(1));
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        let count = self.delegate.match_count();
        if count == 0 {
            return;
        }
        let index = index.min(count - 1);
        // Scroll the minimum amount: moving up, the row snaps to the top edge; moving down, to the
        // bottom edge.
        let strategy = if index < self.selected {
            ScrollStrategy::Top
        } else {
            ScrollStrategy::Bottom
        };
        self.selected = index;
        self.scroll.scroll_to_item(index, strategy);
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.select(self.selected + 1, cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.select(self.selected.saturating_sub(1), cx);
    }

    fn select_next_page(&mut self, _: &SelectNextPage, _: &mut Window, cx: &mut Context<Self>) {
        self.select(self.selected + MAX_VISIBLE_ROWS - 1, cx);
    }

    fn select_previous_page(
        &mut self,
        _: &SelectPreviousPage,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select(self.selected.saturating_sub(MAX_VISIBLE_ROWS - 1), cx);
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected < self.delegate.match_count() {
            self.delegate.confirm(self.selected, window, cx);
        }
    }

    fn render_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let selected = index == self.selected;
        // The outer block spans the full width of the list (uniform_list measures the row height
        // from it); the inner one is a rounded background inset from the panel edges.
        div()
            .w_full()
            .h(px(ROW_HEIGHT))
            .px(px(LIST_INSET))
            .child(
                div()
                    .id(index)
                    .size_full()
                    .px_2p5()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .rounded(px(RADIUS_MD))
                    .cursor_pointer()
                    .when(selected, |row| row.bg(ui.list_selected))
                    .when(!selected, |row| row.hover(|style| style.bg(ui.hover)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.selected = index;
                        this.confirm(&Confirm, window, cx);
                    }))
                    .child(self.delegate.render_match(index, selected, window, cx)),
            )
            .into_any_element()
    }
}

impl<D: PickerDelegate> Focusable for Picker<D> {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl<D: PickerDelegate> Render for Picker<D> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        self.clamp_selection();
        let count = self.delegate.match_count();
        let list = if count == 0 {
            div()
                .h(px(ROW_HEIGHT * 2.))
                .flex()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(ui.dim)
                .child(icon(IconName::Search, ui.dim).size(px(14.)))
                .child(self.delegate.empty_message())
                .into_any_element()
        } else {
            let height = ROW_HEIGHT * count.min(MAX_VISIBLE_ROWS) as f32;
            uniform_list(
                "picker-matches",
                count,
                cx.processor(|this, range: Range<usize>, window, cx| {
                    range
                        .map(|index| this.render_row(index, window, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(self.scroll.clone())
            .h(px(height))
            .into_any_element()
        };
        let footer = self.delegate.render_footer(window, cx);
        ui::popover(ui)
            .key_context("Picker")
            .w(px(PICKER_WIDTH))
            .flex()
            .flex_col()
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_next_page))
            .on_action(cx.listener(Self::select_previous_page))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .child(
                div()
                    .flex_none()
                    .h(px(HEADER_HEIGHT))
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(icon(IconName::Search, ui.text_muted))
                    .child(div().flex_1().min_w_0().child(self.query.clone())),
            )
            .child(ui::divider(ui))
            .child(div().py(px(LIST_INSET)).child(list))
            .child(ui::divider(ui))
            .child(
                div()
                    .flex_none()
                    .h(px(FOOTER_HEIGHT))
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(div().min_w_0().truncate().children(footer))
                    .child(ui::hint_bar(
                        &[
                            ("↑↓", tr("navigate")),
                            ("↵", self.delegate.confirm_label()),
                            ("esc", tr("close")),
                        ],
                        ui,
                    )),
            )
    }
}

/// Text with highlighted characters: `positions` are `char` indices (as in
/// `flux_search::FuzzyMatch`), highlighted with `color`.
pub fn highlighted_text(
    text: impl Into<SharedString>,
    positions: &[usize],
    color: Hsla,
) -> StyledText {
    let text = text.into();
    let style = HighlightStyle {
        color: Some(color),
        ..Default::default()
    };
    let ranges = byte_ranges(&text, positions);
    StyledText::new(text).with_highlights(ranges.into_iter().map(|range| (range, style)))
}

/// Converts character indices to byte ranges, with adjacent characters merged into a single range.
/// Indices past the end of the string are discarded.
pub fn byte_ranges(text: &str, positions: &[usize]) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut positions = positions.iter().copied().peekable();
    for (index, (byte, c)) in text.char_indices().enumerate() {
        while positions.next_if(|&p| p < index).is_some() {}
        if positions.next_if_eq(&index).is_none() {
            continue;
        }
        let end = byte + c.len_utf8();
        match ranges.last_mut() {
            Some(last) if last.end == byte => last.end = end,
            _ => ranges.push(byte..end),
        }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_become_merged_byte_ranges() {
        assert_eq!(byte_ranges("move left", &[0, 1, 5]), vec![0..2, 5..6]);
        assert_eq!(byte_ranges("abc", &[]), Vec::<Range<usize>>::new());
        // "п" and "ф" are 2 bytes each.
        assert_eq!(byte_ranges("путь к файлу", &[0, 7]), vec![0..2, 12..14]);
        // Duplicates and indices past the end of the string are tolerated.
        assert_eq!(byte_ranges("ab", &[1, 1, 9]), vec![1..2]);
    }
}
