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
    dialogs, editors, events, i18n, log, notifications, project, settings, status_bar, storage,
    types, ui,
};
