//! The types of the plugin API, generated from `wit/v0.5/flux-plugin.wit` (the current version):
//! the window builds and reads them (a view, a notification, an event) without knowing about
//! WebAssembly. Plugins built for an older version are answered by the same code
//! (`wit/v0.2`, `wit/v0.3`) through conversions to these types.

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        path: "wit/v0.5",
        world: "plugin",
        additional_derives: [PartialEq],
    });
}

pub use bindings::flux::plugin::{
    claude, contexts, diagnostics, dialogs, editors, events, git, http, i18n, links, log,
    notifications, picker, process, project, review, secrets, server, service, settings,
    status_bar, storage, system, terminal, timers, tools, types, ui,
};
