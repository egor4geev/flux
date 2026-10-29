//! The macOS keychain, where Flux kept the plugins' secrets before it kept them itself
//! ([`crate::secrets`]): a generic password per key, under the plugin's service
//! ([`crate::secrets::service`]). The store moves them over once; uninstalling a plugin removes
//! whatever of its items is left (an item whose read the user denied).
//!
//! Only a Flux of its own touches the user's keychain ([`user`]): tests use a fake [`Keychain`].

use std::path::Path;
use std::sync::Arc;

/// The keychain as the store sees it: the plugins' items.
pub(crate) trait Keychain: Send + Sync {
    /// The service and account of every plugin's item: their attributes only, which macOS gives
    /// without asking the user.
    fn list(&self) -> Result<Vec<(String, String)>, String>;
    /// An item's secret. macOS may ask the user first (a Flux not on the item's access list).
    fn read(&self, service: &str, account: &str) -> Result<Option<String>, String>;
    fn delete(&self, service: &str, account: &str) -> Result<(), String>;
    /// Deletes every item of the service.
    fn delete_all(&self, service: &str) -> Result<(), String>;
}

/// The user's keychain — only for Flux run as an app (`Flux.app`, the build that is on the items'
/// access lists) with the user's own plugins: never in tests (this crate's or the app's), UI
/// scenarios, runs with `FLUX_PLUGINS_HOME` or `FLUX_PLUGIN_SECRETS_FILE`, nor a bare binary
/// (`cargo run`), which macOS would ask the user about for every item.
pub(crate) fn user() -> Option<Arc<dyn Keychain>> {
    if cfg!(test) || crate::paths::overridden() {
        return None;
    }
    let overridden = [
        "FLUX_PLUGINS_HOME",
        "FLUX_PLUGIN_SECRETS_FILE",
        "FLUX_SCENARIO",
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some());
    let bundled = std::env::current_exe().is_ok_and(|exe| in_app_bundle(&exe));
    if overridden || !bundled {
        return None;
    }
    platform::user()
}

/// `exe` is in a `*.app`'s `Contents/MacOS`.
fn in_app_bundle(exe: &Path) -> bool {
    exe.ancestors().skip(1).any(|dir| {
        dir.extension().is_some_and(|ext| ext == "app")
            && exe.starts_with(dir.join("Contents/MacOS"))
    })
}

#[cfg(target_os = "macos")]
mod platform {
    use std::sync::Arc;

    use security_framework::item::{ItemClass, ItemSearchOptions, Limit};
    use security_framework::passwords;

    use super::Keychain;
    use crate::secrets::SERVICE_PREFIX;

    /// `errSecItemNotFound`.
    const NOT_FOUND: i32 = -25300;

    pub fn user() -> Option<Arc<dyn Keychain>> {
        Some(Arc::new(System))
    }

    /// The user's keychain.
    struct System;

    impl Keychain for System {
        fn list(&self) -> Result<Vec<(String, String)>, String> {
            let found = ItemSearchOptions::new()
                .class(ItemClass::generic_password())
                .load_attributes(true)
                .limit(Limit::All)
                .search();
            let items = match found {
                Ok(items) => items,
                Err(err) if err.code() == NOT_FOUND => return Ok(Vec::new()),
                Err(err) => return Err(err.to_string()),
            };
            Ok(items
                .iter()
                .filter_map(|item| item.simplify_dict())
                .filter_map(|mut attributes| {
                    let service = attributes.remove("svce")?;
                    let account = attributes.remove("acct")?;
                    service
                        .starts_with(SERVICE_PREFIX)
                        .then_some((service, account))
                })
                .collect())
        }

        fn read(&self, service: &str, account: &str) -> Result<Option<String>, String> {
            match passwords::get_generic_password(service, account) {
                Ok(secret) => String::from_utf8(secret)
                    .map(Some)
                    .map_err(|_| "it isn't text".to_string()),
                Err(err) if err.code() == NOT_FOUND => Ok(None),
                Err(err) => Err(err.to_string()),
            }
        }

        fn delete(&self, service: &str, account: &str) -> Result<(), String> {
            match passwords::delete_generic_password(service, account) {
                Err(err) if err.code() != NOT_FOUND => Err(err.to_string()),
                _ => Ok(()),
            }
        }

        fn delete_all(&self, service: &str) -> Result<(), String> {
            // `SecItemDelete` may take one matching item at a time: until none is left.
            for _ in 0..10_000 {
                let deleted = ItemSearchOptions::new()
                    .class(ItemClass::generic_password())
                    .service(service)
                    .delete();
                match deleted {
                    Ok(()) => continue,
                    Err(err) if err.code() == NOT_FOUND => return Ok(()),
                    Err(err) => return Err(err.to_string()),
                }
            }
            Ok(())
        }
    }
}

/// Elsewhere Flux never kept secrets in a keychain: nothing to move.
#[cfg(not(target_os = "macos"))]
mod platform {
    use std::sync::Arc;

    use super::Keychain;

    pub fn user() -> Option<Arc<dyn Keychain>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_app_bundle_counts() {
        assert!(in_app_bundle(Path::new(
            "/Applications/Flux.app/Contents/MacOS/flux"
        )));
        assert!(!in_app_bundle(Path::new("/src/flux/target/debug/flux")));
        assert!(!in_app_bundle(Path::new(
            "/src/flux/target/debug/deps/flux_plugin-0123456789abcdef"
        )));
        assert!(!in_app_bundle(Path::new("/Applications/Flux.app/flux")));
        // A test binary never gets the user's keychain.
        assert!(user().is_none());
    }
}
