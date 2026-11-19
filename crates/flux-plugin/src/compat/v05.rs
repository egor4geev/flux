//! Plugins built for the API 0.5 (`wit/v0.5`): Flux runs them with the code of the current
//! version (0.6), as [`super`] does for 0.2 and [`super::v03`] for 0.3.
//!
//! - 0.6 left every interface of 0.5 as it was and added two (`claude-remote`, `tasks`): all of
//!   them are answered by the current implementations under the 0.5 names, and the 0.5 bindings
//!   take the current types for them (`with`) — no conversions.
//! - Only `events` changed: 0.6 added Claude's sessions, the computer's state, the pult's actions
//!   and the tasks' queries. The exports are called through the 0.5 bindings, and such an event
//!   isn't delivered — a 0.5 plugin can't have asked for it (it can't watch, it can't be a source
//!   of tasks).

use wasmtime::component::{HasSelf, Linker};

use crate::api::bindings::flux::plugin as current;
use crate::api::events::{Event, UiEvent, UiInput};
use crate::runtime::State;

/// The bindings of the API 0.5: the exports of a 0.5 component and its `events`; the rest are the
/// current interfaces, which 0.6 didn't change.
pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        path: "wit/v0.5",
        world: "plugin",
        with: {
            "flux:plugin/types@0.5.0": crate::api::bindings::flux::plugin::types,
            "flux:plugin/log@0.5.0": crate::api::bindings::flux::plugin::log,
            "flux:plugin/i18n@0.5.0": crate::api::i18n,
            "flux:plugin/commands@0.5.0": crate::api::bindings::flux::plugin::commands,
            "flux:plugin/notifications@0.5.0": crate::api::bindings::flux::plugin::notifications,
            "flux:plugin/dialogs@0.5.0": crate::api::bindings::flux::plugin::dialogs,
            "flux:plugin/editors@0.5.0": crate::api::bindings::flux::plugin::editors,
            "flux:plugin/project@0.5.0": crate::api::bindings::flux::plugin::project,
            "flux:plugin/storage@0.5.0": crate::api::bindings::flux::plugin::storage,
            "flux:plugin/settings@0.5.0": crate::api::bindings::flux::plugin::settings,
            "flux:plugin/status-bar@0.5.0": crate::api::bindings::flux::plugin::status_bar,
            "flux:plugin/ui@0.5.0": crate::api::bindings::flux::plugin::ui,
            "flux:plugin/http@0.5.0": crate::api::bindings::flux::plugin::http,
            "flux:plugin/server@0.5.0": crate::api::bindings::flux::plugin::server,
            "flux:plugin/process@0.5.0": crate::api::bindings::flux::plugin::process,
            "flux:plugin/terminal@0.5.0": crate::api::bindings::flux::plugin::terminal,
            "flux:plugin/review@0.5.0": crate::api::bindings::flux::plugin::review,
            "flux:plugin/git@0.5.0": crate::api::bindings::flux::plugin::git,
            "flux:plugin/diagnostics@0.5.0": crate::api::bindings::flux::plugin::diagnostics,
            "flux:plugin/secrets@0.5.0": crate::api::bindings::flux::plugin::secrets,
            "flux:plugin/timers@0.5.0": crate::api::bindings::flux::plugin::timers,
            "flux:plugin/system@0.5.0": crate::api::bindings::flux::plugin::system,
            "flux:plugin/picker@0.5.0": crate::api::bindings::flux::plugin::picker,
            "flux:plugin/links@0.5.0": crate::api::bindings::flux::plugin::links,
            "flux:plugin/claude@0.5.0": crate::api::bindings::flux::plugin::claude,
            "flux:plugin/contexts@0.5.0": crate::api::bindings::flux::plugin::contexts,
            "flux:plugin/service@0.5.0": crate::api::bindings::flux::plugin::service,
            "flux:plugin/tools@0.5.0": crate::api::bindings::flux::plugin::tools,
        },
    });
}

use bindings::flux::plugin as v5;

/// The version this module answers.
pub(crate) const API: &str = "0.5";

/// Adds the API 0.5 to the linker, next to the current version and the older ones (the names
/// differ by their version: `flux:plugin/log@0.5.0`).
pub(crate) fn add_to_linker(linker: &mut Linker<State>) -> wasmtime::Result<()> {
    // Every interface with functions: the current implementations under the 0.5 names. `types`
    // and `events` hold types only.
    macro_rules! unchanged {
        ($($interface:ident = $name:literal),* $(,)?) => {
            $(
                current::$interface::add_to_linker_instance::<State, HasSelf<State>>(
                    &mut linker.instance(concat!("flux:plugin/", $name, "@0.5.0"))?,
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
        ui = "ui",
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
        picker = "picker",
        links = "links",
        claude = "claude",
        contexts = "contexts",
        service = "service",
        tools = "tools",
    );
    Ok(())
}

/// An event for a 0.5 plugin; none for one 0.5 doesn't know (0.6's: Claude's sessions, the
/// computer's state, a pult's action, a query for tasks).
pub(crate) fn event(event: &Event) -> Option<v5::events::Event> {
    use v5::events::Event as Old;
    Some(match event {
        Event::ActiveEditorChanged(value) => Old::ActiveEditorChanged(value.clone()),
        Event::EditorOpened(value) => Old::EditorOpened(value.clone()),
        Event::EditorClosed(value) => Old::EditorClosed(*value),
        Event::EditorChanged(value) => Old::EditorChanged(*value),
        Event::SelectionChanged(value) => Old::SelectionChanged(*value),
        Event::EditorSaved(value) => Old::EditorSaved(value.clone()),
        Event::ProjectChanged(value) => Old::ProjectChanged(value.clone()),
        Event::SettingsChanged => Old::SettingsChanged,
        Event::ToolWindowShown(value) => Old::ToolWindowShown(value.clone()),
        Event::ToolWindowHidden(value) => Old::ToolWindowHidden(value.clone()),
        Event::DialogAnswered(value) => Old::DialogAnswered(*value),
        Event::TextAnswered(value) => Old::TextAnswered(value.clone()),
        Event::Ui(input) => Old::Ui(ui_input(input)),
        Event::Timer(value) => Old::Timer(*value),
        Event::HttpResponse(value) => Old::HttpResponse(value.clone()),
        Event::HttpBody(value) => Old::HttpBody(value.clone()),
        Event::HttpDone(value) => Old::HttpDone(value.clone()),
        Event::ServerRequest(value) => Old::ServerRequest(value.clone()),
        Event::StreamClosed(value) => Old::StreamClosed(*value),
        Event::WsOpened(value) => Old::WsOpened(value.clone()),
        Event::WsMessage(value) => Old::WsMessage(value.clone()),
        Event::WsClosed(value) => Old::WsClosed(*value),
        Event::ProcessOutput(value) => Old::ProcessOutput(value.clone()),
        Event::ProcessExited(value) => Old::ProcessExited(*value),
        Event::TerminalExited(value) => Old::TerminalExited(*value),
        Event::TerminalClosed(value) => Old::TerminalClosed(*value),
        Event::ProposalAnswered(value) => Old::ProposalAnswered(value.clone()),
        Event::GitChanged => Old::GitChanged,
        Event::DiagnosticsChanged(value) => Old::DiagnosticsChanged(value.clone()),
        Event::FormAnswered(value) => Old::FormAnswered(value.clone()),
        Event::PickerQuery(value) => Old::PickerQuery(value.clone()),
        Event::PickerChosen(value) => Old::PickerChosen(value.clone()),
        Event::LinkActivated(value) => Old::LinkActivated(value.clone()),
        Event::LinkHovered(value) => Old::LinkHovered(value.clone()),
        Event::GitDone(value) => Old::GitDone(value.clone()),
        Event::TabClosed(value) => Old::TabClosed(value.clone()),
        Event::FilesPicked(value) => Old::FilesPicked(value.clone()),
        Event::ServiceMessage(value) => Old::ServiceMessage(value.clone()),
        Event::WindowOpened(value) => Old::WindowOpened(*value),
        Event::WindowClosed(value) => Old::WindowClosed(*value),
        Event::ServiceStarted => Old::ServiceStarted,
        Event::ToolCalled(value) => Old::ToolCalled(value.clone()),
        Event::ToolConfirmed(value) => Old::ToolConfirmed(*value),
        Event::ToolCancelled(value) => Old::ToolCancelled(*value),
        // 0.6: what a 0.5 plugin can't have asked for.
        Event::ClaudeSession(_)
        | Event::ClaudeSessionGone(_)
        | Event::ClaudeText(_)
        | Event::ClaudeSteps(_)
        | Event::ClaudeRequest(_)
        | Event::ClaudeResolved(_)
        | Event::ClaudeTurnEnded(_)
        | Event::ClaudeFailed(_)
        | Event::ClaudeLimits(_)
        | Event::ClaudeRegion(_)
        | Event::SystemChanged(_)
        | Event::ClaudeAction(_)
        | Event::TaskQuery(_) => return None,
    })
}

/// A window's input for a 0.5 plugin: the same events (0.6 added none).
fn ui_input(input: &UiInput) -> v5::events::UiInput {
    use v5::events::UiEvent as Old;
    v5::events::UiInput {
        window: input.window.clone(),
        element: input.element.clone(),
        event: match &input.event {
            UiEvent::Clicked => Old::Clicked,
            UiEvent::Changed(value) => Old::Changed(value.clone()),
            UiEvent::Submitted(value) => Old::Submitted(value.clone()),
            UiEvent::Toggled(value) => Old::Toggled(*value),
            UiEvent::Selected(value) => Old::Selected(value.clone()),
            UiEvent::Activated(value) => Old::Activated(value.clone()),
            UiEvent::Expanded(value) => Old::Expanded(value.clone()),
            UiEvent::RowAction(value) => Old::RowAction(value.clone()),
            UiEvent::LoadOlder => Old::LoadOlder,
            UiEvent::LoadNewer => Old::LoadNewer,
            UiEvent::Visible(value) => Old::Visible(value.clone()),
            UiEvent::MessageAction(value) => Old::MessageAction(value.clone()),
            UiEvent::MessageActionQuoted(value) => Old::MessageActionQuoted(value.clone()),
            UiEvent::PartClicked(value) => Old::PartClicked(value.clone()),
            UiEvent::LinkClicked(value) => Old::LinkClicked(value.clone()),
            UiEvent::QuoteClicked(value) => Old::QuoteClicked(value.clone()),
            UiEvent::Composed(value) => Old::Composed(value.clone()),
            UiEvent::ComposedWith(value) => Old::ComposedWith(value.clone()),
            UiEvent::ComposerAction(value) => Old::ComposerAction(value.clone()),
            UiEvent::DraftChanged(value) => Old::DraftChanged(value.clone()),
            UiEvent::ReplyCancelled => Old::ReplyCancelled,
            UiEvent::Edited(value) => Old::Edited(value.clone()),
            UiEvent::EditCancelled => Old::EditCancelled,
            UiEvent::EditLast => Old::EditLast,
            UiEvent::Find => Old::Find,
            UiEvent::FindNext => Old::FindNext,
            UiEvent::FindPrevious => Old::FindPrevious,
            UiEvent::Cancelled => Old::Cancelled,
            UiEvent::MediaPlayed(value) => Old::MediaPlayed(value.clone()),
            UiEvent::PanelOpened(value) => Old::PanelOpened(*value),
            UiEvent::PanelSearched(value) => Old::PanelSearched(value.clone()),
            UiEvent::PanelChosen(value) => Old::PanelChosen(value.clone()),
            UiEvent::PanelVisible(value) => Old::PanelVisible(value.clone()),
            UiEvent::PanelLoadMore(value) => Old::PanelLoadMore(*value),
            UiEvent::PinnedClicked(value) => Old::PinnedClicked(value.clone()),
            UiEvent::PinnedAction(value) => Old::PinnedAction(value.clone()),
            UiEvent::Suggest(value) => Old::Suggest(value.clone()),
            UiEvent::KeyboardPressed(value) => Old::KeyboardPressed(value.clone()),
            UiEvent::SelectionAction(value) => Old::SelectionAction(value.clone()),
            UiEvent::ForwardCancelled => Old::ForwardCancelled,
            UiEvent::PollVoted(value) => Old::PollVoted(value.clone()),
            UiEvent::ReactionToggled(value) => Old::ReactionToggled(value.clone()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::claude_remote::{SystemEvent, UsageLimits};
    use crate::api::tasks::{Query, TaskQuery};

    #[test]
    fn events_of_0_6_dont_reach_a_0_5_plugin() {
        assert!(event(&Event::SystemChanged(SystemEvent::WillSleep)).is_none());
        assert!(event(&Event::ClaudeSessionGone("s".into())).is_none());
        assert!(
            event(&Event::ClaudeLimits(UsageLimits {
                five_hour: None,
                seven_day: None,
                rejected: false,
            }))
            .is_none()
        );
        assert!(
            event(&Event::TaskQuery(TaskQuery {
                id: 1,
                query: Query::Mine,
            }))
            .is_none()
        );
        assert!(matches!(
            event(&Event::ServiceMessage((Some(2), "hi".into()))),
            Some(v5::events::Event::ServiceMessage((Some(2), _)))
        ));
        let input = Event::Ui(UiInput {
            window: "chats".into(),
            element: "chat".into(),
            event: UiEvent::PollVoted(("m".into(), 1, vec![0])),
        });
        assert!(matches!(
            event(&input),
            Some(v5::events::Event::Ui(v5::events::UiInput {
                event: v5::events::UiEvent::PollVoted(_),
                ..
            }))
        ));
    }
}
