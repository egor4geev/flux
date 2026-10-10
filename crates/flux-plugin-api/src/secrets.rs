//! The plugin's secrets — tokens, passwords — in the macOS keychain, not in files: only this
//! plugin reads them, and they go when it is uninstalled. Keep a sign-in's token here, not in
//! [`crate::storage`].
//!
//! ```ignore
//! secrets::set("token", &token)?;
//! let token = secrets::get("token");
//! ```

use crate::host::secrets as raw;

/// The secret under `key`; none when there is none.
pub fn get(key: &str) -> Option<String> {
    raw::get(key)
}

/// Keeps a secret under `key`, replacing one there.
pub fn set(key: &str, value: &str) -> Result<(), String> {
    raw::set(key, Some(value))
}

/// Forgets the secret under `key`.
pub fn remove(key: &str) -> Result<(), String> {
    raw::set(key, None)
}
