//! The Rust SDK for Flux plugins (docs/plugins.md). A plugin is a `cdylib` built for
//! `wasm32-wasip2`; it implements [`Plugin`] and registers it with [`register_plugin!`]:
//!
//! ```ignore
//! use flux_plugin_api::{CommandContext, Plugin, register_plugin, notify, tr};
//!
//! struct Hello;
//!
//! impl Plugin for Hello {
//!     fn new() -> Self {
//!         Hello
//!     }
//!
//!     fn run_command(&mut self, command: &str, _context: &CommandContext) {
//!         if command == "hello" {
//!             notify::info(&tr("Hello from a plugin"));
//!         }
//!     }
//! }
//!
//! register_plugin!(Hello);
//! ```
//!
//! [`host`] has every interface of the API as generated from `crates/flux-plugin/wit/flux-plugin.wit`
//! (the reference); the rest are shortcuts for the common cases: [`view`] builds a tool window's
//! content, [`notify`] and [`dialog`] talk to the user, [`editor`] reads the documents,
//! [`storage`] keeps the plugin's data, [`log`] writes its log, [`tr`] / [`trf`] translate and
//! [`setting`] reads its settings; [`CommandContext`] says what a command acts on.
//!
//! Beyond the window, as the manifest's permissions allow: [`http`] — requests to web services,
//! [`server`] — the plugin's server on 127.0.0.1, [`process`] — programs, [`terminal`] — terminal
//! tabs, [`review`] — edits the user reviews in a diff, [`git`] — the repositories, [`diagnostics`]
//! — problems in files; and for every plugin: [`secrets`] in the keychain, [`timers`],
//! [`system`] (the browser, the clipboard, the home folder), [`status`] items. Guide:
//! `docs/plugins.md`.

pub mod diagnostics;
pub mod git;
pub mod http;
pub mod process;
pub mod review;
pub mod secrets;
pub mod server;
pub mod status;
pub mod system;
pub mod terminal;
pub mod timers;
pub mod view;

mod context;
mod text;
mod url;

pub use text::{offset, slice};

/// The bindings generated from `crates/flux-plugin/wit/flux-plugin.wit`.
#[allow(clippy::too_many_arguments)]
pub mod bindings {
    wit_bindgen::generate!({
        path: "../flux-plugin/wit",
        world: "plugin",
        pub_export_macro: true,
        export_macro_name: "export_plugin",
        default_bindings_module: "flux_plugin_api::bindings",
        // Values compare: events, positions, views (a plugin can skip sending an unchanged one).
        additional_derives: [PartialEq],
    });
}

/// The interfaces of the API, as generated: one module per WIT interface (`host::editors::open`,
/// `host::ui::set_view`…).
pub mod host {
    pub use crate::bindings::flux::plugin::{
        commands, diagnostics, dialogs, editors, events, git, http, i18n, log, notifications,
        process, project, review, secrets, server, settings, status_bar, storage, system,
        terminal, timers, types, ui,
    };
}

pub use bindings::Event;
pub use bindings::flux::plugin::events::{UiEvent, UiInput};
pub use bindings::flux::plugin::types::{
    CommandContext, CommandSource, EditorInfo, Position, Range,
};

use std::cell::RefCell;
use std::fmt::Display;

/// A plugin. Flux calls it on the plugin's own thread, one call at a time; each call should
/// return quickly (a call longer than ten seconds stops the plugin).
pub trait Plugin: 'static {
    /// Made before the first call.
    fn new() -> Self
    where
        Self: Sized;

    /// The plugin was loaded: the window opened, or the user turned the plugin on.
    fn activate(&mut self) {}

    /// The plugin is about to be unloaded.
    fn deactivate(&mut self) {}

    /// One of the commands of the manifest was run: `context` says from where (the palette, a
    /// menu…) and what it acts on (the document, its selections, the files).
    fn run_command(&mut self, command: &str, context: &CommandContext) {
        let _ = (command, context);
    }

    /// Something happened in the window, or an answer to a question came.
    fn on_event(&mut self, event: Event) {
        let _ = event;
    }
}

thread_local! {
    static PLUGIN: RefCell<Option<Box<dyn std::any::Any>>> = const { RefCell::new(None) };
}

/// Calls `f` with the plugin, made on the first call. Used by [`register_plugin!`].
#[doc(hidden)]
pub fn __with_plugin<P: Plugin>(f: impl FnOnce(&mut P)) {
    PLUGIN.with(|cell| {
        let mut slot = cell.borrow_mut();
        let plugin = slot.get_or_insert_with(|| Box::new(P::new()));
        if let Some(plugin) = plugin.downcast_mut::<P>() {
            f(plugin);
        }
    });
}

/// Exports the plugin type to Flux: `register_plugin!(MyPlugin);` once in the crate.
#[macro_export]
macro_rules! register_plugin {
    ($plugin:ty) => {
        struct __FluxPluginExport;

        impl $crate::bindings::Guest for __FluxPluginExport {
            fn activate() {
                $crate::__with_plugin::<$plugin>(|plugin| $crate::Plugin::activate(plugin));
            }

            fn deactivate() {
                $crate::__with_plugin::<$plugin>(|plugin| $crate::Plugin::deactivate(plugin));
            }

            fn run_command(command: String, context: $crate::CommandContext) {
                $crate::__with_plugin::<$plugin>(|plugin| {
                    $crate::Plugin::run_command(plugin, &command, &context)
                });
            }

            fn on_event(event: $crate::Event) {
                $crate::__with_plugin::<$plugin>(|plugin| $crate::Plugin::on_event(plugin, event));
            }
        }

        $crate::bindings::export_plugin!(__FluxPluginExport with_types_in $crate::bindings);
    };
}

/// The text in the interface language, from the plugin's `locales/<language>.toml`.
pub fn tr(text: &str) -> String {
    host::i18n::translate(text)
}

/// A translated template with `{0}`, `{1}`… filled in: `trf("{0} items", &[&count])`.
pub fn trf(template: &str, args: &[&dyn Display]) -> String {
    fill(&tr(template), args)
}

/// Puts the arguments in place of `{0}`, `{1}`…; a placeholder without an argument stays.
fn fill(template: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}').and_then(|close| {
            let index: usize = after[..close].parse().ok()?;
            Some((close, args.get(index)?))
        }) {
            Some((close, arg)) => {
                out.push_str(&arg.to_string());
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The value of one of the plugin's settings (`[[settings]]` of the manifest): the user's, or
/// the default; none if the key is unknown or the value doesn't fit `T`.
pub fn setting<T: serde::de::DeserializeOwned>(key: &str) -> Option<T> {
    serde_json::from_str(&host::settings::get(key)?).ok()
}

/// Lines of the plugin's log: Settings → Plugins → the plugin → Log, and a file on disk. What the
/// plugin prints (`println!`, `eprintln!`, a panic's message) goes there too.
pub mod log {
    use crate::host::log::{Level, write};

    /// Details for the plugin's author.
    pub fn debug(message: &str) {
        write(Level::Debug, message);
    }

    /// What the plugin does, for its author.
    pub fn info(message: &str) {
        write(Level::Info, message);
    }

    /// Something went wrong, and the plugin coped.
    pub fn warn(message: &str) {
        write(Level::Warn, message);
    }

    /// Something went wrong, and the plugin couldn't do what it was asked.
    pub fn error(message: &str) {
        write(Level::Error, message);
    }
}

/// Notifications in the plugin's group of Settings → Notifications: `notify::info("Done")`, or
/// [`notify::Notice`] for a body, actions and a sticky card. Progress, updates and removal by id:
/// [`host::notifications`].
pub mod notify {
    use crate::host::notifications::{self, Action, Notification, NotificationKind};

    fn send(kind: NotificationKind, title: &str, body: Option<&str>) -> u64 {
        notifications::notify(&Notification {
            kind,
            title: title.to_string(),
            body: body.map(str::to_string),
            actions: Vec::new(),
            sticky: false,
        })
    }

    /// An information notification; returns its id.
    pub fn info(title: &str) -> u64 {
        send(NotificationKind::Info, title, None)
    }

    /// Something was done.
    pub fn success(title: &str) -> u64 {
        send(NotificationKind::Success, title, None)
    }

    /// A warning: something needs the user's attention.
    pub fn warning(title: &str, body: Option<&str>) -> u64 {
        send(NotificationKind::Warning, title, body)
    }

    /// An error: its card stays until closed.
    pub fn error(title: &str, body: Option<&str>) -> u64 {
        send(NotificationKind::Error, title, body)
    }

    /// A notification with more than a title:
    ///
    /// ```ignore
    /// let id = Notice::warning("Index is out of date")
    ///     .body("12 files changed since the last run.")
    ///     .action("Rebuild", "rebuild")
    ///     .send();
    /// ```
    #[derive(Debug, Clone)]
    pub struct Notice(Notification);

    impl Notice {
        /// A notification of `kind` with `title`.
        pub fn new(kind: NotificationKind, title: &str) -> Self {
            Notice(Notification {
                kind,
                title: title.to_string(),
                body: None,
                actions: Vec::new(),
                sticky: false,
            })
        }

        /// Information.
        pub fn info(title: &str) -> Self {
            Self::new(NotificationKind::Info, title)
        }

        /// Something was done.
        pub fn success(title: &str) -> Self {
            Self::new(NotificationKind::Success, title)
        }

        /// Something needs the user's attention.
        pub fn warning(title: &str) -> Self {
            Self::new(NotificationKind::Warning, title)
        }

        /// An error: its card stays until closed.
        pub fn error(title: &str) -> Self {
            Self::new(NotificationKind::Error, title)
        }

        /// The text under the title.
        pub fn body(mut self, body: &str) -> Self {
            self.0.body = Some(body.to_string());
            self
        }

        /// A button that runs one of the plugin's commands (`[[commands]]` of the manifest).
        pub fn action(mut self, label: &str, command: &str) -> Self {
            self.0.actions.push(Action {
                label: label.to_string(),
                command: command.to_string(),
            });
            self
        }

        /// The card stays until closed.
        pub fn sticky(mut self) -> Self {
            self.0.sticky = true;
            self
        }

        /// Shows the notification; returns its id.
        pub fn send(&self) -> u64 {
            notifications::notify(&self.0)
        }

        /// Replaces the notification `id` with this one.
        pub fn update(&self, id: u64) {
            notifications::update(id, &self.0);
        }
    }
}

/// Questions in Flux dialogs. A question returns its id at once; the answer comes later as
/// `Event::DialogAnswered((id, button))` or `Event::TextAnswered((id, text))`:
///
/// ```ignore
/// self.asked = Some(Ask::warning("Delete the cache?").danger("Delete").cancel("Cancel").send());
/// // …
/// Event::DialogAnswered((id, Some(0))) if Some(id) == self.asked => self.delete_cache(),
/// ```
pub mod dialog {
    use crate::host::dialogs::{
        self, ButtonRole, DialogButton, DialogLevel, Question, TextQuestion,
    };

    /// A question with buttons; the answer is the index of the pressed one.
    #[derive(Debug, Clone)]
    pub struct Ask(Question);

    impl Ask {
        /// A question of `level` with `title`.
        pub fn new(level: DialogLevel, title: &str) -> Self {
            Ask(Question {
                level,
                title: title.to_string(),
                message: None,
                details: None,
                buttons: Vec::new(),
            })
        }

        /// A question for information.
        pub fn info(title: &str) -> Self {
            Self::new(DialogLevel::Info, title)
        }

        /// A question with a risk: overwriting, losing changes.
        pub fn warning(title: &str) -> Self {
            Self::new(DialogLevel::Warning, title)
        }

        /// A question about something that can't be undone.
        pub fn critical(title: &str) -> Self {
            Self::new(DialogLevel::Critical, title)
        }

        /// The text under the title.
        pub fn message(mut self, message: &str) -> Self {
            self.0.message = Some(message.to_string());
            self
        }

        /// Monospace details under a disclosure, with Copy: command output, a stack trace.
        pub fn details(mut self, details: &str) -> Self {
            self.0.details = Some(details.to_string());
            self
        }

        /// A button with its role: the role picks its place, style and key.
        pub fn button(mut self, label: &str, role: ButtonRole) -> Self {
            self.0.buttons.push(DialogButton {
                label: label.to_string(),
                role,
            });
            self
        }

        /// The default button (↵).
        pub fn primary(self, label: &str) -> Self {
            self.button(label, ButtonRole::Primary)
        }

        /// An ordinary button.
        pub fn normal(self, label: &str) -> Self {
            self.button(label, ButtonRole::Normal)
        }

        /// A destructive button: never the default.
        pub fn danger(self, label: &str) -> Self {
            self.button(label, ButtonRole::Danger)
        }

        /// The button Esc presses.
        pub fn cancel(self, label: &str) -> Self {
            self.button(label, ButtonRole::Cancel)
        }

        /// Shows the dialog; returns the question's id.
        pub fn send(&self) -> u64 {
            dialogs::ask(&self.0)
        }
    }

    /// A question with a text field; the answer is the text, or none when cancelled.
    #[derive(Debug, Clone)]
    pub struct AskText(TextQuestion);

    impl AskText {
        /// `confirm` — the confirm button's label: "Create", "Rename".
        pub fn new(title: &str, confirm: &str) -> Self {
            AskText(TextQuestion {
                title: title.to_string(),
                message: None,
                text: String::new(),
                placeholder: None,
                confirm: confirm.to_string(),
            })
        }

        /// The text under the title.
        pub fn message(mut self, message: &str) -> Self {
            self.0.message = Some(message.to_string());
            self
        }

        /// The initial text, selected.
        pub fn text(mut self, text: &str) -> Self {
            self.0.text = text.to_string();
            self
        }

        /// Gray text in the empty field.
        pub fn placeholder(mut self, placeholder: &str) -> Self {
            self.0.placeholder = Some(placeholder.to_string());
            self
        }

        /// Shows the dialog; returns the question's id.
        pub fn send(&self) -> u64 {
            dialogs::ask_text(&self.0)
        }
    }
}

/// The window's documents, the common cases of [`host::editors`].
pub mod editor {
    use crate::host::editors;
    use crate::{EditorInfo, Range};

    /// The active tab's document; none when the active tab is not a document.
    pub fn active() -> Option<EditorInfo> {
        editors::active()
    }

    /// The active document and its text.
    pub fn active_text() -> Option<(EditorInfo, String)> {
        let editor = editors::active()?;
        let text = editors::text(editor.id)?;
        Some((editor, text))
    }

    /// The text of the active document's primary selection; none without a document, empty for
    /// a cursor.
    pub fn selected_text() -> Option<String> {
        let (editor, text) = active_text()?;
        let selection = editors::selections(editor.id).into_iter().next()?;
        Some(crate::slice(&text, selection).to_string())
    }

    /// Opens a file in a tab (or goes to its tab) and selects the span, if given.
    pub fn open(path: &str, selection: Option<Range>) -> Result<u64, String> {
        editors::open(path, selection)
    }
}

/// The plugin's own data, kept between launches: values in a small store (JSON), and a folder
/// for files ([`storage::dir`]).
pub mod storage {
    use crate::host::storage;

    /// A value kept under `key`; none when there is none or it doesn't fit `T`.
    pub fn get<T: serde::de::DeserializeOwned>(key: &str) -> Option<T> {
        serde_json::from_str(&storage::get(key)?).ok()
    }

    /// Keeps a value under `key`.
    pub fn set<T: serde::Serialize + ?Sized>(key: &str, value: &T) {
        if let Ok(json) = serde_json::to_string(value) {
            storage::set(key, Some(&json));
        }
    }

    /// Forgets the value under `key`.
    pub fn remove(key: &str) {
        storage::set(key, None);
    }

    /// The plugin's folder (absolute), readable and writable through `std::fs`.
    pub fn dir() -> String {
        storage::data_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::fill;

    #[test]
    fn templates() {
        assert_eq!(fill("{0} of {1}", &[&3, &"5"]), "3 of 5");
        assert_eq!(fill("{x} {9}", &[&1]), "{x} {9}");
    }
}
