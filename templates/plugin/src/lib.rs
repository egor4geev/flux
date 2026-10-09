//! A plugin to start from: a command that shows a notification with an action, a tool window that
//! lists the open documents (and opens the one picked), a setting. Build it with
//! `cargo build --release --target wasm32-wasip2`; install the folder in Flux: Settings →
//! Plugins → ⚙ → Install Plugin from Disk… — Flux rebuilds and reloads it as it changes.

use flux_plugin_api::host::editors;
use flux_plugin_api::notify::Notice;
use flux_plugin_api::view::*;
use flux_plugin_api::{Event, Plugin, UiEvent, log, register_plugin, setting, tr, trf};

/// The tool window of the manifest.
const WINDOW: &str = "documents";

struct Hello {
    /// The tool window is on screen: only then the list is kept up to date.
    shown: bool,
}

impl Plugin for Hello {
    fn new() -> Self {
        Hello { shown: false }
    }

    fn activate(&mut self) {
        log::info("activated");
    }

    fn run_command(&mut self, command: &str) {
        match command {
            "hello" => {
                let greeting: String =
                    setting("greeting").unwrap_or_else(|| tr("Hello from a plugin!"));
                Notice::info(&greeting)
                    .action(&tr("Show Open Documents"), "show-documents")
                    .send();
            }
            "show-documents" => flux_plugin_api::host::ui::show(WINDOW),
            _ => {}
        }
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::ToolWindowShown(window) if window == WINDOW => {
                self.shown = true;
                render();
            }
            Event::ToolWindowHidden(window) if window == WINDOW => self.shown = false,
            Event::EditorOpened(_)
            | Event::EditorClosed(_)
            | Event::EditorSaved(_)
            | Event::EditorChanged(_)
            | Event::ActiveEditorChanged(_)
                if self.shown =>
            {
                render();
            }
            Event::Ui(input) if input.window == WINDOW => {
                match (input.element.as_str(), input.event) {
                    ("refresh", UiEvent::Clicked) => render(),
                    ("documents", UiEvent::Activated(key)) => {
                        if let Err(error) = editors::open(&key, None) {
                            log::warn(&error);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// The open documents as a list; a row's key is the document's path.
fn render() {
    let documents = editors::list();
    let mut list = Tree::new().empty_text(&tr("No open documents"));
    for document in &documents {
        let Some(path) = &document.path else {
            list.add(RowSpec::new("untitled", &tr("Untitled")));
            continue;
        };
        let name = path.rsplit('/').next().unwrap_or(path);
        let mut row = RowSpec::new(path, name)
            .icon(&format!("file:{name}"))
            .detail(&document.language);
        if document.modified {
            row = row.badge(&tr("modified"));
        }
        list.add(row);
    }
    set_view(
        WINDOW,
        column(
            "root",
            [
                toolbar(
                    "toolbar",
                    [icon_button("refresh", "refresh", &tr("Refresh"))],
                ),
                text(
                    "summary",
                    [span(&trf("Documents: {0}", &[&documents.len()])).tone(Tone::Dim)],
                ),
                list.into_element("documents"),
            ],
        ),
    );
}

register_plugin!(Hello);
