//! Plugins built for the API 0.2 (`wit/v0.2`, part 10.3): Flux runs them with the code of the
//! current version.
//!
//! - The interfaces 0.3 left as they were (the same functions with the same types — wasmtime
//!   checks a function's type by its shape, not by the package it comes from) are answered by the
//!   current implementations under the 0.2 names; what 0.3 added to them (`dialogs.ask-form`,
//!   `git.new-branch`…) is there too, and a 0.2 plugin simply doesn't import it.
//! - The two interfaces whose types grew have 0.2 implementations that convert and go on to the
//!   current ones: `notifications` (an action's argument) and `ui` (a multi-line field, a badge).
//! - The exports are called through the 0.2 bindings: `run-command`'s context loses what 0.3
//!   added, and an event 0.2 doesn't know isn't delivered.

use wasmtime::component::{HasSelf, Linker};

use crate::api::bindings::flux::plugin as current;
use crate::api::events::{Event, UiEvent, UiInput};
use crate::api::http::{HttpError, ResponseHead};
use crate::api::process::{OutputChannel, ProcessOutput};
use crate::api::review::ProposalOutcome;
use crate::api::server::{ServerRequest, WsMessage, WsOpen};
use crate::api::types::{CommandContext, CommandSource, EditorInfo, Position, Range};
use crate::api::{notifications, ui};
use crate::runtime::State;

/// The bindings of the API 0.2: the exports of a 0.2 component, and the two interfaces that need
/// converting.
pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        path: "wit/v0.2",
        world: "plugin",
    });
}

use bindings::flux::plugin as v2;

/// The version this module answers.
pub(crate) const API: &str = "0.2";

/// Adds the API 0.2 to the linker, next to the current version (the names differ by their
/// version: `flux:plugin/log@0.2.0`).
pub(crate) fn add_to_linker(linker: &mut Linker<State>) -> wasmtime::Result<()> {
    // The interfaces whose functions 0.3 didn't change: the current implementations under the
    // 0.2 names. `types` and `events` hold types only: their imports need no definition.
    macro_rules! unchanged {
        ($($interface:ident = $name:literal),* $(,)?) => {
            $(
                current::$interface::add_to_linker_instance::<State, HasSelf<State>>(
                    &mut linker.instance(concat!("flux:plugin/", $name, "@0.2.0"))?,
                    |state| state,
                )?;
            )*
        };
    }
    unchanged!(
        log = "log",
        i18n = "i18n",
        commands = "commands",
        dialogs = "dialogs",
        editors = "editors",
        project = "project",
        storage = "storage",
        settings = "settings",
        status_bar = "status-bar",
        http = "http",
        server = "server",
        process = "process",
        terminal = "terminal",
        review = "review",
        git = "git",
        diagnostics = "diagnostics",
        secrets = "secrets",
        timers = "timers",
        system = "system",
    );
    v2::notifications::add_to_linker::<State, HasSelf<State>>(linker, |state| state)?;
    v2::ui::add_to_linker::<State, HasSelf<State>>(linker, |state| state)?;
    Ok(())
}

/// The API version a component was built for, by the names of its imports
/// (`flux:plugin/log@0.2.0` → "0.2"); none when it imports nothing of the API.
pub(crate) fn built_for(
    component: &wasmtime::component::Component,
    engine: &wasmtime::Engine,
) -> Option<String> {
    component
        .component_type()
        .imports(engine)
        .find_map(|(name, _)| api_of_import(name))
}

/// "flux:plugin/log@0.2.0" → "0.2"; none for an import of something else (WASI).
fn api_of_import(name: &str) -> Option<String> {
    let rest = name.strip_prefix("flux:plugin/")?;
    let (_, version) = rest.split_once('@')?;
    let mut parts = version.split('.');
    let major = parts.next()?;
    let minor = parts.next()?;
    Some(format!("{major}.{minor}"))
}

// --- The interfaces that changed: 0.2 calls in, the current ones out ---

impl v2::notifications::Host for State {
    fn notify(&mut self, notification: v2::notifications::Notification) -> u64 {
        current::notifications::Host::notify(self, from_v2_notification(notification))
    }

    fn update(&mut self, id: u64, notification: v2::notifications::Notification) {
        current::notifications::Host::update(self, id, from_v2_notification(notification))
    }

    fn set_progress(&mut self, id: u64, value: Option<v2::notifications::Progress>) {
        let value = value.map(|progress| match progress {
            v2::notifications::Progress::Indeterminate => notifications::Progress::Indeterminate,
            v2::notifications::Progress::Fraction(done) => notifications::Progress::Fraction(done),
        });
        current::notifications::Host::set_progress(self, id, value)
    }

    fn expire(&mut self, id: u64) {
        current::notifications::Host::expire(self, id)
    }

    fn remove(&mut self, id: u64) {
        current::notifications::Host::remove(self, id)
    }
}

fn from_v2_notification(
    notification: v2::notifications::Notification,
) -> notifications::Notification {
    use v2::notifications::NotificationKind as Kind;
    notifications::Notification {
        kind: match notification.kind {
            Kind::Info => notifications::NotificationKind::Info,
            Kind::Success => notifications::NotificationKind::Success,
            Kind::Warning => notifications::NotificationKind::Warning,
            Kind::Error => notifications::NotificationKind::Error,
        },
        title: notification.title,
        body: notification.body,
        actions: notification
            .actions
            .into_iter()
            .map(|action| notifications::Action {
                label: action.label,
                command: action.command,
                argument: None,
            })
            .collect(),
        sticky: notification.sticky,
    }
}

impl v2::ui::Host for State {
    fn set_view(&mut self, window: String, view: v2::ui::View) {
        current::ui::Host::set_view(self, window, from_v2_view(view))
    }

    fn show(&mut self, window: String) {
        current::ui::Host::show(self, window)
    }

    fn hide(&mut self, window: String) {
        current::ui::Host::hide(self, window)
    }
}

fn from_v2_view(view: v2::ui::View) -> ui::View {
    ui::View {
        elements: view
            .elements
            .into_iter()
            .map(|element| ui::Element {
                id: element.id,
                kind: from_v2_kind(element.kind),
                children: element.children,
            })
            .collect(),
    }
}

fn from_v2_kind(kind: v2::ui::ElementKind) -> ui::ElementKind {
    use v2::ui::ElementKind as Kind;
    match kind {
        Kind::Column => ui::ElementKind::Column,
        Kind::Row => ui::ElementKind::Row,
        Kind::Toolbar => ui::ElementKind::Toolbar,
        Kind::Text(spans) => ui::ElementKind::Text(from_v2_spans(spans)),
        Kind::Markdown(text) => ui::ElementKind::Markdown(text),
        Kind::Button(button) => ui::ElementKind::Button(ui::ButtonSpec {
            label: button.label,
            icon: button.icon,
            tooltip: button.tooltip,
            primary: button.primary,
            enabled: button.enabled,
        }),
        Kind::TextField(field) => ui::ElementKind::TextField(ui::TextFieldSpec {
            text: field.text,
            placeholder: field.placeholder,
            code: field.code,
            multiline: false,
        }),
        Kind::Toggle(toggle) => ui::ElementKind::Toggle(ui::ToggleSpec {
            label: toggle.label,
            on: toggle.on,
            style: match toggle.style {
                v2::ui::ToggleStyle::Switch => ui::ToggleStyle::Switch,
                v2::ui::ToggleStyle::Checkbox => ui::ToggleStyle::Checkbox,
            },
        }),
        Kind::Tree(tree) => ui::ElementKind::Tree(ui::TreeSpec {
            rows: tree
                .rows
                .into_iter()
                .map(|row| ui::TreeRow {
                    key: row.key,
                    parent: row.parent,
                    icon: row.icon,
                    label: from_v2_spans(row.label),
                    detail: row.detail,
                    badge: row.badge,
                    expanded: row.expanded,
                    subtitle: None,
                    tags: Vec::new(),
                    actions: Vec::new(),
                })
                .collect(),
            empty_text: tree.empty_text,
            cards: false,
        }),
        Kind::Divider => ui::ElementKind::Divider,
        Kind::Spacer => ui::ElementKind::Spacer,
        Kind::Progress(done) => ui::ElementKind::Progress(done),
    }
}

fn from_v2_spans(spans: Vec<v2::ui::Span>) -> Vec<ui::Span> {
    use v2::ui::Tone;
    spans
        .into_iter()
        .map(|span| ui::Span {
            text: span.text,
            tone: match span.tone {
                Tone::Normal => ui::Tone::Normal,
                Tone::Muted => ui::Tone::Muted,
                Tone::Dim => ui::Tone::Dim,
                Tone::Accent => ui::Tone::Accent,
                Tone::Success => ui::Tone::Success,
                Tone::Warning => ui::Tone::Warning,
                Tone::Error => ui::Tone::Error,
            },
            bold: span.bold,
            code: span.code,
            highlight: span.highlight,
        })
        .collect()
}

// --- The exports: the current values in, 0.2 values out ---

/// A command's context for a 0.2 plugin: without the argument; a source 0.2 doesn't know (a
/// settings button — a 0.2 manifest has none) as the palette.
pub(crate) fn command_context(context: &CommandContext) -> v2::types::CommandContext {
    use v2::types::CommandSource as Source;
    v2::types::CommandContext {
        source: match context.source {
            CommandSource::Palette | CommandSource::Settings => Source::Palette,
            CommandSource::Keys => Source::Keys,
            CommandSource::EditorMenu => Source::EditorMenu,
            CommandSource::TreeMenu => Source::TreeMenu,
            CommandSource::TabMenu => Source::TabMenu,
            CommandSource::Notification => Source::Notification,
            CommandSource::StatusBar => Source::StatusBar,
        },
        editor: context.editor.as_ref().map(editor_info),
        selections: context.selections.iter().map(range).collect(),
        paths: context.paths.clone(),
    }
}

/// An event for a 0.2 plugin; none for one 0.2 doesn't know (the answer to a form, a picker's
/// query…) — such a plugin never asked for it.
pub(crate) fn event(event: &Event) -> Option<v2::events::Event> {
    use v2::events::Event as Old;
    Some(match event {
        Event::ActiveEditorChanged(editor) => {
            Old::ActiveEditorChanged(editor.as_ref().map(editor_info))
        }
        Event::EditorOpened(editor) => Old::EditorOpened(editor_info(editor)),
        Event::EditorClosed(id) => Old::EditorClosed(*id),
        Event::EditorChanged(id) => Old::EditorChanged(*id),
        Event::SelectionChanged(id) => Old::SelectionChanged(*id),
        Event::EditorSaved(editor) => Old::EditorSaved(editor_info(editor)),
        Event::ProjectChanged(root) => Old::ProjectChanged(root.clone()),
        Event::SettingsChanged => Old::SettingsChanged,
        Event::ToolWindowShown(window) => Old::ToolWindowShown(window.clone()),
        Event::ToolWindowHidden(window) => Old::ToolWindowHidden(window.clone()),
        Event::DialogAnswered(answer) => Old::DialogAnswered(*answer),
        Event::TextAnswered(answer) => Old::TextAnswered(answer.clone()),
        Event::Ui(input) => Old::Ui(ui_input(input)?),
        Event::Timer(id) => Old::Timer(*id),
        Event::HttpResponse(head) => Old::HttpResponse(response_head(head)),
        Event::HttpBody(chunk) => Old::HttpBody(chunk.clone()),
        Event::HttpDone((id, result)) => Old::HttpDone((*id, result.clone().map_err(http_error))),
        Event::ServerRequest(request) => Old::ServerRequest(server_request(request)),
        Event::StreamClosed(id) => Old::StreamClosed(*id),
        Event::WsOpened(open) => Old::WsOpened(ws_open(open)),
        Event::WsMessage((id, message)) => Old::WsMessage((*id, ws_message(message))),
        Event::WsClosed(id) => Old::WsClosed(*id),
        Event::ProcessOutput(output) => Old::ProcessOutput(process_output(output)),
        Event::ProcessExited(exit) => Old::ProcessExited(*exit),
        Event::TerminalExited(exit) => Old::TerminalExited(*exit),
        Event::TerminalClosed(id) => Old::TerminalClosed(*id),
        Event::ProposalAnswered((id, outcome)) => {
            Old::ProposalAnswered((*id, proposal_outcome(outcome)))
        }
        Event::GitChanged => Old::GitChanged,
        Event::DiagnosticsChanged(paths) => Old::DiagnosticsChanged(paths.clone()),
        // 0.3: what a 0.2 plugin can't have asked for.
        Event::FormAnswered(_)
        | Event::PickerQuery(_)
        | Event::PickerChosen(_)
        | Event::LinkActivated(_)
        | Event::LinkHovered(_)
        | Event::GitDone(_) => return None,
    })
}

fn editor_info(editor: &EditorInfo) -> v2::types::EditorInfo {
    v2::types::EditorInfo {
        id: editor.id,
        path: editor.path.clone(),
        language: editor.language.clone(),
        modified: editor.modified,
    }
}

fn range(range: &Range) -> v2::types::Range {
    let position = |position: &Position| v2::types::Position {
        line: position.line,
        column: position.column,
    };
    v2::types::Range {
        start: position(&range.start),
        end: position(&range.end),
    }
}

/// A window's input for a 0.2 plugin; none for a row's button (0.3: a 0.2 plugin has none).
fn ui_input(input: &UiInput) -> Option<v2::events::UiInput> {
    use v2::events::UiEvent as Old;
    Some(v2::events::UiInput {
        window: input.window.clone(),
        element: input.element.clone(),
        event: match &input.event {
            UiEvent::Clicked => Old::Clicked,
            UiEvent::Changed(text) => Old::Changed(text.clone()),
            UiEvent::Submitted(text) => Old::Submitted(text.clone()),
            UiEvent::Toggled(on) => Old::Toggled(*on),
            UiEvent::Selected(key) => Old::Selected(key.clone()),
            UiEvent::Activated(key) => Old::Activated(key.clone()),
            UiEvent::Expanded(expanded) => Old::Expanded(expanded.clone()),
            UiEvent::RowAction(_) => return None,
        },
    })
}

fn response_head(head: &ResponseHead) -> v2::http::ResponseHead {
    v2::http::ResponseHead {
        request: head.request,
        status: head.status,
        headers: head.headers.clone(),
    }
}

fn http_error(error: HttpError) -> v2::http::HttpError {
    use v2::http::HttpError as Old;
    match error {
        HttpError::Denied(text) => Old::Denied(text),
        HttpError::Invalid(text) => Old::Invalid(text),
        HttpError::Connection(text) => Old::Connection(text),
        HttpError::TimedOut => Old::TimedOut,
        HttpError::Cancelled => Old::Cancelled,
        HttpError::TooLarge => Old::TooLarge,
    }
}

fn server_request(request: &ServerRequest) -> v2::server::ServerRequest {
    v2::server::ServerRequest {
        id: request.id,
        method: request.method.clone(),
        path: request.path.clone(),
        headers: request.headers.clone(),
        body: request.body.clone(),
    }
}

fn ws_open(open: &WsOpen) -> v2::server::WsOpen {
    v2::server::WsOpen {
        connection: open.connection,
        path: open.path.clone(),
        headers: open.headers.clone(),
    }
}

fn ws_message(message: &WsMessage) -> v2::server::WsMessage {
    match message {
        WsMessage::Text(text) => v2::server::WsMessage::Text(text.clone()),
        WsMessage::Binary(bytes) => v2::server::WsMessage::Binary(bytes.clone()),
    }
}

fn process_output(output: &ProcessOutput) -> v2::process::ProcessOutput {
    v2::process::ProcessOutput {
        process: output.process,
        channel: match output.channel {
            OutputChannel::Stdout => v2::process::OutputChannel::Stdout,
            OutputChannel::Stderr => v2::process::OutputChannel::Stderr,
        },
        bytes: output.bytes.clone(),
    }
}

fn proposal_outcome(outcome: &ProposalOutcome) -> v2::review::ProposalOutcome {
    use v2::review::ProposalOutcome as Old;
    match outcome {
        ProposalOutcome::Accepted(text) => Old::Accepted(text.clone()),
        ProposalOutcome::Rejected => Old::Rejected,
        ProposalOutcome::Closed => Old::Closed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_of_an_import() {
        assert_eq!(
            api_of_import("flux:plugin/log@0.2.0").as_deref(),
            Some("0.2")
        );
        assert_eq!(
            api_of_import("flux:plugin/ui@0.3.1").as_deref(),
            Some("0.3")
        );
        assert_eq!(api_of_import("wasi:cli/stdout@0.2.6"), None);
        assert_eq!(api_of_import("flux:plugin/log"), None);
    }

    #[test]
    fn a_settings_button_is_the_palette_for_an_old_plugin() {
        let context = CommandContext {
            source: CommandSource::Settings,
            editor: None,
            selections: vec![Range {
                start: Position { line: 1, column: 2 },
                end: Position { line: 3, column: 4 },
            }],
            paths: vec!["/p/a.rs".into()],
            argument: Some("TRACK-1".into()),
        };
        let old = command_context(&context);
        assert!(matches!(old.source, v2::types::CommandSource::Palette));
        assert_eq!(old.selections[0].end.column, 4);
        assert_eq!(old.paths, vec!["/p/a.rs".to_string()]);
    }

    #[test]
    fn an_old_view_is_a_view_of_today() {
        let old = v2::ui::View {
            elements: vec![
                v2::ui::Element {
                    id: "root".into(),
                    kind: v2::ui::ElementKind::Column,
                    children: vec![1, 2],
                },
                v2::ui::Element {
                    id: "query".into(),
                    kind: v2::ui::ElementKind::TextField(v2::ui::TextFieldSpec {
                        text: "x".into(),
                        placeholder: Some("Find".into()),
                        code: true,
                    }),
                    children: Vec::new(),
                },
                v2::ui::Element {
                    id: "rows".into(),
                    kind: v2::ui::ElementKind::Tree(v2::ui::TreeSpec {
                        rows: vec![v2::ui::TreeRow {
                            key: "a".into(),
                            parent: None,
                            icon: None,
                            label: vec![v2::ui::Span {
                                text: "A".into(),
                                tone: v2::ui::Tone::Warning,
                                bold: true,
                                code: false,
                                highlight: false,
                            }],
                            detail: Some("1".into()),
                            badge: None,
                            expanded: true,
                        }],
                        empty_text: None,
                    }),
                    children: Vec::new(),
                },
            ],
        };
        let view = from_v2_view(old);
        assert_eq!(view.elements[0].children, vec![1, 2]);
        assert_eq!(
            view.elements[1].kind,
            ui::ElementKind::TextField(ui::TextFieldSpec {
                text: "x".into(),
                placeholder: Some("Find".into()),
                code: true,
                multiline: false,
            })
        );
        match &view.elements[2].kind {
            ui::ElementKind::Tree(tree) => {
                assert_eq!(tree.rows[0].label[0].tone, ui::Tone::Warning);
                assert_eq!(tree.rows[0].detail.as_deref(), Some("1"));
            }
            other => panic!("expected a tree, got {other:?}"),
        }
    }

    #[test]
    fn new_events_dont_reach_an_old_plugin() {
        assert!(event(&Event::PickerChosen((1, None))).is_none());
        assert!(event(&Event::GitDone((1, Ok(())))).is_none());
        assert!(matches!(
            event(&Event::Timer(3)),
            Some(v2::events::Event::Timer(3))
        ));
    }
}
