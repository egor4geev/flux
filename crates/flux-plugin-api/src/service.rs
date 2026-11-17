//! The plugin's service (API 0.5). With `service = true` in the manifest, Flux runs one more
//! instance of the plugin for the whole app — the service — next to the instance in each window,
//! from the first window that runs the plugin until the plugin is turned off or Flux quits. The
//! service keeps what must be one (a connection, a database, a program); the windows' instances
//! show things and talk to it by messages, which come as `Event::ServiceMessage((from, text))`,
//! in order:
//!
//! ```ignore
//! fn on_event(&mut self, event: &Event) {
//!     match event {
//!         // In the service: a window asks.
//!         Event::ServiceMessage((Some(window), text)) => {
//!             let _ = service::send_to(*window, &format!("pong {text}"));
//!         }
//!         // In a window: the service answers or tells everyone.
//!         Event::ServiceMessage((None, text)) => notify::info(text),
//!         // In a window: the service (re)started and knows nothing of the window yet.
//!         Event::ServiceStarted => {
//!             let _ = service::send("hello");
//!         }
//!         _ => {}
//!     }
//! }
//! ```
//!
//! The service has no window: documents, the project, tool windows, questions and the rest of a
//! window's interfaces fail or do nothing there (a line in the plugin's log says so); its
//! notifications go to the window the user works in. It hears `Event::WindowOpened(id)` and
//! `Event::WindowClosed(id)`; [`windows`] lists the windows running the plugin (in `activate`
//! too). Messages are text: [`send_json`] and [`parse`] carry a value as JSON.

use crate::host::service as raw;

/// This instance is the service.
pub fn is_service() -> bool {
    raw::is_service()
}

/// From a window's instance: sends a message to the service; it comes there as
/// `ServiceMessage((Some(this window's id), message))`. Fails when the service isn't running (yet:
/// `ServiceStarted` says when it is).
pub fn send(message: &str) -> Result<(), String> {
    raw::send(None, message)
}

/// From the service: sends a message to a window's instance; it comes there as
/// `ServiceMessage((None, message))`. Fails when the window is gone.
pub fn send_to(window: u64, message: &str) -> Result<(), String> {
    raw::send(Some(window), message)
}

/// From the service: sends a message to every window's instance.
pub fn broadcast(message: &str) -> Result<(), String> {
    raw::send(None, message)
}

/// For the service: the windows running the plugin, by id.
pub fn windows() -> Vec<u64> {
    raw::windows()
}

/// [`send`] with a value as JSON.
pub fn send_json<T: serde::Serialize + ?Sized>(value: &T) -> Result<(), String> {
    send(&to_json(value)?)
}

/// [`send_to`] with a value as JSON.
pub fn send_json_to<T: serde::Serialize + ?Sized>(window: u64, value: &T) -> Result<(), String> {
    send_to(window, &to_json(value)?)
}

/// [`broadcast`] with a value as JSON.
pub fn broadcast_json<T: serde::Serialize + ?Sized>(value: &T) -> Result<(), String> {
    broadcast(&to_json(value)?)
}

/// A message sent as JSON, read back; none when it isn't a `T`.
pub fn parse<T: serde::de::DeserializeOwned>(message: &str) -> Option<T> {
    serde_json::from_str(message).ok()
}

fn to_json<T: serde::Serialize + ?Sized>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|err| err.to_string())
}
