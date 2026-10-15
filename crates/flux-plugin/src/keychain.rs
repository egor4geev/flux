//! The plugins' secrets in the macOS keychain (`secrets` interface): a generic password per key,
//! under a service of the plugin's own ([`service`]). Read and written by the plugin's `secrets`;
//! all of a plugin's are removed when it is uninstalled ([`crate::install::uninstall`]).
//!
//! `FLUX_PLUGIN_SECRETS_FILE=<file>` keeps them in a JSON file instead (tests, scenarios): a
//! rebuilt unsigned Flux isn't on a keychain item's access list, and macOS would ask the user —
//! the author working next to the run — for the keychain's password.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// The secrets of every plugin by service, then account.
type Secrets = BTreeMap<String, BTreeMap<String, String>>;

/// The file of `FLUX_PLUGIN_SECRETS_FILE`, if set.
fn secrets_file() -> Option<PathBuf> {
    std::env::var_os("FLUX_PLUGIN_SECRETS_FILE")
        .filter(|file| !file.is_empty())
        .map(PathBuf::from)
}

/// Reads and changes the secrets file under a lock (the plugins' threads share it).
fn with_file<T>(file: PathBuf, change: impl FnOnce(&mut Secrets) -> T) -> Result<T, String> {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut secrets: Secrets = match std::fs::read(&file) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|err| err.to_string())?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Secrets::new(),
        Err(err) => return Err(err.to_string()),
    };
    let before = secrets.clone();
    let result = change(&mut secrets);
    if secrets != before {
        let text = serde_json::to_vec_pretty(&secrets).map_err(|err| err.to_string())?;
        std::fs::write(&file, text).map_err(|err| err.to_string())?;
    }
    Ok(result)
}

pub fn get(service: &str, account: &str) -> Result<Option<String>, String> {
    match secrets_file() {
        Some(file) => with_file(file, |secrets| {
            secrets
                .get(service)
                .and_then(|accounts| accounts.get(account))
                .cloned()
        }),
        None => platform::get(service, account),
    }
}

pub fn set(service: &str, account: &str, secret: &str) -> Result<(), String> {
    match secrets_file() {
        Some(file) => with_file(file, |secrets| {
            secrets
                .entry(service.to_string())
                .or_default()
                .insert(account.to_string(), secret.to_string());
        }),
        None => platform::set(service, account, secret),
    }
}

pub fn delete(service: &str, account: &str) -> Result<(), String> {
    match secrets_file() {
        Some(file) => with_file(file, |secrets| {
            if let Some(accounts) = secrets.get_mut(service) {
                accounts.remove(account);
            }
        }),
        None => platform::delete(service, account),
    }
}

/// Deletes every secret of the service.
pub fn delete_all(service: &str) -> Result<(), String> {
    match secrets_file() {
        Some(file) => with_file(file, |secrets| {
            secrets.remove(service);
        }),
        None => platform::delete_all(service),
    }
}

/// The keychain service of a plugin's secrets.
pub fn service(plugin: &str) -> String {
    format!("Flux plugin {plugin}")
}

#[cfg(target_os = "macos")]
mod platform {
    use security_framework::item::{ItemClass, ItemSearchOptions};
    use security_framework::passwords;

    /// `errSecItemNotFound`.
    const NOT_FOUND: i32 = -25300;

    pub fn get(service: &str, account: &str) -> Result<Option<String>, String> {
        match passwords::get_generic_password(service, account) {
            Ok(secret) => String::from_utf8(secret)
                .map(Some)
                .map_err(|_| "it isn't text".to_string()),
            Err(err) if err.code() == NOT_FOUND => Ok(None),
            Err(err) => Err(err.to_string()),
        }
    }

    pub fn set(service: &str, account: &str, secret: &str) -> Result<(), String> {
        passwords::set_generic_password(service, account, secret.as_bytes())
            .map_err(|err| err.to_string())
    }

    pub fn delete(service: &str, account: &str) -> Result<(), String> {
        match passwords::delete_generic_password(service, account) {
            Err(err) if err.code() != NOT_FOUND => Err(err.to_string()),
            _ => Ok(()),
        }
    }

    /// Deletes every generic password of the service.
    pub fn delete_all(service: &str) -> Result<(), String> {
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

/// Elsewhere there is no keychain yet: secrets can't be kept.
#[cfg(not(target_os = "macos"))]
mod platform {
    const UNSUPPORTED: &str = "secrets are kept only on macOS for now";

    pub fn get(_: &str, _: &str) -> Result<Option<String>, String> {
        Err(UNSUPPORTED.into())
    }

    pub fn set(_: &str, _: &str, _: &str) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }

    pub fn delete(_: &str, _: &str) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }

    pub fn delete_all(_: &str) -> Result<(), String> {
        Ok(())
    }
}
