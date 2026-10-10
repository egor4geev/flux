//! Plugins without UI (stage 8, ADR-029): WebAssembly components of the `flux:plugin` world
//! (`wit/flux-plugin.wit`) run by wasmtime, each on its own thread.
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
//! - [`api`] — the types of the WIT interfaces, shared with the window.
//! - [`log`], [`locales`], [`dev`], [`paths`] — the plugin's log, its translations, building a
//!   plugin under development, where everything lives on disk.

pub mod api;
pub mod catalog;
pub mod dev;
mod host;
pub mod install;
pub(crate) mod keychain;
pub mod locales;
pub mod log;
pub mod manifest;
pub mod paths;
pub mod registry;
pub mod runtime;
#[cfg(test)]
pub(crate) mod tests;

/// The version of the plugin API (`flux:plugin@0.2.0`) a manifest names in `api`.
pub const API_VERSION: &str = "0.2";
