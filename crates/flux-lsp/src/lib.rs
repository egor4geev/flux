//! Language Server Protocol client without UI.
//!
//! Starts language servers as child processes and talks JSON-RPC to them over stdio on background
//! threads; responses and server events come out as `futures` types, so any executor (gpui in
//! flux-app) can await them. Positions on our side are character indices in a [`Rope`], as
//! everywhere in flux; on the wire they are UTF-16 (the only encoding we advertise).
//!
//! [`Rope`]: flux_core::Rope

pub mod config;
pub mod edit;
pub mod install;
pub mod position;
pub mod snippet;
pub mod sync;

mod client;
mod transport;

pub use client::{LanguageServer, RequestError, ServerEvent};
pub use config::{
    Install, ServerConfig, default_servers, language_id, server_for_path, servers_for_path,
};
pub use lsp_types;
pub use position::Lines;
