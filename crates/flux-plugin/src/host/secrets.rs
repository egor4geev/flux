//! `secrets`: the plugin's secrets in the macOS keychain — a generic password per key, under the
//! plugin's service ([`keychain::service`]) — rather than in files. [`crate::install::uninstall`]
//! removes them with the plugin. A locked keychain asks the user to unlock it; the wait doesn't
//! count toward the plugin's time limit.

use std::time::Instant;

use crate::api::bindings::flux::plugin::secrets;
use crate::keychain;
use crate::runtime::State;

impl secrets::Host for State {
    fn get(&mut self, key: String) -> Option<String> {
        let service = keychain::service(&self.host.id);
        let started = Instant::now();
        let secret = keychain::get(&service, &key);
        self.host.waited += started.elapsed();
        match secret {
            Ok(secret) => secret,
            Err(err) => {
                self.host
                    .warn(&format!("Couldn't read the secret \"{key}\": {err}"));
                None
            }
        }
    }

    fn set(&mut self, key: String, value: Option<String>) -> Result<(), String> {
        if key.is_empty() {
            return Err("A secret needs a key".into());
        }
        let service = keychain::service(&self.host.id);
        let started = Instant::now();
        let result = match &value {
            Some(value) => keychain::set(&service, &key, value),
            None => keychain::delete(&service, &key),
        };
        self.host.waited += started.elapsed();
        result.map_err(|err| format!("Couldn't save the secret \"{key}\": {err}"))
    }
}
