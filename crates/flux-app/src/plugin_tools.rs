//! Claude's tools from plugins (API 0.5, FluxGram part 1, wiki ADR-043): the manifest's `[[tools]]`
//! of the plugins running in a window join Flux's MCP server `flux` of each Claude session the
//! window starts (the CLI reads the list once, when its process starts: a plugin turned on later
//! is in the next sessions). Claude sees a tool as `mcp__flux__<prefix>_<name>`, the prefix from
//! the plugin's id (`flux.fluxgram` → `fluxgram`).
//!
//! A call goes to the plugin's instance in the session's window as `tool-called`, under an id of
//! the app's own counter; the plugin's answer (`tools.respond`) goes back to the session: the text
//! for Claude, and how the step looks in the chat ([`StepOutput`]). A `write` tool's card
//! (`tools.confirm`) is a question of the session, as a permission card is — in any permission
//! mode (the session's hook allows Flux's tools; the plugin asks): the chat shows it above the
//! field, the status bar and the notifications count it. An interrupt, a closed session or a
//! stopped plugin ends the call: the plugin hears `tool-cancelled`, Claude gets an error. A plugin
//! that doesn't answer keeps the call running until Claude is stopped (reading a long history
//! takes its time).

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use flux_claude::mcp::{self, ToolSpec};
use flux_plugin::api::events::Event;
use flux_plugin::api::tools::{ConfirmChoice, Confirmation, ToolCall, ToolOutput as PluginOutput};
use flux_plugin::log::Level;
use gpui::{App, Context, Global, SharedString, WeakEntity};
use serde_json::{Value, json};

use crate::claude_session::ClaudeSession;
use crate::i18n::trn;
use crate::markdown::{self, Block};
use crate::plugins::{PluginStatus, PluginStore};

/// A plugin's tool as a session's server lists it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PluginTool {
    /// The name in the server: "fluxgram_read_chat".
    pub name: String,
    pub plugin: Arc<str>,
    /// The manifest's name: "read_chat".
    pub tool: String,
    /// The manifest's title in the interface language: the step in the chat.
    pub title: SharedString,
    /// The plugin's name in the interface language: a group of its steps, the card.
    pub plugin_name: SharedString,
    /// The plugin's icon (an asset path); none — the default.
    pub icon: Option<SharedString>,
}

/// How a finished call of a plugin's tool looks in the chat (the plugin's `tool-output`).
#[derive(Clone, Debug, Default)]
pub(crate) struct StepOutput {
    /// The step's line instead of the tool's title.
    pub summary: Option<String>,
    /// Dim on the right of the line.
    pub detail: Option<String>,
    /// The excerpts under the line when the step is open, parsed.
    pub excerpt: Option<Rc<Vec<Block>>>,
}

/// A call of a plugin's tool in progress in a session.
struct PendingCall {
    /// The CLI's request: the answer goes back with it; the card is a question under this id.
    request: String,
    tool: PluginTool,
    tool_use_id: Option<String>,
    input: Value,
    /// The plugin's card on screen.
    card: Option<Confirmation>,
}

/// A session's side of the plugins' tools: the window's plugins, the tools its server lists, the
/// calls in progress and how the answered ones look.
#[derive(Default)]
pub(crate) struct SessionTools {
    store: Option<WeakEntity<PluginStore>>,
    tools: Vec<PluginTool>,
    calls: HashMap<u64, PendingCall>,
    /// By the step's tool use id.
    steps: HashMap<String, StepOutput>,
}

/// A call ended by Flux or the user, not by the plugin's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallEnd {
    /// Esc, the stop button: the plugin hears `tool-cancelled`, Claude gets an error.
    Interrupted,
    /// The CLI withdrew the call or its process ended (the session closed): the plugin hears
    /// `tool-cancelled`; nobody waits for an answer.
    Withdrawn,
    /// The plugin stopped: Claude gets an error; the instance is gone.
    PluginStopped,
}

impl SessionTools {
    pub fn new(store: Option<WeakEntity<PluginStore>>) -> Self {
        SessionTools {
            store,
            ..SessionTools::default()
        }
    }

    pub fn store(&self) -> Option<&WeakEntity<PluginStore>> {
        self.store.as_ref()
    }

    /// The tools of the plugins running in the window now, for a process about to start; the
    /// names don't clash with `taken` (Flux's own tools). Their specs join the server.
    pub fn refresh(&mut self, taken: &[String], cx: &mut App) -> Vec<ToolSpec> {
        let Some(store) = self.store.as_ref().and_then(WeakEntity::upgrade) else {
            self.tools.clear();
            return Vec::new();
        };
        let (tools, specs) = list(store.read(cx), taken);
        self.tools = tools;
        specs
    }

    /// The plugin's tool behind a name of the server ("fluxgram_read_chat").
    pub fn tool(&self, name: &str) -> Option<&PluginTool> {
        self.tools.iter().find(|tool| tool.name == name)
    }

    /// The plugin's tool behind a step of the chat (`mcp__flux__fluxgram_read_chat`).
    pub fn of_step(&self, tool_name: &str) -> Option<&PluginTool> {
        mcp::flux_tool(tool_name).and_then(|name| self.tool(name))
    }

    /// How an answered step looks, by its tool use id.
    pub fn output(&self, tool_use_id: &str) -> Option<&StepOutput> {
        self.steps.get(tool_use_id)
    }

    /// The card of a plugin shown under this request id, with its tool.
    pub fn card(&self, request: &str) -> Option<(&PluginTool, &Confirmation)> {
        self.calls.values().find_map(|call| {
            (call.request == request)
                .then_some(call.card.as_ref())
                .flatten()
                .map(|card| (&call.tool, card))
        })
    }

    /// Hands a call to the plugin's instance in the window: `Err` — why it can't run (Claude gets
    /// it at once as the tool's error).
    pub fn start_call(
        &mut self,
        request: &str,
        name: &str,
        input: Value,
        tool_use_id: Option<String>,
        session: WeakEntity<ClaudeSession>,
        cx: &mut App,
    ) -> Result<(), String> {
        let Some(tool) = self.tool(name).cloned() else {
            return Err(format!("Flux has no tool {name}."));
        };
        let store = self.store.clone().filter(|store| store.upgrade().is_some());
        let running = store
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .is_some_and(|store| {
                store.read(cx).plugin(&tool.plugin).is_some_and(|plugin| {
                    plugin.status == PluginStatus::Running && plugin.entry.manifest.wasm.is_some()
                })
            });
        let (Some(store), true) = (store, running) else {
            return Err(format!(
                "The plugin {} isn't running in Flux (it is turned off, starting or stopped).",
                tool.plugin_name
            ));
        };
        let id = next_id();
        cx.default_global::<Calls>().0.insert(
            id,
            Route {
                session,
                store: store.clone(),
                plugin: tool.plugin.clone(),
            },
        );
        let event = Event::ToolCalled(ToolCall {
            id,
            tool: tool.tool.clone(),
            arguments: input.to_string(),
        });
        send(&store, &tool.plugin, event, cx);
        self.calls.insert(
            id,
            PendingCall {
                request: request.to_string(),
                tool,
                tool_use_id,
                input,
                card: None,
            },
        );
        Ok(())
    }

    /// The plugin answered: the call ends; the request to answer, and whether its card was up.
    pub fn answered(&mut self, call: u64, output: &PluginOutput) -> Option<(String, bool)> {
        let pending = self.calls.remove(&call)?;
        if let Some(id) = &pending.tool_use_id {
            self.steps.insert(id.clone(), StepOutput::of(output));
        }
        Some((pending.request, pending.card.is_some()))
    }

    /// The plugin's card for a call: what the session asks with (`None` — the call is gone).
    /// `Some(false)` — a card was up already and is replaced.
    pub fn show_card(&mut self, call: u64, card: Confirmation) -> Option<CardShown> {
        let pending = self.calls.get_mut(&call)?;
        let replaced = pending.card.replace(card.clone()).is_some();
        Some(CardShown {
            request: pending.request.clone(),
            tool_name: mcp::tool_name(&pending.tool.name),
            tool_use_id: pending.tool_use_id.clone().unwrap_or_default(),
            input: pending.input.clone(),
            title: card.title,
            subtitle: card.subtitle,
            replaced,
        })
    }

    /// The user's choice on the card of `request`: the plugin hears it. False — no such card.
    pub fn choose(&mut self, request: &str, choice: ConfirmChoice, cx: &mut App) -> bool {
        let Some((&id, pending)) = self
            .calls
            .iter_mut()
            .find(|(_, call)| call.request == request)
        else {
            return false;
        };
        if pending.card.take().is_none() {
            return false;
        }
        let plugin = pending.tool.plugin.clone();
        if let Some(store) = &self.store {
            send(store, &plugin, Event::ToolConfirmed((id, choice)), cx);
        }
        true
    }

    /// Ends the calls in progress (of `plugin` only, when given): the plugin hears it unless it
    /// stopped. The requests to answer or forget, with whether their cards were up.
    pub fn end_calls(
        &mut self,
        plugin: Option<&str>,
        end: CallEnd,
        cx: &mut App,
    ) -> Vec<(String, bool)> {
        let ids: Vec<u64> = self
            .calls
            .iter()
            .filter(|(_, call)| plugin.is_none_or(|plugin| *call.tool.plugin == *plugin))
            .map(|(id, _)| *id)
            .collect();
        let mut ended = Vec::new();
        for id in ids {
            let Some(pending) = self.calls.remove(&id) else {
                continue;
            };
            forget(id, cx);
            if end != CallEnd::PluginStopped
                && let Some(store) = &self.store
            {
                send(store, &pending.tool.plugin, Event::ToolCancelled(id), cx);
            }
            ended.push((pending.request, pending.card.is_some()));
        }
        ended
    }

    /// The call of a request the CLI withdrew (`control_cancel_request`), if one is in progress.
    pub fn withdraw(&mut self, request: &str, cx: &mut App) -> Option<bool> {
        let id = self
            .calls
            .iter()
            .find(|(_, call)| call.request == request)
            .map(|(id, _)| *id)?;
        let pending = self.calls.remove(&id)?;
        forget(id, cx);
        if let Some(store) = &self.store {
            send(store, &pending.tool.plugin, Event::ToolCancelled(id), cx);
        }
        Some(pending.card.is_some())
    }
}

/// What a card asks the session to show: a question under the call's request.
pub(crate) struct CardShown {
    pub request: String,
    /// Claude's name of the tool.
    pub tool_name: String,
    pub tool_use_id: String,
    pub input: Value,
    pub title: String,
    pub subtitle: Option<String>,
    pub replaced: bool,
}

impl StepOutput {
    fn of(output: &PluginOutput) -> Self {
        let excerpt = output
            .excerpt
            .as_deref()
            .filter(|excerpt| !excerpt.trim().is_empty())
            .map(|excerpt| Rc::new(markdown::parse(excerpt)));
        StepOutput {
            summary: output
                .summary
                .clone()
                .filter(|line| !line.trim().is_empty()),
            detail: output.detail.clone().filter(|line| !line.trim().is_empty()),
            excerpt,
        }
    }
}

// --- The app's calls ---

/// Where each call in progress came from, by its id: the plugin's answer and card go there.
#[derive(Default)]
struct Calls(HashMap<u64, Route>);

impl Global for Calls {}

struct Route {
    session: WeakEntity<ClaudeSession>,
    store: WeakEntity<PluginStore>,
    plugin: Arc<str>,
}

/// The next call's id: unique in the app, so in every instance.
fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn send(store: &WeakEntity<PluginStore>, plugin: &str, event: Event, cx: &mut App) {
    if let Some(store) = store.upgrade() {
        store.read(cx).send_to(plugin, event);
    }
}

fn forget(id: u64, cx: &mut App) {
    if cx.has_global::<Calls>() {
        cx.global_mut::<Calls>().0.remove(&id);
    }
}

/// The session of a call of this plugin in this window, if the call is in progress.
fn session_of(
    call: u64,
    plugin: &str,
    cx: &mut Context<PluginStore>,
) -> Option<WeakEntity<ClaudeSession>> {
    let store = cx.entity_id();
    let route = cx.try_global::<Calls>()?.0.get(&call)?;
    (route.store.entity_id() == store && *route.plugin == *plugin).then(|| route.session.clone())
}

/// `tools.respond`: the answer to a call of the plugin's tool goes to its session.
pub(crate) fn respond(
    store: &mut PluginStore,
    plugin: &str,
    call: u64,
    output: PluginOutput,
    cx: &mut Context<PluginStore>,
) {
    let Some(session) = session_of(call, plugin, cx) else {
        return store.log_line(
            plugin,
            Level::Debug,
            &format!("tools.respond({call}): the call is gone"),
        );
    };
    forget(call, cx);
    session
        .update(cx, |session, cx| {
            session.plugin_tool_answered(call, output, cx)
        })
        .ok();
}

/// `tools.confirm`: the card of a `write` tool's call shows in its session's chat.
pub(crate) fn confirm(
    store: &mut PluginStore,
    plugin: &str,
    call: u64,
    card: Box<Confirmation>,
    cx: &mut Context<PluginStore>,
) {
    let Some(session) = session_of(call, plugin, cx) else {
        return store.log_line(
            plugin,
            Level::Debug,
            &format!("tools.confirm({call}): the call is gone"),
        );
    };
    session
        .update(cx, |session, cx| session.show_plugin_card(call, *card, cx))
        .ok();
}

// --- The tools a session lists ---

/// The tools of the plugins running (or starting) in the window, with their specs: names unique
/// among the plugins and apart from `taken`; a schema that can't be read is logged and replaced
/// by "any object".
fn list(store: &PluginStore, taken: &[String]) -> (Vec<PluginTool>, Vec<ToolSpec>) {
    let plugins: Vec<_> = store
        .plugins()
        .iter()
        .filter(|plugin| {
            matches!(
                plugin.status,
                PluginStatus::Running | PluginStatus::Starting
            ) && plugin.entry.manifest.wasm.is_some()
                && !plugin.entry.manifest.tools.is_empty()
        })
        .collect();
    let ids: Vec<&str> = plugins.iter().map(|plugin| plugin.id()).collect();
    let prefixes = prefixes(&ids);
    let mut names: Vec<String> = taken.to_vec();
    let (mut tools, mut specs) = (Vec::new(), Vec::new());
    for (plugin, prefix) in plugins.iter().zip(prefixes) {
        let manifest = &plugin.entry.manifest;
        let icon = manifest
            .icon
            .as_deref()
            .map(|file| crate::icons::plugin_asset(plugin.id(), file));
        for spec in &manifest.tools {
            let name = server_name(&prefix, &spec.name);
            if names.contains(&name) {
                store.log_line(
                    plugin.id(),
                    Level::Warn,
                    &format!(
                        "The tool {} isn't offered to Claude: its name {name} is taken",
                        spec.name
                    ),
                );
                continue;
            }
            let file = spec.schema.as_deref();
            let schema = match parse_schema(file.map(|file| plugin.entry.files.read(file))) {
                Ok(schema) => schema,
                Err(error) => {
                    store.log_line(
                        plugin.id(),
                        Level::Warn,
                        &format!(
                            "The schema of the tool {} ({}): {error}; Claude may pass any object",
                            spec.name,
                            file.unwrap_or_default()
                        ),
                    );
                    json!({ "type": "object" })
                }
            };
            names.push(name.clone());
            specs.push(ToolSpec {
                name: name.clone(),
                title: spec.title.clone(),
                description: spec.description.clone(),
                input_schema: schema,
                read_only: !spec.write,
            });
            tools.push(PluginTool {
                name,
                plugin: Arc::from(plugin.id()),
                tool: spec.name.clone(),
                title: plugin.tr(&spec.title).to_string().into(),
                plugin_name: plugin.name().to_string().into(),
                icon: icon.clone(),
            });
        }
    }
    (tools, specs)
}

/// Each plugin's prefix: the last part of its id in `[a-z0-9_]` (`flux.fluxgram` → `fluxgram`),
/// unique among them (a number after a repeated one).
fn prefixes(ids: &[&str]) -> Vec<String> {
    let mut taken: Vec<String> = Vec::new();
    for id in ids {
        let last = id.rsplit('.').next().unwrap_or(id);
        let mut base: String = last
            .chars()
            .map(|c| match c.to_ascii_lowercase() {
                c @ ('a'..='z' | '0'..='9') => c,
                _ => '_',
            })
            .collect();
        base = base.trim_matches('_').to_string();
        if base.is_empty() {
            base = "plugin".into();
        }
        let mut prefix = base.clone();
        let mut n = 2;
        while taken.contains(&prefix) {
            prefix = format!("{base}{n}");
            n += 1;
        }
        taken.push(prefix);
    }
    taken
}

/// A tool's name in the server: `<prefix>_<name>`, the prefix cut so that Claude's whole name
/// (`mcp__flux__…`) stays within the API's limit.
fn server_name(prefix: &str, tool: &str) -> String {
    let room = mcp::MAX_TOOL_NAME - mcp::tool_name("").len();
    let keep = room.saturating_sub(tool.len() + 1).max(1);
    let prefix: String = prefix.chars().take(keep).collect();
    format!("{prefix}_{tool}")
}

/// The arguments' schema from the plugin's file (`None` — the manifest names none: no arguments):
/// a JSON schema of an object, as the MCP wants; `Err` — why it isn't one.
fn parse_schema(file: Option<Option<std::borrow::Cow<'static, [u8]>>>) -> Result<Value, String> {
    let Some(file) = file else {
        return Ok(json!({ "type": "object", "properties": {} }));
    };
    let Some(bytes) = file else {
        return Err("the file can't be read".into());
    };
    let schema: Value = serde_json::from_slice(&bytes).map_err(|err| format!("not JSON: {err}"))?;
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => Ok(schema),
        Some(other) => Err(format!("its type is {other}, not object")),
        None if schema.is_object() => Err("it has no \"type\": \"object\"".into()),
        None => Err("not a JSON object".into()),
    }
}

/// The group of a plugin's steps in the chat: "FluxGram · 3 steps".
pub(crate) fn group_title(plugin_name: &str, steps: usize) -> String {
    format!("{plugin_name} · {}", trn(steps, "{n} step", "{n} steps"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    #[test]
    fn prefixes_come_from_the_ids_and_stay_apart() {
        assert_eq!(
            prefixes(&[
                "flux.fluxgram",
                "acme.issue-tracker",
                "other.fluxgram",
                "x.--"
            ]),
            vec!["fluxgram", "issue_tracker", "fluxgram2", "plugin"]
        );
    }

    #[test]
    fn names_fit_the_limit() {
        assert_eq!(server_name("fluxgram", "read_chat"), "fluxgram_read_chat");
        let long_tool = "a".repeat(40);
        let name = server_name("a_very_long_plugin_prefix", &long_tool);
        assert!(mcp::tool_name(&name).len() <= mcp::MAX_TOOL_NAME);
        assert!(name.ends_with(&format!("_{long_tool}")));
        assert_eq!(&name[..12], "a_very_long_");
    }

    #[test]
    fn a_bad_schema_falls_back() {
        let read = |text: &'static str| Some(Some(Cow::Borrowed(text.as_bytes())));
        assert_eq!(
            parse_schema(read(
                r#"{"type":"object","properties":{"text":{"type":"string"}}}"#
            ))
            .unwrap()["properties"]["text"]["type"],
            "string"
        );
        assert_eq!(
            parse_schema(None).unwrap(),
            json!({ "type": "object", "properties": {} })
        );
        assert!(parse_schema(Some(None)).is_err());
        assert!(parse_schema(read("not json")).is_err());
        assert!(parse_schema(read(r#"{"type":"string"}"#)).is_err());
        assert!(parse_schema(read(r#"[1, 2]"#)).is_err());
    }

    #[test]
    fn step_output_drops_empty_lines() {
        let output = PluginOutput {
            text: "ok".into(),
            error: false,
            summary: Some("Backend — 50 messages".into()),
            detail: Some("  ".into()),
            excerpt: Some("**Dima:** done".into()),
        };
        let step = StepOutput::of(&output);
        assert_eq!(step.summary.as_deref(), Some("Backend — 50 messages"));
        assert_eq!(step.detail, None);
        assert!(step.excerpt.is_some());
    }
}
