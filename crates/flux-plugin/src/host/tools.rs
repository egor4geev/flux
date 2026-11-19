//! `tools` (API 0.5): the answers to Claude's calls of the plugin's tools and the cards of the
//! `write` ones. The app delivers a call to the instance of the session's window as `tool-called`;
//! the instance keeps the calls in progress ([`HostState::track`]): one that is gone
//! (`tool-cancelled`, or answered already) gets no answer and no card. Only the windows' instances
//! get calls: the service's answers are dropped.

use crate::api::bindings::flux::plugin::tools;
use crate::api::events::Event;
use crate::runtime::{HostCall, InstanceRole, State};

use super::HostState;

impl tools::Host for State {
    fn respond(&mut self, call: u64, output: tools::ToolOutput) {
        let host = &mut self.host;
        if host.role == InstanceRole::Service {
            return host.warn("tools.respond: Claude's tools are called in the windows' instances");
        }
        if !host.tool_calls.remove(&call) {
            return host.warn(&format!("tools.respond: the call {call} is gone"));
        }
        host.call(HostCall::ToolRespond { call, output });
    }

    fn confirm(&mut self, call: u64, card: tools::Confirmation) -> Result<(), String> {
        let host = &self.host;
        if host.role == InstanceRole::Service {
            return Err(
                "tools.confirm: Claude's tools are called in the windows' instances".into(),
            );
        }
        if !host.tool_calls.contains(&call) {
            return Err(format!("tools.confirm: the call {call} is gone"));
        }
        host.call(HostCall::ToolConfirm {
            call,
            card: Box::new(card),
        });
        Ok(())
    }
}

impl HostState {
    /// Follows the calls of Claude's tools an event starts or ends, before the plugin hears it.
    pub(crate) fn track(&mut self, event: &Event) {
        match event {
            Event::ToolCalled(call) => {
                self.tool_calls.insert(call.id);
            }
            Event::ToolCancelled(call) => {
                self.tool_calls.remove(call);
            }
            _ => {}
        }
    }
}
