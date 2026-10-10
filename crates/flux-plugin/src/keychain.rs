//! The plugins' secrets in the macOS keychain (`secrets` interface): a generic password per key,
//! under a service of the plugin's own ([`service`]). Read and written by the plugin's `secrets`;
//! all of a plugin's are removed when it is uninstalled ([`crate::install::uninstall`]).

pub use platform::{delete, delete_all, get, set};

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
