//! A popup list to choose from (API 0.3), drawn by Flux as its own pickers (Open Task, the
//! branches): a search field over grouped items — or a short menu without one. Flux filters the
//! items by the query, or a [`Picker::live`] plugin searches itself. The choice comes as
//! `Event::PickerChosen((id, key))` (none — dismissed); a plugin's new picker closes its previous
//! one.
//!
//! ```ignore
//! self.picker = picker::Picker::new(&tr("Find a task by its key or words"))
//!     .live()
//!     .items(tasks.iter().map(|task| {
//!         picker::item(&task.key, &task.summary).detail(&task.key).group(&task.status)
//!     }))
//!     .open();
//! // Event::PickerQuery((id, query)) → picker::update(id, found, false)
//! // Event::PickerChosen((id, Some(key))) → open the task
//! let menu = picker::Picker::menu()
//!     .item(picker::item("in-progress", &tr("In Progress")))
//!     .under("tracker", "status")
//!     .open();
//! ```

use crate::host::picker::{self as raw, PickerAnchor, PickerItem, PickerSpec};
use crate::host::ui::Span;
use crate::view::span;

/// A picker being built: [`Picker::new`] or [`Picker::menu`], then the methods, then
/// [`Picker::open`].
#[derive(Debug, Clone)]
pub struct Picker(PickerSpec);

impl Picker {
    /// A picker with a search field; `placeholder` — its gray text.
    pub fn new(placeholder: &str) -> Self {
        Picker(PickerSpec {
            placeholder: placeholder.to_string(),
            items: Vec::new(),
            searchable: true,
            live_query: false,
            empty_text: None,
            loading: false,
            anchor: PickerAnchor::Window,
        })
    }

    /// A short menu without a search field: a status's transitions, the day's meetings.
    pub fn menu() -> Self {
        let mut picker = Self::new("");
        picker.0.searchable = false;
        picker
    }

    pub fn item(mut self, item: Item) -> Self {
        self.0.items.push(item.0);
        self
    }

    pub fn items(mut self, items: impl IntoIterator<Item = Item>) -> Self {
        self.0.items.extend(items.into_iter().map(|item| item.0));
        self
    }

    /// The plugin searches itself: every change of the query comes as `Event::PickerQuery`, and
    /// the plugin answers with [`update`].
    pub fn live(mut self) -> Self {
        self.0.live_query = true;
        self
    }

    /// A spinner in the field: the plugin is still looking.
    pub fn loading(mut self) -> Self {
        self.0.loading = true;
        self
    }

    /// Shown when there are no items.
    pub fn empty_text(mut self, text: &str) -> Self {
        self.0.empty_text = Some(text.to_string());
        self
    }

    /// Above one of the plugin's status bar items (`[[status-items]]`).
    pub fn at_status_item(mut self, item: &str) -> Self {
        self.0.anchor = PickerAnchor::StatusItem(item.to_string());
        self
    }

    /// Under an element of one of the plugin's tool windows.
    pub fn under(mut self, window: &str, element: &str) -> Self {
        self.0.anchor = PickerAnchor::Element((window.to_string(), element.to_string()));
        self
    }

    /// Opens the picker; returns its id.
    pub fn open(&self) -> u64 {
        raw::open(&self.0)
    }

    /// What it would open: the spec.
    pub fn spec(&self) -> &PickerSpec {
        &self.0
    }
}

/// An item of a picker: [`item`], then the methods.
#[derive(Debug, Clone, PartialEq)]
pub struct Item(PickerItem);

/// An item named by `key` (in `Event::PickerChosen`) with `label`.
pub fn item(key: &str, label: &str) -> Item {
    Item(PickerItem {
        key: key.to_string(),
        icon: None,
        label: vec![span(label)],
        detail: None,
        badge: None,
        group: None,
    })
}

impl Item {
    /// A label of several spans: a key in the code font, then a title.
    pub fn spans(mut self, spans: impl IntoIterator<Item = Span>) -> Self {
        self.0.label = spans.into_iter().collect();
        self
    }

    /// A named icon, as in tool windows: a built-in name, `file:<name>`, an SVG of the plugin.
    pub fn icon(mut self, icon: &str) -> Self {
        self.0.icon = Some(icon.to_string());
        self
    }

    /// Dim text after the label.
    pub fn detail(mut self, detail: &str) -> Self {
        self.0.detail = Some(detail.to_string());
        self
    }

    /// On the right: a status, a time.
    pub fn badge(mut self, badge: &str) -> Self {
        self.0.badge = Some(badge.to_string());
        self
    }

    /// The heading of the item's group: "Today", "In Progress".
    pub fn group(mut self, group: &str) -> Self {
        self.0.group = Some(group.to_string());
        self
    }
}

impl From<Item> for PickerItem {
    fn from(item: Item) -> PickerItem {
        item.0
    }
}

/// New items of an open picker (a live one's search results); `loading` — still looking.
pub fn update(picker: u64, items: impl IntoIterator<Item = Item>, loading: bool) {
    let items: Vec<PickerItem> = items.into_iter().map(|item| item.0).collect();
    raw::update(picker, &items, loading)
}

/// Closes it without a choice.
pub fn close(picker: u64) {
    raw::close(picker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::SpanStyle;
    use crate::view::Tone;

    #[test]
    fn builds_a_picker() {
        let picker = Picker::new("Find")
            .live()
            .loading()
            .empty_text("Nothing")
            .at_status_item("active-task")
            .item(item("a", "Alpha").detail("1").group("Mine"))
            .items([item("b", "Beta")
                .spans([span("TRACK-2").code(), span(" Beta").tone(Tone::Dim)])
                .icon("tracker")
                .badge("Open")]);
        let spec = picker.spec();
        assert!(spec.searchable && spec.live_query && spec.loading);
        assert_eq!(spec.empty_text.as_deref(), Some("Nothing"));
        assert_eq!(spec.anchor, PickerAnchor::StatusItem("active-task".into()));
        assert_eq!(spec.items[0].group.as_deref(), Some("Mine"));
        assert_eq!(spec.items[1].label[0].text, "TRACK-2");
        assert!(spec.items[1].label[0].code);
        let menu = Picker::menu().under("tracker", "status");
        assert!(!menu.spec().searchable);
        assert_eq!(
            menu.spec().anchor,
            PickerAnchor::Element(("tracker".into(), "status".into()))
        );
    }
}
