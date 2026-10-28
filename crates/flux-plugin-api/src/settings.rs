//! Writing the plugin's settings (API 0.3) and the lines of its settings page; reading is
//! [`crate::setting`]. A `secret` setting is one of the plugin's secrets: [`crate::secrets::get`]
//! with its key.
//!
//! ```ignore
//! settings::busy("connection-status", &tr("Checking…"));
//! settings::status("connection-status", &trf("Signed in as {0}", &[&login]), Tone::Success);
//! settings::set("org-id", "7654321")?;
//! ```

use crate::host::settings::{self as raw, SettingStatus};
pub use crate::host::ui::Tone;

/// Writes a setting's value: the page shows it at once (the plugin isn't told
/// `settings-changed` about its own write). The error says why a value doesn't fit.
pub fn set<T: serde::Serialize + ?Sized>(key: &str, value: &T) -> Result<(), String> {
    let json = serde_json::to_string(value).map_err(|err| err.to_string())?;
    raw::set(key, Some(&json))
}

/// Brings a setting back to the manifest's default.
pub fn reset(key: &str) -> Result<(), String> {
    raw::set(key, None)
}

/// A `status` line of the settings page: `Tone::Success` for "Signed in as …", `Tone::Error` for a
/// failed check.
pub fn status(key: &str, text: &str, tone: Tone) {
    raw::set_status(
        key,
        Some(&SettingStatus {
            text: text.to_string(),
            tone,
            busy: false,
        }),
    )
}

/// A `status` line with a spinner: "Checking…".
pub fn busy(key: &str, text: &str) {
    raw::set_status(
        key,
        Some(&SettingStatus {
            text: text.to_string(),
            tone: Tone::Muted,
            busy: true,
        }),
    )
}

/// The manifest's text of the line again.
pub fn clear_status(key: &str) {
    raw::set_status(key, None)
}
