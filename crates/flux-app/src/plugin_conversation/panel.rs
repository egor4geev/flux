//! The field's panel (API 0.5, FluxGram part 4): a ☺ button by the field opens a panel over the
//! feed, by the field's right edge, with the tabs the composer names (`composer-spec.panel`):
//!
//! - Emoji — Flux's own ([`crate::emoji`]): the recent ones first, then by category; a search by
//!   the Russian and English names and keywords; a skin tone for all (six circles by the search,
//!   kept with the recent ones in their own file, [`crate::emoji::Recent`]). A click puts one into
//!   the field at the caret and gives the field the keyboard; the recent ones change the next time
//!   the panel opens, so nothing moves under the pointer.
//! - Stickers and GIFs — the plugin's: opening a tab (each time) comes as `panel-opened`, its
//!   content as `panel` changes ([`Panel::set_content`]; an answer to another search than the
//!   tab's current one is dropped) and `panel-put` ([`Panel::put`]); a spinner until the first
//!   comes. Stickers fit into squares, GIFs keep their proportions in rows of one height (each row
//!   filled edge to edge). The search goes out as `panel-searched` when the typing stops
//!   ([`SEARCH_DELAY`]); the items on screen as `panel-visible` (at most every
//!   [`super::VISIBLE_EVERY`]); near the end, while the tab's `more` is `more`, `panel-load-more`
//!   (once per state). A click sends one (`panel-chosen`), and the panel stays open. Animated
//!   stickers and GIFs play as in the feed, only those on screen, within the same limits
//!   ([`super::media::Media::render_animation`], [`super::media::Media::render_gif`]).
//!
//! Each tab's content is a `list` of rows (a section's heading, a row of items): only the rows on
//! screen are built. A strip of the sections at the bottom (the emoji's categories, the sticker
//! sets' pictures) scrolls to one and marks the one in sight.
//!
//! Esc, a click outside it or ☺ again closes it; the field keeps the keyboard (Esc in the panel's
//! search gives it back). Nothing here asks for frames by itself.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use flux_plugin::api::events::UiEvent;
use flux_plugin::api::ui::{History, PanelContent, PanelItem, PanelTab};
use gpui::{
    AnyElement, Bounds, ClickEvent, Context, Corner, ElementId, Entity, Focusable, ListAlignment,
    ListOffset, ListState, MouseDownEvent, Pixels, SharedString, Subscription, Task, Window,
    anchored, deferred, div, list, point, prelude::*, px,
};

use super::{CancelEdit, ClearSelection, Conversation, fit, render_picture};
use crate::emoji::{self, Emoji, Recent, Tone};
use crate::i18n::tr;
use crate::icons::{IconName, icon};
use crate::input::{InputEvent, TextInput};
use crate::motion;
use crate::theme::{self, Theme, UiColors};
use crate::ui;

/// The panel's size.
const WIDTH: f32 = 360.;
const HEIGHT: f32 = 420.;
/// Between the panel and the field.
const GAP: f32 = 6.;
const TABS_HEIGHT: f32 = 42.;
const SEARCH_HEIGHT: f32 = 34.;
const STRIP_HEIGHT: f32 = 40.;
/// The grid's inset from the panel's sides.
const INSET: f32 = 8.;
/// The grid's width: the panel's less its border and the insets.
const GRID: f32 = WIDTH - 2. - 2. * INSET;
const HEADING_HEIGHT: f32 = 30.;
/// Emoji: a square cell each, this many to a row.
const EMOJI_COLUMNS: usize = 9;
const EMOJI_CELL: f32 = GRID / EMOJI_COLUMNS as f32;
const EMOJI_SIZE: f32 = 24.;
/// Stickers: fit into a square of [`STICKER_SIZE`] in a cell, this many to a row.
const STICKER_COLUMNS: usize = 4;
const STICKER_CELL: f32 = GRID / STICKER_COLUMNS as f32;
const STICKER_ROW: f32 = 80.;
const STICKER_SIZE: f32 = 72.;
/// GIFs: rows about this tall, filled edge to edge.
const GIF_HEIGHT: f32 = 96.;
const GIF_GAP: f32 = 4.;
/// A GIF's proportions are kept within these.
const GIF_ASPECT: (f32, f32) = (0.5, 2.5);
/// A spinner's row, an empty tab's.
const LOADING_HEIGHT: f32 = 56.;
const EMPTY_HEIGHT: f32 = 120.;
/// `panel-searched` goes out this long after the typing stops.
const SEARCH_DELAY: Duration = Duration::from_millis(300);
/// `panel-load-more` goes out with this many rows or fewer below the screen.
const MORE_WITHIN: usize = 4;
/// The list lays out this much above and below what is seen.
const OVERDRAW: f32 = 200.;
const STRIP_BUTTON: f32 = 30.;

/// A row of a tab's list.
#[derive(Clone, Debug, PartialEq)]
enum Row {
    /// A section's heading (by the section's index).
    Heading(usize),
    /// A section's items `start..end`, in a row `height` tall.
    Items {
        section: usize,
        start: usize,
        end: usize,
        height: f32,
    },
    /// The plugin is loading (the first content, the next page): a spinner.
    Loading,
    /// Nothing in the tab, or nothing found.
    Empty,
}

/// A section of the emoji tab: the recent ones, a category, what the search found.
struct EmojiSection {
    /// In English, for `tr`; empty — no heading (the search's).
    title: &'static str,
    /// Its glyph in the strip; none — the recent ones' clock.
    icon: Option<&'static str>,
    items: Vec<&'static Emoji>,
}

/// The panel's state: open or not, the tab, the plugin's content by tab, the rows of the tab shown.
pub(super) struct Panel {
    open: bool,
    /// Openings so far: the panel appears anew each time (its motion's id).
    opened: u64,
    tab: PanelTab,
    /// The plugin's content of the stickers' and the GIFs' tabs; none — not come yet.
    stickers: Option<PanelContent>,
    gifs: Option<PanelContent>,
    /// The search field (made when the panel first opens) and each tab's search.
    search: Option<Entity<TextInput>>,
    queries: [String; 3],
    /// The emoji tab's sections, and what they are made of: the recent ones as the panel
    /// opened, the tone.
    emoji: Vec<EmojiSection>,
    recent: Vec<&'static Emoji>,
    tone: Tone,
    rows: Vec<Row>,
    list: ListState,
    /// The section in sight: the strip marks it.
    current: usize,
    search_timer: Option<Task<()>>,
    /// The last `panel-visible` sent, when, and the timer of the one waiting for its turn.
    visible_sent: Option<(String, String)>,
    visible_at: Option<Instant>,
    visible_timer: Option<Task<()>>,
    /// `panel-load-more` went out in this state of the tab's content (by tab).
    asked_more: [bool; 3],
    /// Where the ☺ button was drawn: a press on it isn't one outside the panel.
    button: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The tabs' gliding pill.
    segments: Rc<ui::Segments>,
    _search_events: Option<Subscription>,
}

/// A tab's place in the per-tab arrays.
fn slot(tab: PanelTab) -> usize {
    match tab {
        PanelTab::Emoji => 0,
        PanelTab::Stickers => 1,
        PanelTab::Gifs => 2,
    }
}

fn tab_title(tab: PanelTab) -> &'static str {
    match tab {
        PanelTab::Emoji => tr("Emoji"),
        PanelTab::Stickers => tr("Stickers"),
        PanelTab::Gifs => tr("GIF"),
    }
}

fn search_placeholder(tab: PanelTab) -> &'static str {
    match tab {
        PanelTab::Emoji => tr("Search Emoji"),
        PanelTab::Stickers => tr("Search Stickers"),
        PanelTab::Gifs => tr("Search GIFs"),
    }
}

/// The tooltips of the skin tones' circles.
const TONE_NAMES: [&str; emoji::TONES as usize] = [
    "No Skin Tone",
    "Light Skin Tone",
    "Medium-Light Skin Tone",
    "Medium Skin Tone",
    "Medium-Dark Skin Tone",
    "Dark Skin Tone",
];

impl Panel {
    pub(super) fn new() -> Self {
        Self {
            open: false,
            opened: 0,
            tab: PanelTab::Emoji,
            stickers: None,
            gifs: None,
            search: None,
            queries: Default::default(),
            emoji: Vec::new(),
            recent: Vec::new(),
            tone: 0,
            rows: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(OVERDRAW)),
            current: 0,
            search_timer: None,
            visible_sent: None,
            visible_at: None,
            visible_timer: None,
            asked_more: [false; 3],
            button: Rc::default(),
            segments: Rc::default(),
            _search_events: None,
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    /// What a tab shows (`panel` change): an answer to a search other than the tab's current
    /// one is dropped.
    pub(super) fn set_content(&mut self, content: PanelContent) {
        let tab = content.tab;
        if tab == PanelTab::Emoji || content.query.trim() != self.queries[slot(tab)].trim() {
            return;
        }
        // A new search's answer starts at the top; the same search's (a page more, a fresher
        // one) keeps the place.
        let new_search = self
            .content(tab)
            .is_none_or(|before| before.query != content.query);
        self.asked_more[slot(tab)] = false;
        *self.content_mut(tab) = Some(content);
        if self.open && self.tab == tab {
            self.rebuild(new_search);
        }
    }

    /// Items replaced by their keys (`panel-put`), wherever they are in the tab.
    pub(super) fn put(&mut self, tab: PanelTab, items: Vec<PanelItem>) {
        if tab == PanelTab::Emoji {
            return;
        }
        let Some(content) = self.content_mut(tab).as_mut() else {
            return;
        };
        for item in items {
            for section in &mut content.sections {
                for old in &mut section.items {
                    if old.key == item.key {
                        *old = item.clone();
                    }
                }
            }
        }
        // A GIF's size may have come with it: its rows follow.
        if self.open && self.tab == tab {
            self.rebuild(false);
        }
    }

    fn content(&self, tab: PanelTab) -> Option<&PanelContent> {
        match tab {
            PanelTab::Emoji => None,
            PanelTab::Stickers => self.stickers.as_ref(),
            PanelTab::Gifs => self.gifs.as_ref(),
        }
    }

    /// A plugin tab's content (the emoji tab has none: its own is the GIFs' slot, never asked).
    fn content_mut(&mut self, tab: PanelTab) -> &mut Option<PanelContent> {
        match tab {
            PanelTab::Stickers => &mut self.stickers,
            PanelTab::Emoji | PanelTab::Gifs => &mut self.gifs,
        }
    }

    /// The emoji tab's sections: the recent ones and the categories, or what the search found.
    fn emoji_sections(&self) -> Vec<EmojiSection> {
        let query = self.queries[slot(PanelTab::Emoji)].trim();
        if !query.is_empty() {
            return vec![EmojiSection {
                title: "",
                icon: None,
                items: emoji::search(query),
            }];
        }
        let recent = (!self.recent.is_empty()).then(|| EmojiSection {
            title: "Recent",
            icon: None,
            items: self.recent.clone(),
        });
        recent
            .into_iter()
            .chain(emoji::categories().iter().map(|category| EmojiSection {
                title: category.title,
                icon: Some(category.icon),
                items: category.emoji().iter().collect(),
            }))
            .collect()
    }

    /// The rows of the tab shown, again; `top` — the list starts over at the top, otherwise the
    /// rows that stayed keep their place.
    fn rebuild(&mut self, top: bool) {
        let rows = match self.tab {
            PanelTab::Emoji => {
                self.emoji = self.emoji_sections();
                emoji_rows(&self.emoji)
            }
            tab => match self.content(tab) {
                None => vec![Row::Loading],
                Some(content) => plugin_rows(content, tab),
            },
        };
        let old = std::mem::replace(&mut self.rows, rows);
        if top {
            self.list.reset(self.rows.len());
            self.current = 0;
            self.visible_sent = None;
            return;
        }
        let same = old
            .iter()
            .zip(&self.rows)
            .take_while(|(old, new)| old == new)
            .count();
        if same < old.len() || same < self.rows.len() {
            self.list.splice(same..old.len(), self.rows.len() - same);
        }
    }

    /// The section a row belongs to.
    fn section_of(&self, row: usize) -> usize {
        self.rows[..=row.min(self.rows.len().saturating_sub(1))]
            .iter()
            .rev()
            .find_map(|row| match row {
                Row::Heading(section) | Row::Items { section, .. } => Some(*section),
                _ => None,
            })
            .unwrap_or(0)
    }

    /// The row of a section's heading (or its first row).
    fn row_of(&self, section: usize) -> Option<usize> {
        self.rows.iter().position(|row| match row {
            Row::Heading(at) | Row::Items { section: at, .. } => *at == section,
            _ => false,
        })
    }
}

/// The emoji tab's rows: each section's heading and its emoji, [`EMOJI_COLUMNS`] to a row.
fn emoji_rows(sections: &[EmojiSection]) -> Vec<Row> {
    let mut rows = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        if !section.title.is_empty() {
            rows.push(Row::Heading(index));
        }
        for start in (0..section.items.len()).step_by(EMOJI_COLUMNS) {
            rows.push(Row::Items {
                section: index,
                start,
                end: (start + EMOJI_COLUMNS).min(section.items.len()),
                height: EMOJI_CELL,
            });
        }
    }
    if rows.is_empty() {
        rows.push(Row::Empty);
    }
    rows
}

/// A plugin tab's rows: each section's heading (if it has a title) and its items — stickers
/// [`STICKER_COLUMNS`] to a row, GIFs in rows of their proportions; a spinner while more loads.
fn plugin_rows(content: &PanelContent, tab: PanelTab) -> Vec<Row> {
    let mut rows = Vec::new();
    for (index, section) in content.sections.iter().enumerate() {
        if section.items.is_empty() {
            continue;
        }
        if !section.title.is_empty() {
            rows.push(Row::Heading(index));
        }
        match tab {
            PanelTab::Gifs => {
                let aspects: Vec<f32> = section.items.iter().map(gif_aspect).collect();
                for (start, end, height) in justify(&aspects, GRID, GIF_HEIGHT, GIF_GAP) {
                    rows.push(Row::Items {
                        section: index,
                        start,
                        end,
                        height,
                    });
                }
            }
            _ => {
                for start in (0..section.items.len()).step_by(STICKER_COLUMNS) {
                    rows.push(Row::Items {
                        section: index,
                        start,
                        end: (start + STICKER_COLUMNS).min(section.items.len()),
                        height: STICKER_ROW,
                    });
                }
            }
        }
    }
    if rows.is_empty() && content.more != History::Loading {
        rows.push(Row::Empty);
    }
    if content.more == History::Loading {
        rows.push(Row::Loading);
    }
    rows
}

/// A GIF's width to its height, from its poster's size (unknown — a square), within
/// [`GIF_ASPECT`].
fn gif_aspect(item: &PanelItem) -> f32 {
    let (width, height) = (item.image.width, item.image.height);
    if width == 0 || height == 0 {
        return 1.;
    }
    (width as f32 / height as f32).clamp(GIF_ASPECT.0, GIF_ASPECT.1)
}

/// Pictures of these proportions in rows `width` wide: each row takes pictures until they are as
/// wide as the row at `height`, then is scaled to fill it exactly (so a little lower); the last
/// row, not full, keeps `height`. Rows as (start, end, height), whole pixels tall.
fn justify(aspects: &[f32], width: f32, height: f32, gap: f32) -> Vec<(usize, usize, f32)> {
    let mut rows = Vec::new();
    let (mut start, mut sum) = (0, 0.);
    for (index, aspect) in aspects.iter().enumerate() {
        sum += aspect;
        let gaps = gap * (index - start) as f32;
        if sum * height + gaps >= width {
            rows.push((start, index + 1, ((width - gaps) / sum).floor().max(1.)));
            start = index + 1;
            sum = 0.;
        }
    }
    if start < aspects.len() {
        rows.push((start, aspects.len(), height));
    }
    rows
}

impl Conversation {
    /// The ☺ button by the field (`tabs` — the composer's): accent while the panel is open.
    pub(super) fn render_panel_button(
        &self,
        tabs: &[PanelTab],
        _window: &mut Window,
        cx: &mut Context<Conversation>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let first = tabs.first().copied().unwrap_or(PanelTab::Emoji);
        let button = self.panel.button.clone();
        div()
            .flex_none()
            .on_children_prepainted(move |children, _, _| button.set(children.first().copied()))
            .child(
                ui::toggle_button("fg-panel-button", IconName::Smile, self.panel.open, ui)
                    .tooltip(ui::tooltip(tr("Emoji, Stickers and GIFs"), None))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        if this.panel.open {
                            this.close_panel(window, cx);
                        } else {
                            this.open_panel(first, window, cx);
                        }
                    })),
            )
            .into_any_element()
    }

    /// Opens the panel on its last tab (the first of `tabs` the first time).
    fn open_panel(&mut self, first: PanelTab, window: &mut Window, cx: &mut Context<Self>) {
        let tabs = self
            .spec
            .composer
            .as_ref()
            .map(|composer| composer.panel.clone())
            .unwrap_or_default();
        if !tabs.contains(&self.panel.tab) {
            self.panel.tab = first;
        }
        if self.panel.search.is_none() {
            let search = cx.new(|cx| {
                TextInput::new(search_placeholder(self.panel.tab), cx)
                    .borderless()
                    .icon(IconName::Search)
            });
            self.panel._search_events = Some(cx.subscribe_in(
                &search,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Changed => this.panel_searched(cx),
                },
            ));
            self.panel.search = Some(search);
        }
        let recent = Recent::get(cx);
        self.panel.recent = recent.emoji.clone();
        self.panel.tone = recent.tone;
        self.panel.open = true;
        self.panel.opened += 1;
        let tab = self.panel.tab;
        self.show_tab(tab, cx);
        // The field keeps the keyboard.
        window.focus(&self.input.focus_handle(cx));
        cx.notify();
    }

    pub(super) fn close_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.panel.open {
            return;
        }
        self.panel.open = false;
        self.panel.search_timer = None;
        self.panel.visible_timer = None;
        self.panel.visible_sent = None;
        // Each opening starts without a search: the plugin's tabs hear theirs is cleared.
        for tab in [PanelTab::Stickers, PanelTab::Gifs] {
            if !self.panel.queries[slot(tab)].trim().is_empty() {
                self.emit(UiEvent::PanelSearched((tab, String::new())), cx);
                *self.panel.content_mut(tab) = None;
            }
        }
        self.panel.queries = Default::default();
        if let Some(search) = self.panel.search.clone() {
            let focused = search.focus_handle(cx).is_focused(window);
            search.update(cx, |search, cx| search.set_text("", cx));
            if focused {
                window.focus(&self.input.focus_handle(cx));
            }
        }
        cx.notify();
    }

    /// Shows a tab: its search in the field, its rows from the top; a plugin's tab asks for its
    /// content (`panel-opened`, each time).
    fn show_tab(&mut self, tab: PanelTab, cx: &mut Context<Self>) {
        // A search still waiting for the typing to stop goes now, for the tab it was typed in.
        if self.panel.search_timer.take().is_some() {
            let left = self.panel.tab;
            let query = self.panel.queries[slot(left)].trim().to_string();
            self.emit(UiEvent::PanelSearched((left, query)), cx);
        }
        self.panel.tab = tab;
        self.panel.visible_timer = None;
        if let Some(search) = self.panel.search.clone() {
            let query = self.panel.queries[slot(tab)].clone();
            search.update(cx, |search, cx| {
                search.set_placeholder(search_placeholder(tab), cx);
                if search.text() != query {
                    search.set_text(&query, cx);
                }
            });
        }
        self.panel.rebuild(true);
        if tab != PanelTab::Emoji {
            self.panel.asked_more[slot(tab)] = false;
            self.emit(UiEvent::PanelOpened(tab), cx);
        }
        cx.notify();
    }

    /// The search field's text changed: the emoji tab searches at once; a plugin's tab tells the
    /// plugin when the typing stops.
    fn panel_searched(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &self.panel.search else {
            return;
        };
        let text = search.read(cx).text();
        let tab = self.panel.tab;
        if self.panel.queries[slot(tab)] == text {
            return;
        }
        let changed = self.panel.queries[slot(tab)].trim() != text.trim();
        self.panel.queries[slot(tab)] = text;
        if !changed {
            return;
        }
        if tab == PanelTab::Emoji {
            self.panel.rebuild(true);
            cx.notify();
            return;
        }
        self.panel.search_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DELAY).await;
            this.update(cx, |this, cx| {
                this.panel.search_timer = None;
                let query = this.panel.queries[slot(tab)].trim().to_string();
                this.emit(UiEvent::PanelSearched((tab, query)), cx);
            })
            .ok();
        }));
    }

    /// An emoji chosen: into the field at the caret, remembered; the field takes the keyboard.
    fn choose_emoji(&mut self, emoji: &'static Emoji, window: &mut Window, cx: &mut Context<Self>) {
        let text = emoji::with_tone(emoji, self.panel.tone);
        self.input.update(cx, |input, cx| input.insert(text, cx));
        Recent::chose(emoji, cx);
        window.focus(&self.input.focus_handle(cx));
    }

    fn set_tone(&mut self, tone: Tone, cx: &mut Context<Self>) {
        self.panel.tone = tone;
        Recent::set_tone(tone, cx);
        cx.notify();
    }

    /// The strip's section chosen: the list scrolls to it.
    fn scroll_to_section(&mut self, section: usize, cx: &mut Context<Self>) {
        if let Some(row) = self.panel.row_of(section) {
            self.panel.list.scroll_to(ListOffset {
                item_ix: row,
                offset_in_item: px(0.),
            });
            self.panel.current = section;
            cx.notify();
        }
    }

    /// After the list is laid out: the section in sight (the strip), the plugin's items on screen
    /// (`panel-visible`), the end near (`panel-load-more`).
    fn panel_sight(&mut self, cx: &mut Context<Self>) {
        let panel = &self.panel;
        if !panel.open || panel.rows.is_empty() {
            return;
        }
        let viewport = panel.list.viewport_bounds();
        let first = panel.list.logical_scroll_top().item_ix;
        let mut last = first;
        while last + 1 < panel.rows.len()
            && panel
                .list
                .bounds_for_item(last + 1)
                .is_some_and(|bounds| bounds.top() < viewport.bottom())
        {
            last += 1;
        }
        let current = panel.section_of(first);
        if current != panel.current {
            self.panel.current = current;
            cx.notify();
        }
        let tab = self.panel.tab;
        let Some(content) = self.panel.content(tab) else {
            return;
        };
        // The keys of the first and the last item on screen.
        let key = |row: &Row, first: bool| match row {
            Row::Items {
                section,
                start,
                end,
                ..
            } => {
                let items = &content.sections[*section].items;
                items
                    .get(if first { *start } else { end - 1 })
                    .map(|item| item.key.clone())
            }
            _ => None,
        };
        let rows = &self.panel.rows[first..=last.min(self.panel.rows.len() - 1)];
        let range = rows
            .iter()
            .find_map(|row| key(row, true))
            .zip(rows.iter().rev().find_map(|row| key(row, false)));
        let more = content.more == History::More
            && last + MORE_WITHIN + 1 >= self.panel.rows.len()
            && !self.panel.asked_more[slot(tab)];
        if more {
            self.panel.asked_more[slot(tab)] = true;
            self.emit(UiEvent::PanelLoadMore(tab), cx);
        }
        if let Some(range) = range
            && self.panel.visible_sent.as_ref() != Some(&range)
        {
            self.send_panel_visible(tab, range, cx);
        }
    }

    /// `panel-visible`, at most every [`super::VISIBLE_EVERY`]: one waiting for its turn goes
    /// when the period is over (the range then on screen).
    fn send_panel_visible(
        &mut self,
        tab: PanelTab,
        range: (String, String),
        cx: &mut Context<Self>,
    ) {
        let due = self
            .panel
            .visible_at
            .is_none_or(|at| at.elapsed() >= super::VISIBLE_EVERY);
        if due {
            self.panel.visible_at = Some(Instant::now());
            self.panel.visible_sent = Some(range.clone());
            self.panel.visible_timer = None;
            self.emit(UiEvent::PanelVisible((tab, range.0, range.1)), cx);
            return;
        }
        if self.panel.visible_timer.is_some() {
            return;
        }
        self.panel.visible_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(super::VISIBLE_EVERY).await;
            this.update(cx, |this, cx| {
                this.panel.visible_timer = None;
                this.panel.visible_at = None;
                this.panel_sight(cx);
            })
            .ok();
        }));
    }

    /// The panel over the feed while it is open, by the field's right edge (a popover drawn
    /// above everything; its keys are the conversation's, not the field's).
    pub(super) fn render_panel(
        &self,
        window: &mut Window,
        cx: &mut Context<Conversation>,
    ) -> Option<AnyElement> {
        if !self.panel.open {
            return None;
        }
        let field = self.field_bounds.get()?;
        let tabs = self.spec.composer.as_ref()?.panel.clone();
        let ui = Theme::ui(cx);
        let corner = point(field.right(), field.top() - px(GAP));
        let button = self.panel.button.clone();
        let weak = cx.entity().downgrade();
        let list = div()
            .flex_1()
            .min_h_0()
            .px(px(INSET))
            .on_children_prepainted(move |_, _, cx| {
                weak.update(cx, |this, cx| this.panel_sight(cx)).ok();
            })
            .child(
                list(
                    self.panel.list.clone(),
                    cx.processor(|this, ix, window, cx| this.render_panel_row(ix, window, cx)),
                )
                .size_full(),
            );
        let panel = ui::popover(ui)
            .id("fg-panel")
            .occlude()
            .w(px(WIDTH))
            .h(px(HEIGHT))
            .flex()
            .flex_col()
            .on_mouse_down_out(
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    // ☺ itself closes it by its click.
                    if button.get().is_some_and(|at| at.contains(&event.position)) {
                        return;
                    }
                    this.close_panel(window, cx);
                }),
            )
            // Esc in the panel's search: closes it, the field gets the keyboard.
            .capture_action(cx.listener(|this, _: &ClearSelection, window, cx| {
                cx.stop_propagation();
                this.close_panel(window, cx);
            }))
            .child(self.render_panel_tabs(&tabs, ui, cx))
            .child(self.render_panel_search(ui, cx))
            .child(list)
            .child(ui::divider(ui))
            .child(self.render_panel_strip(window, cx));
        let panel = motion::Appear::pop((1., 1.)).apply_from(
            ("fg-panel-appear", self.panel.opened as usize),
            corner,
            panel,
        );
        Some(
            deferred(
                anchored()
                    .anchor(Corner::BottomRight)
                    .position(corner)
                    .snap_to_window_with_margin(px(crate::popup::WINDOW_MARGIN))
                    .child(panel),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// The tabs at the panel's top, the selected one on a gliding pill.
    fn render_panel_tabs(
        &self,
        tabs: &[PanelTab],
        ui: UiColors,
        cx: &mut Context<Conversation>,
    ) -> impl IntoElement + use<> {
        let selected = tabs.iter().position(|tab| *tab == self.panel.tab);
        let pill = self.panel.segments.pill(selected, "fg-panel-tab-pill", ui);
        let own_backdrop = pill.is_none();
        let segments = self.panel.segments.clone();
        let tabs = tabs.iter().enumerate().map(|(index, tab)| {
            let tab = *tab;
            let on = selected == Some(index);
            div()
                .id(("fg-panel-tab", index))
                .flex_1()
                .h(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(ui::RADIUS_SM))
                .cursor_pointer()
                .text_size(px(theme::TEXT_SM))
                .text_color(if on { ui.foreground } else { ui.text_muted })
                .when(on, |chip| chip.font_weight(gpui::FontWeight::MEDIUM))
                .when(on && own_backdrop, |chip| chip.bg(ui.list_selected))
                .when(!on, |chip| {
                    chip.hover(move |style| style.bg(ui.hover).text_color(ui.foreground))
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if this.panel.tab != tab {
                        this.show_tab(tab, cx);
                    }
                }))
                .child(segments.measure(index))
                .child(tab_title(tab))
        });
        div()
            .flex_none()
            .h(px(TABS_HEIGHT))
            .px(px(INSET + 2.))
            .pt(px(8.))
            .pb(px(6.))
            .child(
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .gap_1()
                    .child(self.panel.segments.measure_origin())
                    .children(pill)
                    .children(tabs),
            )
    }

    /// The search field, with the skin tones' circles on the emoji tab.
    fn render_panel_search(&self, ui: UiColors, cx: &mut Context<Conversation>) -> AnyElement {
        let tones = (self.panel.tab == PanelTab::Emoji).then(|| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(3.))
                .children((0..emoji::TONES).map(|tone| {
                    let on = tone == self.panel.tone;
                    div()
                        .id(("fg-panel-tone", usize::from(tone)))
                        .size(px(18.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .cursor_pointer()
                        .border_1()
                        .border_color(if on {
                            ui.accent
                        } else {
                            gpui::transparent_black()
                        })
                        .when(!on, |ring| {
                            ring.hover(move |style| style.border_color(ui.hover))
                        })
                        .tooltip(ui::tooltip(tr(TONE_NAMES[usize::from(tone)]), None))
                        .on_click(
                            cx.listener(move |this, _: &ClickEvent, _, cx| this.set_tone(tone, cx)),
                        )
                        .child(
                            div()
                                .size(px(12.))
                                .rounded_full()
                                .bg(emoji::tone_swatch(tone)),
                        )
                }))
        });
        div()
            .flex_none()
            .h(px(SEARCH_HEIGHT))
            .mx(px(INSET + 2.))
            .mb(px(4.))
            .pl(px(8.))
            .pr(px(6.))
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(ui::RADIUS_MD))
            .bg(ui.input_background)
            .border_1()
            .border_color(ui.input_border)
            .child(div().flex_1().min_w_0().children(self.panel.search.clone()))
            .children(tones)
            .into_any_element()
    }

    /// The strip of the tab's sections at the panel's foot: the one in sight marked.
    fn render_panel_strip(
        &self,
        window: &mut Window,
        cx: &mut Context<Conversation>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let button =
            |index: usize, glyph: AnyElement, title: SharedString, cx: &mut Context<Self>| {
                let on = index == self.panel.current;
                div()
                    .id(("fg-panel-section", index))
                    .flex_none()
                    .size(px(STRIP_BUTTON))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(ui::RADIUS_MD))
                    .cursor_pointer()
                    .when(on, |button| button.bg(ui.list_selected))
                    .when(!on, |button| button.hover(move |style| style.bg(ui.hover)))
                    .when(!title.is_empty(), |button| {
                        button.tooltip(ui::tooltip(title.clone(), None))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.scroll_to_section(index, cx)
                    }))
                    .child(glyph)
                    .into_any_element()
            };
        let mut buttons = Vec::new();
        match self.panel.tab {
            PanelTab::Emoji => {
                // The search's results have no sections to go to.
                if self.panel.queries[0].trim().is_empty() {
                    for (index, section) in self.panel.emoji.iter().enumerate() {
                        let glyph = match section.icon {
                            Some(glyph) => div()
                                .text_size(px(18.))
                                .line_height(px(STRIP_BUTTON))
                                .child(glyph)
                                .into_any_element(),
                            None => icon(IconName::Clock, ui.text_muted).into_any_element(),
                        };
                        buttons.push(button(index, glyph, tr(section.title).into(), cx));
                    }
                }
            }
            tab => {
                if let Some(content) = self.panel.content(tab) {
                    for (index, section) in content.sections.iter().enumerate() {
                        let Some(spec) = section
                            .icon
                            .as_ref()
                            .or(section.items.first().map(|item| &item.image))
                        else {
                            continue;
                        };
                        let size = fit(spec.width, spec.height, 24., 24.);
                        let glyph =
                            render_picture(spec, size, &self.plugin, cx.entity_id(), window, cx);
                        buttons.push(button(index, glyph, section.title.clone().into(), cx));
                    }
                }
            }
        }
        div()
            .id("fg-panel-strip")
            .flex_none()
            .h(px(STRIP_HEIGHT))
            .px(px(INSET))
            .flex()
            .items_center()
            .gap(px(2.))
            .overflow_x_scroll()
            .children(buttons)
            .into_any_element()
    }

    /// A row of the tab's list.
    fn render_panel_row(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Conversation>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let Some(row) = self.panel.rows.get(ix).cloned() else {
            return div().into_any_element();
        };
        match row {
            Row::Heading(section) => {
                let title = match self.panel.tab {
                    PanelTab::Emoji => self
                        .panel
                        .emoji
                        .get(section)
                        .map(|section| SharedString::from(tr(section.title))),
                    tab => self
                        .panel
                        .content(tab)
                        .and_then(|content| content.sections.get(section))
                        .map(|section| SharedString::from(section.title.clone())),
                };
                div()
                    .h(px(HEADING_HEIGHT))
                    .px(px(6.))
                    .pb(px(6.))
                    .flex()
                    .items_end()
                    .text_size(px(theme::TEXT_SM))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(ui.dim)
                    .children(title)
                    .into_any_element()
            }
            Row::Items {
                section,
                start,
                end,
                height,
            } => match self.panel.tab {
                PanelTab::Emoji => self.render_emoji_row(section, start..end, ui, cx),
                tab => self.render_items_row(tab, section, start..end, height, window, cx),
            },
            Row::Loading => div()
                .h(px(LOADING_HEIGHT))
                .flex()
                .items_center()
                .justify_center()
                .child(ui::pulse_dot("fg-panel-loading", 8., ui.accent))
                .into_any_element(),
            Row::Empty => div()
                .h(px(EMPTY_HEIGHT))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.text_muted)
                .child(
                    if self.panel.queries[slot(self.panel.tab)].trim().is_empty() {
                        tr("Nothing here yet")
                    } else {
                        tr("Nothing found")
                    },
                )
                .into_any_element(),
        }
    }

    fn render_emoji_row(
        &self,
        section: usize,
        range: std::ops::Range<usize>,
        ui: UiColors,
        cx: &mut Context<Conversation>,
    ) -> AnyElement {
        let Some(items) = self.panel.emoji.get(section) else {
            return div().into_any_element();
        };
        let tone = self.panel.tone;
        let cells = items.items[range.clone()]
            .iter()
            .enumerate()
            .map(|(at, emoji)| {
                let emoji: &'static Emoji = emoji;
                div()
                    .id(ElementId::from((
                        "fg-panel-emoji",
                        (section << 16) | (range.start + at),
                    )))
                    .flex_none()
                    .size(px(EMOJI_CELL))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(ui::RADIUS_MD))
                    .cursor_pointer()
                    .hover(move |style| style.bg(ui.hover))
                    .text_size(px(EMOJI_SIZE))
                    .line_height(px(EMOJI_CELL))
                    .tooltip(ui::tooltip(emoji.local_name(), None))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.choose_emoji(emoji, window, cx)
                    }))
                    .child(emoji::with_tone(emoji, tone))
            });
        div()
            .h(px(EMOJI_CELL))
            .flex()
            .children(cells)
            .into_any_element()
    }

    /// A row of stickers (each fit into its square) or GIFs (each as wide as its proportions).
    fn render_items_row(
        &self,
        tab: PanelTab,
        section: usize,
        range: std::ops::Range<usize>,
        height: f32,
        window: &mut Window,
        cx: &mut Context<Conversation>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let Some(items) = self
            .panel
            .content(tab)
            .and_then(|content| content.sections.get(section))
            .map(|section| {
                &section.items
                    [range.start.min(section.items.len())..range.end.min(section.items.len())]
            })
        else {
            return div().into_any_element();
        };
        let gifs = tab == PanelTab::Gifs;
        // A full row of GIFs: their widths fill it; the last row keeps their own.
        let total: f32 = items.iter().map(gif_aspect).sum();
        let room = GRID - GIF_GAP * items.len().saturating_sub(1) as f32;
        let full = (total * height - room).abs() < height;
        let cells: Vec<AnyElement> = items
            .iter()
            .enumerate()
            .map(|(at, item)| {
                let key = item.key.clone();
                let media_key = format!("panel/{}", item.key);
                let id = ElementId::from(("fg-panel-item", (section << 16) | (range.start + at)));
                let cell = if gifs {
                    let width = if full {
                        room * gif_aspect(item) / total
                    } else {
                        (gif_aspect(item) * height).floor()
                    };
                    let gif = self.media.render_gif(
                        &media_key,
                        item.video.as_deref(),
                        &item.image,
                        (width, height),
                        window,
                        cx,
                    );
                    div()
                        .id(id)
                        .flex_none()
                        .w(px(width))
                        .h(px(height))
                        .cursor_pointer()
                        .hover(|style| style.opacity(0.85))
                        .child(gif)
                } else {
                    let size = fit(
                        item.image.width,
                        item.image.height,
                        STICKER_SIZE,
                        STICKER_SIZE,
                    );
                    let sticker =
                        self.media
                            .render_animation(&media_key, &item.image, size, window, cx);
                    div()
                        .id(id)
                        .flex_none()
                        .w(px(STICKER_CELL))
                        .h(px(STICKER_ROW))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(ui::RADIUS_MD))
                        .cursor_pointer()
                        .hover(move |style| style.bg(ui.hover))
                        .when_some(item.emoji.clone(), |cell, emoji| {
                            cell.tooltip(ui::tooltip(emoji, None))
                        })
                        .child(sticker)
                };
                cell.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.emit(UiEvent::PanelChosen((tab, key.clone())), cx)
                }))
                .into_any_element()
            })
            .collect();
        div()
            .h(px(height + if gifs { GIF_GAP } else { 0. }))
            .flex()
            .gap(px(if gifs { GIF_GAP } else { 0. }))
            .children(cells)
            .into_any_element()
    }
}

/// Esc in the field closes the open panel first (the field's own Esc leaves an edit).
pub(super) fn field_escape(
    this: &mut Conversation,
    _: &CancelEdit,
    window: &mut Window,
    cx: &mut Context<Conversation>,
) {
    if this.panel.is_open() {
        cx.stop_propagation();
        this.close_panel(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gifs_fill_their_rows() {
        // Four squares at 100 px in a row 310 wide (gaps 10): three fill it, scaled to 100.
        let rows = justify(&[1., 1., 1., 1.], 310., 100., 5.);
        assert_eq!(rows, [(0, 3, 100.), (3, 4, 100.)]);
        // A wide one and a tall one: the row is scaled down to fit.
        let rows = justify(&[2., 0.5, 1.], 300., 100., 0.);
        assert_eq!(rows, [(0, 3, 85.)]);
        assert!(justify(&[], 300., 100., 4.).is_empty());
    }

    #[test]
    fn emoji_rows_go_by_section() {
        let sections = vec![
            EmojiSection {
                title: "Recent",
                icon: None,
                items: emoji::categories()[0].emoji()[..3].iter().collect(),
            },
            EmojiSection {
                title: "Flags",
                icon: Some("🏁"),
                items: emoji::categories()[7].emoji()[..10].iter().collect(),
            },
        ];
        let rows = emoji_rows(&sections);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0], Row::Heading(0));
        assert!(matches!(
            rows[1],
            Row::Items {
                section: 0,
                start: 0,
                end: 3,
                ..
            }
        ));
        assert_eq!(rows[2], Row::Heading(1));
        assert!(matches!(
            rows[4],
            Row::Items {
                section: 1,
                start: 9,
                end: 10,
                ..
            }
        ));
        // The search's results have no heading; nothing found — an empty row.
        let found = vec![EmojiSection {
            title: "",
            icon: None,
            items: Vec::new(),
        }];
        assert_eq!(emoji_rows(&found), [Row::Empty]);
    }
}
