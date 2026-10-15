//! `picker` (plugin API 0.3, part 10.3): a plugin's popup list, drawn as Flux's own (Open Task, the
//! branches): a search field over grouped items with icons, details and badges — or a short menu
//! without one (a status's transitions, the day's meetings). Flux filters the items by the query
//! (fuzzy, as the palette; the groups stay in their order), or a `live-query` plugin searches
//! itself (`picker-query`, after the typing rests; `update` with a spinner while it looks). It opens
//! under the title bar, above one of the plugin's status bar items or under an element of its tool
//! window — at the window's top when that isn't on screen.
//!
//! Every picker is answered exactly once with `picker-chosen`: the chosen item's key (↵, a click),
//! or none — Esc, a click outside, `picker.close`, the plugin's (or another) new picker, a dialog in
//! the way. The answer goes when the popup's entity is released, so no way of closing it is missed.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use flux_plugin::api::events::Event;
use flux_plugin::api::picker::{PickerAnchor, PickerItem, PickerSpec};
use flux_plugin::api::ui::Span;
use flux_search::match_list;
use gpui::{
    AnyElement, App, Bounds, Context, Corner, DismissEvent, FontWeight, Global, Pixels, Point,
    SharedString, StyledText, Task, TextRun, WeakEntity, Window, WindowId, div, font, point,
    prelude::*, px, relative,
};

use crate::i18n::tr;
use crate::icons::plugin_icon;
use crate::picker::{PICKER_WIDTH, Picker, PickerDelegate, ROW_HEIGHT};
use crate::plugin_view::{single_line, tone_color};
use crate::plugins::PluginStore;
use crate::theme::{self, Theme, UiColors};
use crate::ui;
use crate::workspace::Workspace;

/// How long the query rests before a live picker's plugin hears it.
const QUERY_DELAY: Duration = Duration::from_millis(150);
/// A menu (no search field) is as wide as its rows, within these bounds, and its rows lower — as
/// JetBrains' list popups.
/// A menu (no search field) is sized as Flux's context menus: their least width, their rows.
const MENU_MIN_WIDTH: f32 = crate::context_menu::MENU_MIN_WIDTH;
const MENU_MAX_WIDTH: f32 = 440.;
const MENU_ROW_HEIGHT: f32 = 30.;
/// A searchable picker under an element (a board's dropdown) is a dropdown, not the palette.
const DROPDOWN_WIDTH: f32 = 440.;
/// The width of a character of the interface font at the list's size, for a menu's width.
const CHAR_WIDTH: f32 = 7.5;
/// A row's insets, the icon and the gaps.
const ROW_CHROME: f32 = 56.;
/// Between the anchor (a status bar item, an element) and the popup.
const ANCHOR_GAP: f32 = 6.;

/// Where the plugins' status bar items were drawn last, per window: a picker opens above one.
/// Written at every frame through the cell (changing a global itself would wake its observers).
#[derive(Default)]
struct StatusItemBounds(RefCell<HashMap<(WindowId, SharedString, SharedString), Bounds<Pixels>>>);

impl Global for StatusItemBounds {}

/// The status bar drew the plugins' items `keys` ((plugin, item)) at `bounds`.
pub(crate) fn record_status_items(
    keys: &[(SharedString, SharedString)],
    bounds: &[Bounds<Pixels>],
    window: &Window,
    cx: &mut App,
) {
    if !cx.has_global::<StatusItemBounds>() {
        cx.set_global(StatusItemBounds::default());
    }
    let window = window.window_handle().window_id();
    let mut map = cx.global::<StatusItemBounds>().0.borrow_mut();
    map.retain(|(owner, _, _), _| *owner != window);
    for ((plugin, item), bounds) in keys.iter().zip(bounds) {
        map.insert((window, plugin.clone(), item.clone()), *bounds);
    }
}

fn status_item_bounds(
    window: WindowId,
    plugin: &str,
    item: &str,
    cx: &App,
) -> Option<Bounds<Pixels>> {
    let map = cx.try_global::<StatusItemBounds>()?.0.borrow();
    map.iter()
        .find(|((owner, owner_plugin, owner_item), _)| {
            *owner == window && owner_plugin == plugin && owner_item == item
        })
        .map(|(_, bounds)| *bounds)
}

/// Opens a plugin's picker (closing its previous one).
pub(crate) fn open(
    workspace: &mut Workspace,
    plugin: &str,
    id: u64,
    spec: &PickerSpec,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    // A picker open now (this plugin's or another's) gives way, answered as dismissed — reopening
    // the same kind of popup would only close it.
    if workspace.modal_is::<Picker<PluginPicker>>() {
        workspace.dismiss_modal(window, cx);
    }
    let anchor = anchor_point(workspace, plugin, &spec.anchor, window, cx);
    let delegate = PluginPicker::new(workspace.plugins.downgrade(), plugin, id, spec);
    workspace.toggle_modal(window, cx, |window, cx| Picker::new(delegate, window, cx));
    let opened = workspace
        .modal_view::<Picker<PluginPicker>>()
        .filter(|picker| picker.read(cx).delegate.is(plugin, id));
    let Some(picker) = opened else {
        // A dialog is open: popups don't open behind it.
        workspace
            .plugins
            .read(cx)
            .send_to(plugin, Event::PickerChosen((id, None)));
        return;
    };
    cx.observe_release(&picker, |_, picker, cx| picker.delegate.answer(None, cx))
        .detach();
    match anchor {
        Some((point, corner)) => workspace.anchor_modal_at(point, corner),
        // The status item or the element isn't drawn yet — the plugin showed its window in the
        // same call: once a frame has drawn it, the picker moves under it.
        None if spec.anchor != PickerAnchor::Window => anchor_later(
            plugin.to_string(),
            spec.anchor.clone(),
            picker.entity_id(),
            ANCHOR_FRAMES,
            window,
            cx,
        ),
        None => {}
    }
}

/// How many frames a picker waits for its status item or element to be drawn.
const ANCHOR_FRAMES: usize = 4;

/// Places the open picker `picker` under its anchor once a frame has drawn the anchor; gpui runs
/// a next frame's callbacks before drawing it, so the first try may come too early.
fn anchor_later(
    plugin: String,
    anchor: PickerAnchor,
    picker: gpui::EntityId,
    tries: usize,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    cx.on_next_frame(window, move |workspace, window, cx| {
        let open = workspace
            .modal_view::<Picker<PluginPicker>>()
            .is_some_and(|open| open.entity_id() == picker);
        if !open {
            return;
        }
        match anchor_point(workspace, &plugin, &anchor, window, cx) {
            Some((point, corner)) => {
                workspace.anchor_modal_at(point, corner);
                cx.notify();
            }
            None if tries > 1 => anchor_later(plugin, anchor, picker, tries - 1, window, cx),
            None => {}
        }
    });
}

/// New items of a plugin's open picker; `loading` — it still looks.
pub(crate) fn update(
    workspace: &mut Workspace,
    plugin: &str,
    id: u64,
    items: &[PickerItem],
    loading: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let _ = window;
    let Some(picker) = workspace.modal_view::<Picker<PluginPicker>>() else {
        return;
    };
    picker.update(cx, |picker, cx| {
        if !picker.delegate.is(plugin, id) {
            return;
        }
        let query = picker.query(cx);
        picker.delegate.set_items(items.to_vec(), loading, &query);
        picker.select_first(cx);
    });
}

/// Closes a plugin's picker without a choice.
pub(crate) fn close(
    workspace: &mut Workspace,
    plugin: &str,
    id: u64,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let open = workspace
        .modal_view::<Picker<PluginPicker>>()
        .is_some_and(|picker| picker.read(cx).delegate.is(plugin, id));
    if open {
        workspace.dismiss_modal(window, cx);
    }
}

/// The plugin stopped (it failed, was turned off, restarts): its open picker closes — a choice
/// would go nowhere.
pub(crate) fn plugin_stopped(
    workspace: &mut Workspace,
    plugin: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let open = workspace
        .modal_view::<Picker<PluginPicker>>()
        .is_some_and(|picker| picker.read(cx).delegate.plugin == plugin);
    if open {
        workspace.dismiss_modal(window, cx);
    }
}

/// Where a picker opens: none — under the title bar.
fn anchor_point(
    workspace: &Workspace,
    plugin: &str,
    anchor: &PickerAnchor,
    window: &Window,
    cx: &App,
) -> Option<(Point<Pixels>, Corner)> {
    match anchor {
        PickerAnchor::Window => None,
        // Above the item, from its left edge, as JetBrains' status bar popups.
        PickerAnchor::StatusItem(item) => {
            let bounds =
                status_item_bounds(window.window_handle().window_id(), plugin, item, cx)?;
            Some((
                point(bounds.left(), bounds.top() - px(ANCHOR_GAP)),
                Corner::BottomLeft,
            ))
        }
        // Under the element, from its left edge, as a dropdown.
        PickerAnchor::Element((tool, element)) => {
            let store = workspace.plugins.read(cx);
            let tool = store
                .tool_windows()
                .iter()
                .find(|known| known.plugin == plugin && known.id == *tool)?;
            let bounds = tool.view.read(cx).element_bounds(element)?;
            Some((
                point(bounds.left(), bounds.bottom() + px(ANCHOR_GAP)),
                Corner::TopLeft,
            ))
        }
    }
}

/// A plugin's picker: its items as rows — the groups' headings and the items.
pub struct PluginPicker {
    store: WeakEntity<PluginStore>,
    plugin: SharedString,
    id: u64,
    placeholder: SharedString,
    searchable: bool,
    /// Opened under an element of a tool window: a dropdown's width.
    under_element: bool,
    live: bool,
    empty_text: Option<SharedString>,
    items: Vec<PickerItem>,
    loading: bool,
    rows: Vec<Row>,
    /// The query the rows are for.
    query: String,
    /// The plugin has its answer.
    answered: bool,
    /// The query waiting for the typing to rest (a live picker).
    _query: Option<Task<()>>,
}

/// A row of the list.
#[derive(Debug, PartialEq)]
enum Row {
    /// A group's heading: not selectable.
    Heading(SharedString),
    /// An item (its index), with the characters the query matched in its label.
    Item { index: usize, positions: Vec<usize> },
}

impl PluginPicker {
    fn new(store: WeakEntity<PluginStore>, plugin: &str, id: u64, spec: &PickerSpec) -> Self {
        let mut picker = Self {
            store,
            plugin: plugin.to_string().into(),
            id,
            placeholder: single_line(&spec.placeholder).into(),
            searchable: spec.searchable,
            under_element: matches!(spec.anchor, PickerAnchor::Element(_)),
            live: spec.live_query,
            empty_text: spec
                .empty_text
                .as_deref()
                .map(|text| single_line(text).into()),
            items: Vec::new(),
            loading: false,
            rows: Vec::new(),
            query: String::new(),
            answered: false,
            _query: None,
        };
        picker.set_items(spec.items.clone(), spec.loading, "");
        picker
    }

    fn is(&self, plugin: &str, id: u64) -> bool {
        self.plugin == plugin && self.id == id
    }

    /// The plugin's (new) items, shown for `query`.
    fn set_items(&mut self, items: Vec<PickerItem>, loading: bool, query: &str) {
        self.items = items;
        self.loading = loading;
        self.query = query.to_string();
        self.rows = rows(&self.items, if self.live { "" } else { query });
    }

    /// Tells the plugin its choice, once: an item's key, or none.
    fn answer(&mut self, key: Option<String>, cx: &App) {
        if std::mem::replace(&mut self.answered, true) {
            return;
        }
        if let Some(store) = self.store.upgrade() {
            store
                .read(cx)
                .send_to(&self.plugin, Event::PickerChosen((self.id, key)));
        }
    }

    fn render_item(
        &self,
        item: &PickerItem,
        positions: &[usize],
        selected: bool,
        cx: &App,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let item_icon = item
            .icon
            .as_deref()
            .and_then(|name| plugin_icon(&self.plugin, name, &ui));
        div()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .whitespace_nowrap()
            .children(item_icon.map(|item_icon| item_icon.render(ui.text_muted)))
            .child(
                div()
                    .min_w_0()
                    .flex_shrink()
                    .truncate()
                    .child(label_text(&item.label, positions, ui)),
            )
            // A detail keeps a part of the row; the label gives way first.
            .children(item.detail.as_deref().map(|detail| {
                div()
                    .flex_none()
                    .max_w(relative(0.45))
                    .truncate()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(if selected { ui.text_muted } else { ui.dim })
                    .child(single_line(detail))
            }))
            .child(div().flex_1().min_w(px(8.)))
            .children(
                item.badge
                    .as_deref()
                    .map(|badge| ui::badge(single_line(badge), ui.text_muted)),
            )
            .into_any_element()
    }
}

impl PickerDelegate for PluginPicker {
    fn placeholder(&self) -> SharedString {
        self.placeholder.clone()
    }

    fn match_count(&self) -> usize {
        self.rows.len()
    }

    fn update_matches(&mut self, query: &str, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        let changed = self.query != query;
        self.query = query.to_string();
        if !self.live {
            self.rows = rows(&self.items, query);
            return;
        }
        if !changed {
            return;
        }
        // The plugin searches: it hears the query once the typing rests, and the spinner shows
        // until its answer.
        self.loading = true;
        let query = query.to_string();
        self._query = Some(cx.spawn(async move |picker, cx| {
            cx.background_executor().timer(QUERY_DELAY).await;
            picker
                .update(cx, |picker, cx| {
                    let delegate = &picker.delegate;
                    if let Some(store) = delegate.store.upgrade() {
                        store
                            .read(cx)
                            .send_to(&delegate.plugin, Event::PickerQuery((delegate.id, query)));
                    }
                })
                .ok();
        }));
    }

    fn confirm(&mut self, index: usize, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(Row::Item { index, .. }) = self.rows.get(index) else {
            return;
        };
        let key = self.items[*index].key.clone();
        self.answer(Some(key), cx);
        cx.emit(DismissEvent);
    }

    fn render_match(
        &mut self,
        index: usize,
        selected: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        match self.rows.get(index) {
            Some(Row::Heading(heading)) => div()
                .size_full()
                .flex()
                .items_end()
                .pb(px(5.))
                .child(ui::section_label(heading.clone(), ui).truncate())
                .into_any_element(),
            Some(Row::Item { index, positions }) => match self.items.get(*index) {
                Some(item) => self.render_item(item, positions, selected, cx),
                None => div().into_any_element(),
            },
            None => div().into_any_element(),
        }
    }

    fn empty_message(&self) -> SharedString {
        if let Some(text) = &self.empty_text {
            return text.clone();
        }
        if self.loading {
            return tr("Searching…").into();
        }
        tr("No matches").into()
    }

    fn confirm_label(&self) -> &'static str {
        tr("choose")
    }

    fn selectable(&self, index: usize) -> bool {
        matches!(self.rows.get(index), Some(Row::Item { .. }))
    }

    fn searchable(&self) -> bool {
        self.searchable
    }

    fn loading(&self) -> bool {
        self.loading
    }

    fn width(&self) -> f32 {
        if self.searchable {
            return if self.under_element {
                DROPDOWN_WIDTH
            } else {
                PICKER_WIDTH
            };
        }
        // As wide as the longest row, estimated from its characters.
        let longest = self
            .items
            .iter()
            .map(|item| {
                let row = label_of(item).chars().count()
                    + item.detail.as_deref().map_or(0, |detail| detail.chars().count() + 2)
                    + item.badge.as_deref().map_or(0, |badge| badge.chars().count() + 3)
                    + if item.icon.is_some() { 3 } else { 0 };
                let heading = item.group.as_deref().map_or(0, |group| group.chars().count());
                row.max(heading)
            })
            .max()
            .unwrap_or(0);
        (longest as f32 * CHAR_WIDTH + ROW_CHROME).clamp(MENU_MIN_WIDTH, MENU_MAX_WIDTH)
    }

    fn row_height(&self) -> f32 {
        if self.searchable {
            ROW_HEIGHT
        } else {
            MENU_ROW_HEIGHT
        }
    }

    fn show_footer(&self) -> bool {
        self.searchable
    }
}

/// The text the query matches: the label's spans on one line.
fn label_of(item: &PickerItem) -> String {
    item.label
        .iter()
        .map(|span| single_line(&span.text))
        .collect()
}

/// The rows for `query`: all items without one; the matching ones with one — by how well they
/// match, or, when the items have groups, in their groups' order (a heading over each group with
/// matches; the groups in the order of their first item).
fn rows(items: &[PickerItem], query: &str) -> Vec<Row> {
    let labels: Vec<String> = items.iter().map(label_of).collect();
    let matches = match_list(query, &labels);
    if items.iter().all(|item| item.group.is_none()) {
        return matches
            .into_iter()
            .map(|found| Row::Item {
                index: found.index,
                positions: found.positions,
            })
            .collect();
    }
    let mut positions: HashMap<usize, Vec<usize>> = matches
        .into_iter()
        .map(|found| (found.index, found.positions))
        .collect();
    let mut groups: Vec<Option<&str>> = Vec::new();
    for item in items {
        let group = item.group.as_deref();
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
    let mut rows = Vec::new();
    for group in groups {
        let members: Vec<usize> = (0..items.len())
            .filter(|&index| {
                items[index].group.as_deref() == group && positions.contains_key(&index)
            })
            .collect();
        if members.is_empty() {
            continue;
        }
        if let Some(group) = group {
            rows.push(Row::Heading(single_line(group).into()));
        }
        for index in members {
            let positions = positions.remove(&index).unwrap_or_default();
            rows.push(Row::Item { index, positions });
        }
    }
    rows
}

/// An item's label: its spans' tones, weights and fonts, and the characters the query matched in
/// the match color.
fn label_text(spans: &[Span], positions: &[usize], ui: UiColors) -> StyledText {
    let mut text = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let mut positions = positions.iter().copied().peekable();
    let mut index = 0;
    for span in spans.iter().filter(|span| !span.text.is_empty()) {
        let piece = single_line(&span.text);
        let mut run_font = font(if span.code {
            theme::code_font()
        } else {
            theme::UI_FONT
        });
        if span.bold {
            run_font.weight = FontWeight::SEMIBOLD;
        }
        let background = span.highlight.then_some(ui.search_match);
        for c in piece.chars() {
            let color = if positions.next_if_eq(&index).is_some() {
                ui.match_text
            } else {
                tone_color(span.tone, ui)
            };
            match runs.last_mut() {
                Some(run)
                    if run.color == color
                        && run.font == run_font
                        && run.background_color == background =>
                {
                    run.len += c.len_utf8()
                }
                _ => runs.push(TextRun {
                    len: c.len_utf8(),
                    font: run_font.clone(),
                    color,
                    background_color: background,
                    underline: None,
                    strikethrough: None,
                }),
            }
            index += 1;
        }
        text.push_str(&piece);
    }
    StyledText::new(text).with_runs(runs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_plugin::api::ui::Tone;

    fn item(key: &str, label: &str, group: Option<&str>) -> PickerItem {
        PickerItem {
            key: key.into(),
            icon: None,
            label: vec![Span {
                text: label.into(),
                tone: Tone::Normal,
                bold: false,
                code: false,
                highlight: false,
            }],
            detail: None,
            badge: None,
            group: group.map(str::to_string),
        }
    }

    fn shape(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Heading(heading) => format!("# {heading}"),
                Row::Item { index, .. } => index.to_string(),
            })
            .collect()
    }

    #[test]
    fn groups_keep_the_order_of_their_first_item() {
        let items = [
            item("a", "Daily", Some("Today")),
            item("b", "Review", Some("Tomorrow")),
            item("c", "Retro", Some("Today")),
            item("d", "Lunch", None),
        ];
        assert_eq!(
            shape(&rows(&items, "")),
            ["# Today", "0", "2", "# Tomorrow", "1", "3"]
        );
        // A query keeps the groups with matches, in the same order.
        assert_eq!(shape(&rows(&items, "re")), ["# Today", "2", "# Tomorrow", "1"]);
    }

    #[test]
    fn without_groups_the_best_match_comes_first() {
        let items = [
            item("1", "TRACK-142 Fix the sign-in", None),
            item("2", "TRACK-7 Sign the build", None),
            item("3", "WEB-9 Footer", None),
        ];
        let found = rows(&items, "sign");
        assert_eq!(found.len(), 2);
        assert!(shape(&found).iter().all(|row| row != "2"));
        // The matched characters are known for the highlight.
        assert!(found.iter().all(|row| matches!(
            row,
            Row::Item { positions, .. } if positions.len() == 4
        )));
        assert_eq!(rows(&items, "").len(), 3);
    }

    #[test]
    fn a_label_is_its_spans_on_one_line() {
        let mut two = item("k", "TRACK-1", None);
        two.label.push(Span {
            text: " Fix\nit".into(),
            tone: Tone::Muted,
            bold: true,
            code: false,
            highlight: false,
        });
        assert_eq!(label_of(&two), "TRACK-1 Fix it");
    }
}
