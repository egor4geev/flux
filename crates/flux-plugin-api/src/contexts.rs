//! Contexts (API 0.3), as Tasks & Contexts in JetBrains: the window's open tabs kept under a key,
//! so a task's tabs come back when the user returns to it. The keys are the plugin's own; the
//! user sees the contexts by their titles in Load Context.
//!
//! ```ignore
//! // Starting work on TRACK-142 after TRACK-140:
//! contexts::switch(Some(("TRACK-140", "TRACK-140 Fix the header")), Some("TRACK-142"))?;
//! ```

use crate::host::contexts::{self as raw, ContextRef};

/// Saves the open tabs as the context `save` — (key, title) — when given, closes them (documents
/// with unsaved changes and running Claude sessions stay) and opens the tabs of `load` when it was
/// saved.
pub fn switch(save: Option<(&str, &str)>, load: Option<&str>) -> Result<(), String> {
    let save = save.map(|(key, title)| ContextRef {
        key: key.to_string(),
        title: title.to_string(),
    });
    raw::switch(save.as_ref(), load)
}

/// Forgets the saved context `key`.
pub fn remove(key: &str) {
    raw::remove(key)
}
