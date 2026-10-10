//! The plugin's status bar items (`[[status-items]]` of the manifest):
//!
//! ```ignore
//! status::set("clock", "09:41");
//! status::item("2 meetings today").tooltip("Next: Standup at 10:00").command("agenda").show("agenda");
//! status::hide("clock");
//! ```

use crate::host::status_bar::{self, StatusItem};

/// Shows (or updates) the item `id` with `text`.
pub fn set(id: &str, text: &str) {
    item(text).show(id)
}

/// Hides the item `id`.
pub fn hide(id: &str) {
    status_bar::set(id, None)
}

/// An item with more than text: [`item`], then the methods add to it.
#[derive(Debug, Clone)]
pub struct Item(StatusItem);

pub fn item(text: &str) -> Item {
    Item(StatusItem {
        text: text.to_string(),
        tooltip: None,
        command: None,
    })
}

impl Item {
    /// Shown when the pointer rests on the item.
    pub fn tooltip(mut self, tooltip: &str) -> Self {
        self.0.tooltip = Some(tooltip.to_string());
        self
    }

    /// One of the plugin's commands a click runs.
    pub fn command(mut self, command: &str) -> Self {
        self.0.command = Some(command.to_string());
        self
    }

    /// Shows it as the item `id`.
    pub fn show(&self, id: &str) {
        status_bar::set(id, Some(&self.0))
    }
}
