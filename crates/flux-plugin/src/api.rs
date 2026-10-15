//! The types of the plugin API, generated from `wit/v0.3/flux-plugin.wit` (the current version):
//! the window builds and reads them (a view, a notification, an event) without knowing about
//! WebAssembly. Plugins built for an older version (`wit/v0.2`) are answered by the same code
//! through conversions to these types.

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        path: "wit/v0.3",
        world: "plugin",
        additional_derives: [PartialEq],
    });
}

pub use bindings::flux::plugin::{
    claude, contexts, diagnostics, dialogs, editors, events, git, http, i18n, links, log,
    notifications, picker, process, project, review, secrets, server, settings, status_bar,
    storage, system, terminal, timers, types, ui,
};
