//! Plugins without UI (stage 8, ADR-029): WebAssembly components of the `flux:plugin` world
//! (`wit/v0.3/flux-plugin.wit`; plugins built for 0.2 — `wit/v0.2`, [`compat`]) run by wasmtime,
//! each on its own thread.
//!
//! - [`manifest`] — `flux-plugin.toml`: who the plugin is, its permissions and what it adds
//!   (commands, tool windows, status bar items, settings; with 8.3 — languages, grammars, language
//!   servers, color themes, file icons, without code).
//! - [`catalog`] — the catalog (Marketplace, stage 8.3): the index of published plugins, packages,
//!   updates, suggestions for files.
//! - [`registry`] — where plugins come from: bundled into Flux, installed, under development.
//! - [`install`] — installing from disk and removing.
//! - [`runtime`] — an instance of a plugin: its thread, calls into it, its calls to the window
//!   ([`runtime::PluginMessage`]); limits on time and memory; the sandbox. `host` answers the
//!   API's functions on the plugin's side of the thread: what needs the window becomes a message,
//!   the rest (the network, the server, programs, timers, secrets) is done right there.
//! - [`secrets`] — the plugins' secrets, kept by Flux encrypted.
//! - [`api`] — the types of the WIT interfaces, shared with the window.
//! - [`log`], [`locales`], [`dev`], [`paths`] — the plugin's log, its translations, building a
//!   plugin under development, where everything lives on disk.

pub mod api;
pub mod catalog;
pub(crate) mod compat;
pub mod dev;
mod host;
pub mod install;
pub mod locales;
pub mod log;
pub mod manifest;
pub mod paths;
pub mod registry;
pub mod runtime;
pub(crate) mod secrets;
#[cfg(test)]
pub(crate) mod tests;

/// The user's login shell environment (a terminal's `PATH`), read once per process.
pub use host::login_env;

/// The version of the plugin API (`flux:plugin@0.3.0`) a manifest names in `api`: what plugins are
/// built for now (the SDK, the template, the bundled plugins).
pub const API_VERSION: &str = "0.3";

/// The versions this Flux runs, oldest first: a plugin built for an older one keeps working (its
/// calls are answered through `wit/v0.2`), and a plugin without code (a language, a theme) names
/// any of them.
pub const SUPPORTED_API_VERSIONS: &[&str] = &["0.2", API_VERSION];

/// This Flux runs plugins built for `api`.
pub fn supports_api(api: &str) -> bool {
    SUPPORTED_API_VERSIONS.contains(&api)
}

/// A plugin's secret, as its `secrets.get` reads it: Settings keeps a plugin's `secret` settings
/// there (API 0.3). It may wait for another Flux writing them — call it off the UI thread.
pub fn plugin_secret(plugin: &str, key: &str) -> Result<Option<String>, String> {
    secrets::get(&secrets::service(plugin), key)
}

/// Saves a plugin's secret (none removes it), as its `secrets.set` does.
pub fn set_plugin_secret(plugin: &str, key: &str, value: Option<&str>) -> Result<(), String> {
    let service = secrets::service(plugin);
    match value {
        Some(value) => secrets::set(&service, key, value),
        None => secrets::delete(&service, key),
    }
}
