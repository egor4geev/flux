//! A selection list with a query field: the foundation of the command palette and file search.
//!
//! [`Picker`] owns the field ([`TextInput`]), the selected row, scrolling, and the keys; what to
//! search for and how to draw a row is up to the [`PickerDelegate`]. Shown as an overlay window:
//! `Workspace::toggle_modal(window, cx, |window, cx| Picker::new(..))`.
//!
//! A delegate may also (part 10.3, the plugins' popups): have rows that can't be selected (a
//! group's heading), drop the query field (a short menu: ↑↓ ↵ Esc), say it is still looking (a
//! pulsing dot), set its width and row height, hide the footer, wait unseen for its place.
//!
//! Motion (`crate::motion`): the list's height glides when its items change on their own — a
//! project still being walked, a plugin's new items, loading → loaded; what the typing changes
//! comes at once.

use std::ops::Range;
use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, ClickEvent, Context, DismissEvent, Entity,
    EventEmitter, FocusHandle, Focusable, HighlightStyle, Hsla, KeyBinding, Render, ScrollStrategy,
    SharedString, StyledText, Subscription, UniformListScrollHandle, Window, actions, div,
    prelude::*, px, uniform_list,
};

use crate::i18n::tr;
use crate::icons::{IconName, icon};
use crate::input::{InputEvent, TextInput};
use crate::motion::{self, TransitionExt as _};
use crate::theme::{self, Theme};
use crate::ui::{self, RADIUS_MD};

/// Height of a list row.
pub const ROW_HEIGHT: f32 = 34.;
/// How many rows are visible without scrolling.
const MAX_VISIBLE_ROWS: usize = 10;
pub const PICKER_WIDTH: f32 = 640.;
/// Header with the query and footer with hints.
const HEADER_HEIGHT: f32 = 50.;
const FOOTER_HEIGHT: f32 = 36.;
/// Inset of the rows from the panel edges, and of the list from the header and footer.
const LIST_INSET: f32 = 6.;
/// The height of the message shown instead of an empty list.
const EMPTY_HEIGHT: f32 = ROW_HEIGHT * 2.;
/// A new height of the list this soon after the query changed is still the typing's (a search
/// finishing in steps): it comes at once.
const TYPING_SETTLE: Duration = Duration::from_millis(300);

actions!(
    picker,
    [
        SelectNext,
        SelectPrevious,
        SelectNextPage,
        SelectPreviousPage,
        Confirm,
        Dismiss,
        /// The selected row removed, if the list can do it ([`PickerDelegate::delete`]): ⌘⌫ in the
        /// history of Claude's sessions (part 10.2); lists bind it in their own key context.
        DeleteSelected,
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

    /// The key context of the list: "Picker" plus the list's own name, for keys of its own.
    fn key_context(&self) -> &'static str {
        "Picker"
    }

    /// [`DeleteSelected`] on row `index`; lists that can't remove rows ignore it.
    fn delete(&mut self, _index: usize, _window: &mut Window, _cx: &mut Context<Picker<Self>>) {}

    /// What ⌘⌫ does, for the footer hint; `None` — the list doesn't remove rows.
    fn delete_label(&self) -> Option<&'static str> {
        None
    }

    /// Row `index` can be selected and chosen: a group's heading can't (the keys and the mouse
    /// pass it by).
    fn selectable(&self, _index: usize) -> bool {
        true
    }

    /// The query field is shown; without it the list is a short menu (↑↓ ↵ Esc).
    fn searchable(&self) -> bool {
        true
    }

    /// The matches are still coming: a pulsing dot in the header (or in the empty list).
    fn loading(&self) -> bool {
        false
    }

    fn width(&self) -> f32 {
        PICKER_WIDTH
    }

    fn row_height(&self) -> f32 {
        ROW_HEIGHT
    }

    /// The footer with the key hints is shown.
    fn show_footer(&self) -> bool {
        true
    }

    /// The list has its place. One that waits for it (a plugin's picker above a status bar item
    /// not drawn yet) is laid out but not seen and takes no mouse; its query already takes the
    /// keys.
    fn placed(&self) -> bool {
        true
    }
}

/// How the list's height changes ([`Picker::height_key`]).
#[derive(Default)]
struct ListHeight {
    /// The height the list was last given.
    target: Option<f32>,
    /// The query it was given for.
    query: String,
    /// When the query last changed.
    typed: Option<Instant>,
    /// The key of the height's glide: a new one starts it over at its target — a jump.
    key: usize,
}

pub struct Picker<D: PickerDelegate> {
    pub delegate: D,
    query: Entity<TextInput>,
    /// The keys' focus of a list without the query field.
    focus: FocusHandle,
    selected: usize,
    scroll: UniformListScrollHandle,
    height: ListHeight,
    _subscription: Subscription,
}

impl<D: PickerDelegate> EventEmitter<DismissEvent> for Picker<D> {}

impl<D: PickerDelegate> Picker<D> {
    /// Selects row `index` (clamped) and scrolls to it.
    pub fn set_selected(&mut self, index: usize, cx: &mut Context<Self>) {
        let count = self.delegate.match_count();
        if count == 0 {
            return;
        }
        self.selected = index.min(count - 1);
        self.clamp_selection();
        self.scroll
            .scroll_to_item(self.selected, gpui::ScrollStrategy::Center);
        cx.notify();
    }

    /// The first row that can be selected, scrolled to the top: the delegate's matches changed
    /// on their own (a plugin's new items).
    pub fn select_first(&mut self, cx: &mut Context<Self>) {
        self.selected = self.next_selectable(0).unwrap_or(0);
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// The query typed so far.
    pub fn query(&self, cx: &App) -> String {
        self.query.read(cx).text()
    }

    /// The first row from `index` on that can be selected.
    fn next_selectable(&self, index: usize) -> Option<usize> {
        (index..self.delegate.match_count()).find(|&row| self.delegate.selectable(row))
    }

    /// The last row up to `index` that can be selected.
    fn previous_selectable(&self, index: usize) -> Option<usize> {
        let count = self.delegate.match_count();
        if count == 0 {
            return None;
        }
        (0..=index.min(count - 1))
            .rev()
            .find(|&row| self.delegate.selectable(row))
    }

    fn delete_selected(&mut self, _: &DeleteSelected, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected < self.delegate.match_count() {
            self.delegate.delete(self.selected, window, cx);
        }
    }

    pub fn new(mut delegate: D, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            TextInput::new(delegate.placeholder(), cx)
                .borderless()
                .large()
        });
        let subscription = cx.subscribe_in(&query, window, Self::on_query_event);
        delegate.update_matches("", window, cx);
        let mut picker = Self {
            delegate,
            query,
            focus: cx.focus_handle(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            height: ListHeight::default(),
            _subscription: subscription,
        };
        picker.selected = picker.next_selectable(0).unwrap_or(0);
        picker
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
                self.height.typed = Some(Instant::now());
                let text = query.read(cx).text();
                self.delegate.update_matches(&text, window, cx);
                self.selected = self.next_selectable(0).unwrap_or(0);
                self.scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            }
        }
    }

    /// The key of the list's height glide for a list `target` high: the height glides to a new
    /// target when only the items changed — a project still being walked, a plugin's new items,
    /// loading → loaded; when the query changed since the height last did, or just now (a search
    /// finishing in steps), it jumps — typing stays instant.
    fn height_key(&mut self, target: f32, cx: &App) -> usize {
        let height = &mut self.height;
        if height.target == Some(target) {
            return height.key;
        }
        let query = self.query.read(cx).text();
        let typed = height.query != query
            || height
                .typed
                .is_some_and(|typed| typed.elapsed() < TYPING_SETTLE);
        if height.target.is_some() && typed {
            height.key += 1;
        }
        height.target = Some(target);
        height.query = query;
        height.key
    }

    fn clamp_selection(&mut self) {
        let count = self.delegate.match_count();
        self.selected = self.selected.min(count.saturating_sub(1));
        if count > 0 && !self.delegate.selectable(self.selected) {
            self.selected = self
                .next_selectable(self.selected)
                .or_else(|| self.previous_selectable(self.selected))
                .unwrap_or(self.selected);
        }
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        let count = self.delegate.match_count();
        if count == 0 {
            return;
        }
        let index = index.min(count - 1);
        // A heading is passed by in the direction of the move; past the last row there is none.
        let index = if index >= self.selected {
            self.next_selectable(index)
                .or_else(|| self.previous_selectable(index))
        } else {
            self.previous_selectable(index)
                .or_else(|| self.next_selectable(index))
        };
        let Some(index) = index else {
            return;
        };
        // Scroll the minimum amount: moving up, the row snaps to the top edge; moving down, to the
        // bottom edge.
        let strategy = if index < self.selected {
            ScrollStrategy::Top
        } else {
            ScrollStrategy::Bottom
        };
        // Going up to a group's first row shows its heading too.
        let shown = match strategy {
            ScrollStrategy::Top if index > 0 && !self.delegate.selectable(index - 1) => index - 1,
            _ => index,
        };
        self.selected = index;
        self.scroll.scroll_to_item(shown, strategy);
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
        if self.selected < self.delegate.match_count() && self.delegate.selectable(self.selected) {
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
        let selectable = self.delegate.selectable(index);
        let content = self.delegate.render_match(index, selected, window, cx);
        // The outer block spans the full width of the list (uniform_list measures the row height
        // from it); the inner one is a rounded background inset from the panel edges.
        let row = div()
            .id(index)
            .size_full()
            .px_2p5()
            .flex()
            .items_center()
            .overflow_hidden()
            .rounded(px(RADIUS_MD));
        let row = if selectable {
            row.cursor_pointer()
                .when(selected, |row| row.bg(ui.list_selected))
                .when(!selected, |row| row.hover(|style| style.bg(ui.hover)))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.selected = index;
                    this.confirm(&Confirm, window, cx);
                }))
        } else {
            row
        };
        div()
            .w_full()
            .h(px(self.delegate.row_height()))
            .px(px(LIST_INSET))
            .child(row.child(content))
            .into_any_element()
    }
}

impl<D: PickerDelegate> Focusable for Picker<D> {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.delegate.searchable() {
            self.query.focus_handle(cx)
        } else {
            self.focus.clone()
        }
    }
}

impl<D: PickerDelegate> Render for Picker<D> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        self.clamp_selection();
        let count = self.delegate.match_count();
        let searchable = self.delegate.searchable();
        let loading = self.delegate.loading();
        let row_height = self.delegate.row_height();
        let (list, height) = if count == 0 {
            let empty = div()
                .h(px(EMPTY_HEIGHT))
                .px_4()
                .flex()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(ui.dim)
                .when(loading, |empty| empty.child(pulse(ui.accent)))
                .when(!loading && searchable, |empty| {
                    empty.child(icon(IconName::Search, ui.dim).size(px(14.)))
                })
                .child(div().truncate().child(self.delegate.empty_message()))
                .into_any_element();
            (empty, EMPTY_HEIGHT)
        } else {
            let height = row_height * count.min(MAX_VISIBLE_ROWS) as f32;
            let list = uniform_list(
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
            .into_any_element();
            (list, height)
        };
        // The list keeps its own height (its scrolling counts on it); what shows of it glides.
        let key = self.height_key(height, cx);
        let list = div().overflow_hidden().child(list).with_transition(
            ("picker-list-height", key),
            height,
            motion::SMOOTH,
            |list, height| list.h(px(height)),
        );
        let footer = self
            .delegate
            .show_footer()
            .then(|| self.delegate.render_footer(window, cx))
            .flatten();
        let delete_label = self.delegate.delete_label();
        let header = searchable.then(|| {
            div()
                .flex_none()
                .h(px(HEADER_HEIGHT))
                .px_4()
                .flex()
                .items_center()
                .gap_3()
                .child(icon(IconName::Search, ui.text_muted))
                .child(div().flex_1().min_w_0().child(self.query.clone()))
                .when(loading, |header| header.child(pulse(ui.accent)))
        });
        let hints = self.delegate.show_footer().then(|| {
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
                    &[("↑↓", tr("navigate")), ("↵", self.delegate.confirm_label())]
                        .into_iter()
                        .chain(delete_label.map(|label| ("⌘⌫", label)))
                        .chain([("esc", tr("close"))])
                        .collect::<Vec<_>>(),
                    ui,
                ))
        });
        let popover = ui::popover(ui)
            .key_context(self.delegate.key_context())
            .when(!searchable, |popover| popover.track_focus(&self.focus))
            .w(px(self.delegate.width()))
            .flex()
            .flex_col()
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_next_page))
            .on_action(cx.listener(Self::select_previous_page))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .on_action(cx.listener(Self::delete_selected))
            .when_some(header, |popover, header| {
                popover.child(header).child(ui::divider(ui))
            })
            .child(div().py(px(LIST_INSET)).child(list))
            .when_some(hints, |popover, hints| {
                popover.child(ui::divider(ui)).child(hints)
            });
        if self.delegate.placed() {
            popover.into_any_element()
        } else {
            // Waiting for its place: not seen, no mouse — the query field already has the keys.
            motion::inert(popover.opacity(0.)).into_any_element()
        }
    }
}

/// A pulsing dot: the matches are still coming (as Find in Files' search in progress); still when
/// motion is off.
fn pulse(color: Hsla) -> AnyElement {
    let dot = div().flex_none().size(px(7.)).rounded(px(4.)).bg(color);
    if !motion::enabled() {
        return dot.into_any_element();
    }
    dot.with_animation(
        "picker-loading",
        Animation::new(Duration::from_millis(900)).repeat(),
        |dot, delta| dot.opacity(0.3 + 0.7 * (1. - (2. * delta - 1.).abs())),
    )
    .into_any_element()
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
