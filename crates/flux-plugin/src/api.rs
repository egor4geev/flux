//! The types of the plugin API, generated from `wit/flux-plugin.wit`: the window builds and reads
//! them (a view, a notification, an event) without knowing about WebAssembly.

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        path: "wit",
        world: "plugin",
        additional_derives: [PartialEq],
    });
}

pub use bindings::flux::plugin::{
    diagnostics, dialogs, editors, events, git, http, i18n, log, notifications, process, project,
    review, secrets, server, settings, status_bar, storage, system, terminal, timers, types, ui,
};
