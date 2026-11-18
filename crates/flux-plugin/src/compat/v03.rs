//! Plugins built for the API 0.3 and 0.4 (`wit/v0.3`; 0.4 changed only the manifest): Flux runs
//! them with the code of the current version (0.5), as [`super`] does for 0.2.
//!
//! - Every interface but `ui` is answered by the current implementations under the 0.3 names:
//!   0.5 only added functions to `dialogs` (`pick-files`) and `process` (`helper`), and a 0.3
//!   plugin simply doesn't import them.
//! - `ui` has a 0.3 implementation that converts and goes on to the current one: a tree row and a
//!   tree grew (an avatar, a badge's tone, icons after the label, two lines), and so did the
//!   elements (a picture, an avatar, a QR code, a conversation) and a button (a count). So does
//!   `picker`: its item grew an avatar.
//! - The exports are called through the 0.3 bindings: an event or a window's input 0.3 doesn't
//!   know (a tab closed, a service's message, Claude's tool call, a conversation's input) isn't
//!   delivered — such a plugin can't have asked for it.

use wasmtime::component::{HasSelf, Linker};

use crate::api::bindings::flux::plugin as current;
use crate::api::events::{Event, UiEvent, UiInput};
use crate::api::http::{HttpError, ResponseHead};
use crate::api::process::{OutputChannel, ProcessOutput};
use crate::api::review::ProposalOutcome;
use crate::api::server::{ServerRequest, WsMessage, WsOpen};
use crate::api::types::{CommandContext, CommandSource, EditorInfo, Position, Range};
use crate::api::ui;
use crate::runtime::State;

/// The bindings of the API 0.3: the exports of a 0.3 component, and `ui`, which needs converting.
pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        path: "wit/v0.3",
        world: "plugin",
    });
}

use bindings::flux::plugin as v3;

/// The version of the interfaces this module answers (API 0.3 and 0.4).
pub(crate) const API: &str = "0.3";

/// Adds the API 0.3 to the linker, next to the current version and 0.2 (the names differ by their
/// version: `flux:plugin/log@0.3.0`).
pub(crate) fn add_to_linker(linker: &mut Linker<State>) -> wasmtime::Result<()> {
    // The interfaces whose functions 0.5 didn't change (or only added to): the current
    // implementations under the 0.3 names. `types` and `events` hold types only.
    macro_rules! unchanged {
        ($($interface:ident = $name:literal),* $(,)?) => {
            $(
                current::$interface::add_to_linker_instance::<State, HasSelf<State>>(
                    &mut linker.instance(concat!("flux:plugin/", $name, "@0.3.0"))?,
                    |state| state,
                )?;
            )*
        };
    }
    unchanged!(
        log = "log",
        i18n = "i18n",
        commands = "commands",
        notifications = "notifications",
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
        links = "links",
        claude = "claude",
        contexts = "contexts",
    );
    v3::ui::add_to_linker::<State, HasSelf<State>>(linker, |state| state)?;
    v3::picker::add_to_linker::<State, HasSelf<State>>(linker, |state| state)?;
    Ok(())
}

// --- `picker`: 0.3 items, without an avatar ---

impl v3::picker::Host for State {
    fn open(&mut self, spec: v3::picker::PickerSpec) -> u64 {
        let spec = current::picker::PickerSpec {
            placeholder: spec.placeholder,
            items: spec.items.into_iter().map(from_v3_item).collect(),
            searchable: spec.searchable,
            live_query: spec.live_query,
            empty_text: spec.empty_text,
            loading: spec.loading,
            anchor: match spec.anchor {
                v3::picker::PickerAnchor::Window => current::picker::PickerAnchor::Window,
                v3::picker::PickerAnchor::StatusItem(item) => {
                    current::picker::PickerAnchor::StatusItem(item)
                }
                v3::picker::PickerAnchor::Element(element) => {
                    current::picker::PickerAnchor::Element(element)
                }
            },
        };
        current::picker::Host::open(self, spec)
    }

    fn update(&mut self, picker: u64, items: Vec<v3::picker::PickerItem>, loading: bool) {
        let items = items.into_iter().map(from_v3_item).collect();
        current::picker::Host::update(self, picker, items, loading)
    }

    fn close(&mut self, picker: u64) {
        current::picker::Host::close(self, picker)
    }
}

fn from_v3_item(item: v3::picker::PickerItem) -> current::picker::PickerItem {
    current::picker::PickerItem {
        key: item.key,
        icon: item.icon,
        label: from_v3_spans(item.label),
        detail: item.detail,
        badge: item.badge,
        group: item.group,
        avatar: None,
    }
}

// --- `ui`: 0.3 calls in, the current ones out ---

impl v3::ui::Host for State {
    fn set_view(&mut self, window: String, view: v3::ui::View) {
        current::ui::Host::set_view(self, window, from_v3_view(view))
    }

    fn show(&mut self, window: String) {
        current::ui::Host::show(self, window)
    }

    fn hide(&mut self, window: String) {
        current::ui::Host::hide(self, window)
    }

    fn set_title(&mut self, window: String, title: String) {
        current::ui::Host::set_title(self, window, title)
    }
}

fn from_v3_view(view: v3::ui::View) -> ui::View {
    ui::View {
        elements: view
            .elements
            .into_iter()
            .map(|element| ui::Element {
                id: element.id,
                kind: from_v3_kind(element.kind),
                children: element.children,
            })
            .collect(),
    }
}

fn from_v3_kind(kind: v3::ui::ElementKind) -> ui::ElementKind {
    use v3::ui::ElementKind as Kind;
    match kind {
        Kind::Column => ui::ElementKind::Column,
        Kind::Row => ui::ElementKind::Row,
        Kind::Toolbar => ui::ElementKind::Toolbar,
        Kind::Text(spans) => ui::ElementKind::Text(from_v3_spans(spans)),
        Kind::Markdown(text) => ui::ElementKind::Markdown(text),
        Kind::Button(button) => ui::ElementKind::Button(ui::ButtonSpec {
            label: button.label,
            icon: button.icon,
            tooltip: button.tooltip,
            primary: button.primary,
            enabled: button.enabled,
            busy: false,
            badge: None,
        }),
        Kind::TextField(field) => ui::ElementKind::TextField(ui::TextFieldSpec {
            text: field.text,
            placeholder: field.placeholder,
            code: field.code,
            multiline: field.multiline,
            secret: false,
        }),
        Kind::Toggle(toggle) => ui::ElementKind::Toggle(ui::ToggleSpec {
            label: toggle.label,
            on: toggle.on,
            style: match toggle.style {
                v3::ui::ToggleStyle::Switch => ui::ToggleStyle::Switch,
                v3::ui::ToggleStyle::Checkbox => ui::ToggleStyle::Checkbox,
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
                    label: from_v3_spans(row.label),
                    detail: row.detail,
                    badge: row.badge,
                    expanded: row.expanded,
                    subtitle: row.subtitle.map(from_v3_spans),
                    tags: row.tags.into_iter().map(from_v3_badge).collect(),
                    actions: row.actions.into_iter().map(from_v3_action).collect(),
                    avatar: None,
                    badge_tone: None,
                    label_icons: Vec::new(),
                    mention: false,
                })
                .collect(),
            empty_text: tree.empty_text,
            cards: tree.cards,
            two_line: false,
        }),
        Kind::Divider => ui::ElementKind::Divider,
        Kind::Spacer => ui::ElementKind::Spacer,
        Kind::Progress(done) => ui::ElementKind::Progress(done),
        Kind::Badge(badge) => ui::ElementKind::Badge(from_v3_badge(badge)),
        Kind::Properties(properties) => ui::ElementKind::Properties(
            properties
                .into_iter()
                .map(|property| ui::Property {
                    id: property.id,
                    label: property.label,
                    value: from_v3_spans(property.value),
                    editable: property.editable,
                })
                .collect(),
        ),
    }
}

fn from_v3_tone(tone: v3::ui::Tone) -> ui::Tone {
    use v3::ui::Tone;
    match tone {
        Tone::Normal => ui::Tone::Normal,
        Tone::Muted => ui::Tone::Muted,
        Tone::Dim => ui::Tone::Dim,
        Tone::Accent => ui::Tone::Accent,
        Tone::Success => ui::Tone::Success,
        Tone::Warning => ui::Tone::Warning,
        Tone::Error => ui::Tone::Error,
    }
}

fn from_v3_spans(spans: Vec<v3::ui::Span>) -> Vec<ui::Span> {
    spans
        .into_iter()
        .map(|span| ui::Span {
            text: span.text,
            tone: from_v3_tone(span.tone),
            bold: span.bold,
            code: span.code,
            highlight: span.highlight,
        })
        .collect()
}

fn from_v3_badge(badge: v3::ui::BadgeSpec) -> ui::BadgeSpec {
    ui::BadgeSpec {
        text: badge.text,
        tone: from_v3_tone(badge.tone),
        menu: badge.menu,
    }
}

fn from_v3_action(action: v3::ui::RowAction) -> ui::RowAction {
    ui::RowAction {
        id: action.id,
        label: action.label,
        icon: action.icon,
        tooltip: action.tooltip,
        primary: action.primary,
    }
}

// --- The exports: the current values in, 0.3 values out ---

/// A command's context for a 0.3 plugin: the same fields.
pub(crate) fn command_context(context: &CommandContext) -> v3::types::CommandContext {
    use v3::types::CommandSource as Source;
    v3::types::CommandContext {
        source: match context.source {
            CommandSource::Palette => Source::Palette,
            CommandSource::Keys => Source::Keys,
            CommandSource::EditorMenu => Source::EditorMenu,
            CommandSource::TreeMenu => Source::TreeMenu,
            CommandSource::TabMenu => Source::TabMenu,
            CommandSource::Notification => Source::Notification,
            CommandSource::StatusBar => Source::StatusBar,
            CommandSource::Settings => Source::Settings,
        },
        editor: context.editor.as_ref().map(editor_info),
        selections: context.selections.iter().map(range).collect(),
        paths: context.paths.clone(),
        argument: context.argument.clone(),
    }
}

/// An event for a 0.3 plugin; none for one 0.3 doesn't know (0.5's: a tab closed, the file
/// dialog's answer, the service's messages, Claude's tool calls).
pub(crate) fn event(event: &Event) -> Option<v3::events::Event> {
    use v3::events::Event as Old;
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
        Event::FormAnswered(answer) => Old::FormAnswered(answer.clone()),
        Event::PickerQuery(query) => Old::PickerQuery(query.clone()),
        Event::PickerChosen(chosen) => Old::PickerChosen(chosen.clone()),
        Event::LinkActivated(link) => Old::LinkActivated(link.clone()),
        Event::LinkHovered(link) => Old::LinkHovered(link.clone()),
        Event::GitDone(done) => Old::GitDone(done.clone()),
        // 0.5: what a 0.3 plugin can't have asked for.
        Event::TabClosed(_)
        | Event::FilesPicked(_)
        | Event::ServiceMessage(_)
        | Event::WindowOpened(_)
        | Event::WindowClosed(_)
        | Event::ServiceStarted
        | Event::ToolCalled(_)
        | Event::ToolConfirmed(_)
        | Event::ToolCancelled(_) => return None,
    })
}

fn editor_info(editor: &EditorInfo) -> v3::types::EditorInfo {
    v3::types::EditorInfo {
        id: editor.id,
        path: editor.path.clone(),
        language: editor.language.clone(),
        modified: editor.modified,
    }
}

fn range(range: &Range) -> v3::types::Range {
    let position = |position: &Position| v3::types::Position {
        line: position.line,
        column: position.column,
    };
    v3::types::Range {
        start: position(&range.start),
        end: position(&range.end),
    }
}

/// A window's input for a 0.3 plugin; none for a conversation's (0.5: a 0.3 plugin has none).
fn ui_input(input: &UiInput) -> Option<v3::events::UiInput> {
    use v3::events::UiEvent as Old;
    Some(v3::events::UiInput {
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
            UiEvent::RowAction(action) => Old::RowAction(action.clone()),
            UiEvent::LoadOlder
            | UiEvent::LoadNewer
            | UiEvent::Visible(_)
            | UiEvent::MessageAction(_)
            | UiEvent::MessageActionQuoted(_)
            | UiEvent::PartClicked(_)
            | UiEvent::LinkClicked(_)
            | UiEvent::QuoteClicked(_)
            | UiEvent::Composed(_)
            | UiEvent::ComposerAction(_)
            | UiEvent::DraftChanged(_)
            | UiEvent::ReplyCancelled
            | UiEvent::Edited(_)
            | UiEvent::EditCancelled
            | UiEvent::EditLast
            | UiEvent::Find
            | UiEvent::FindNext
            | UiEvent::FindPrevious
            | UiEvent::Cancelled => return None,
        },
    })
}

fn response_head(head: &ResponseHead) -> v3::http::ResponseHead {
    v3::http::ResponseHead {
        request: head.request,
        status: head.status,
        headers: head.headers.clone(),
    }
}

fn http_error(error: HttpError) -> v3::http::HttpError {
    use v3::http::HttpError as Old;
    match error {
        HttpError::Denied(text) => Old::Denied(text),
        HttpError::Invalid(text) => Old::Invalid(text),
        HttpError::Connection(text) => Old::Connection(text),
        HttpError::TimedOut => Old::TimedOut,
        HttpError::Cancelled => Old::Cancelled,
        HttpError::TooLarge => Old::TooLarge,
    }
}

fn server_request(request: &ServerRequest) -> v3::server::ServerRequest {
    v3::server::ServerRequest {
        id: request.id,
        method: request.method.clone(),
        path: request.path.clone(),
        headers: request.headers.clone(),
        body: request.body.clone(),
    }
}

fn ws_open(open: &WsOpen) -> v3::server::WsOpen {
    v3::server::WsOpen {
        connection: open.connection,
        path: open.path.clone(),
        headers: open.headers.clone(),
    }
}

fn ws_message(message: &WsMessage) -> v3::server::WsMessage {
    match message {
        WsMessage::Text(text) => v3::server::WsMessage::Text(text.clone()),
        WsMessage::Binary(bytes) => v3::server::WsMessage::Binary(bytes.clone()),
    }
}

fn process_output(output: &ProcessOutput) -> v3::process::ProcessOutput {
    v3::process::ProcessOutput {
        process: output.process,
        channel: match output.channel {
            OutputChannel::Stdout => v3::process::OutputChannel::Stdout,
            OutputChannel::Stderr => v3::process::OutputChannel::Stderr,
        },
        bytes: output.bytes.clone(),
    }
}

fn proposal_outcome(outcome: &ProposalOutcome) -> v3::review::ProposalOutcome {
    use v3::review::ProposalOutcome as Old;
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
    fn a_0_3_view_is_a_view_of_today() {
        let span = |text: &str| v3::ui::Span {
            text: text.into(),
            tone: v3::ui::Tone::Accent,
            bold: false,
            code: false,
            highlight: false,
        };
        let old = v3::ui::View {
            elements: vec![
                v3::ui::Element {
                    id: "board".into(),
                    kind: v3::ui::ElementKind::Tree(v3::ui::TreeSpec {
                        rows: vec![v3::ui::TreeRow {
                            key: "TRACK-1".into(),
                            parent: None,
                            icon: Some("bug".into()),
                            label: vec![span("Fix")],
                            detail: None,
                            badge: Some("3".into()),
                            expanded: false,
                            subtitle: Some(vec![span("In progress")]),
                            tags: vec![v3::ui::BadgeSpec {
                                text: "high".into(),
                                tone: v3::ui::Tone::Warning,
                                menu: false,
                            }],
                            actions: vec![v3::ui::RowAction {
                                id: "take".into(),
                                label: Some("Take".into()),
                                icon: None,
                                tooltip: None,
                                primary: true,
                            }],
                        }],
                        empty_text: None,
                        cards: true,
                    }),
                    children: Vec::new(),
                },
                v3::ui::Element {
                    id: "props".into(),
                    kind: v3::ui::ElementKind::Properties(vec![v3::ui::Property {
                        id: "status".into(),
                        label: "Status".into(),
                        value: vec![span("Open")],
                        editable: true,
                    }]),
                    children: Vec::new(),
                },
            ],
        };
        let view = from_v3_view(old);
        match &view.elements[0].kind {
            ui::ElementKind::Tree(tree) => {
                assert!(tree.cards);
                assert!(!tree.two_line);
                let row = &tree.rows[0];
                assert_eq!(row.subtitle.as_ref().unwrap()[0].tone, ui::Tone::Accent);
                assert_eq!(row.tags[0].tone, ui::Tone::Warning);
                assert_eq!(row.actions[0].id, "take");
                assert!(row.avatar.is_none() && row.label_icons.is_empty() && !row.mention);
            }
            other => panic!("expected a tree, got {other:?}"),
        }
        match &view.elements[1].kind {
            ui::ElementKind::Properties(properties) => assert!(properties[0].editable),
            other => panic!("expected properties, got {other:?}"),
        }
    }

    #[test]
    fn events_of_0_5_dont_reach_a_0_3_plugin() {
        assert!(event(&Event::ServiceStarted).is_none());
        assert!(event(&Event::ServiceMessage((Some(1), "x".into()))).is_none());
        assert!(event(&Event::ToolCancelled(1)).is_none());
        assert!(matches!(
            event(&Event::PickerChosen((2, None))),
            Some(v3::events::Event::PickerChosen((2, None)))
        ));
        let input = Event::Ui(UiInput {
            window: "board".into(),
            element: "chat".into(),
            event: UiEvent::LoadOlder,
        });
        assert!(event(&input).is_none());
        let input = Event::Ui(UiInput {
            window: "board".into(),
            element: "rows".into(),
            event: UiEvent::RowAction(("TRACK-1".into(), "take".into())),
        });
        assert!(matches!(event(&input), Some(v3::events::Event::Ui(_))));
    }
}
