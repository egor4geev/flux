//! `secrets`: the plugin's secrets, which Flux keeps encrypted ([`crate::secrets`]) under the
//! plugin's service ([`store::service`]), apart from its files. [`crate::install::uninstall`]
//! removes them with the plugin. Waiting for them (another Flux writing them) doesn't count toward
//! the plugin's time limit.

use std::time::Instant;

use crate::api::bindings::flux::plugin::secrets;
use crate::runtime::State;
use crate::secrets as store;

impl secrets::Host for State {
    fn get(&mut self, key: String) -> Option<String> {
        let service = store::service(&self.host.id);
        let started = Instant::now();
        let secret = store::get(&service, &key);
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
        let service = store::service(&self.host.id);
        let started = Instant::now();
        let result = match &value {
            Some(value) => store::set(&service, &key, value),
            None => store::delete(&service, &key),
        };
        self.host.waited += started.elapsed();
        result.map_err(|err| format!("Couldn't save the secret \"{key}\": {err}"))
    }
}
