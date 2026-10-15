//! A subagent's conversation on the chat's place (part 10.2, the mockup the author approved):
//! «Watch» / «Open» on a subagent's card shows what it does — live, with the same groups and cards
//! as the session's conversation — under a bar of crumbs «← session › Agent · its task» with its
//! totals and Stop, its task from Claude on top, and a line at the bottom: that Claude waits for it
//! (or what it answered), Esc to go back. A subagent of the subagent opens deeper; Esc goes up one
//! level, the crumbs anywhere. The session's conversation keeps its place meanwhile: each level has
//! its own list ([`TranscriptState`] with a scope).
//!
//! No message field while a subagent is in sight: one can't write to a subagent.

use flux_claude::{EntryId, ToolCall, ToolEntry, ToolState};
use gpui::{
    AnyElement, App, ClickEvent, Context, KeyBinding, SharedString, Window, actions, div,
    prelude::*, px,
};

use crate::claude_chat::ClaudeChat;
use crate::claude_transcript::{self, TranscriptState};
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::theme::{self, Theme, UiColors};
use crate::ui;

/// The key context of the chat while a subagent is in sight.
pub const CONTEXT: &str = "ClaudeAgentView";

actions!(
    claude_agent_view,
    [
        /// Esc in a subagent's view: one level up.
        Back,
        /// The newest subagent of the conversation in sight, on the chat's place (what «Watch»
        /// on its card does; UI scenarios can't click).
        OpenLatest,
    ]
);

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Back, Some(CONTEXT))]);
}

/// One level of subagents in sight: the Agent call and its own list.
pub struct AgentView {
    pub tool: EntryId,
    pub transcript: TranscriptState,
}

impl ClaudeChat {
    /// The transcript in sight: the deepest open subagent's, else the session's.
    pub(crate) fn shown_transcript(&self) -> &TranscriptState {
        self.agents
            .last()
            .map_or(&self.transcript, |agent| &agent.transcript)
    }

    pub(crate) fn shown_transcript_mut(&mut self) -> &mut TranscriptState {
        match self.agents.last_mut() {
            Some(agent) => &mut agent.transcript,
            None => &mut self.transcript,
        }
    }

    /// The session's transcript, then each open subagent's.
    pub(crate) fn transcripts_mut(&mut self) -> impl Iterator<Item = &mut TranscriptState> {
        std::iter::once(&mut self.transcript)
            .chain(self.agents.iter_mut().map(|agent| &mut agent.transcript))
    }

    /// The session changed: every list compares its entries again; the search looks again.
    pub(crate) fn invalidate_transcripts(&mut self) {
        for transcript in self.transcripts_mut() {
            transcript.invalidate();
        }
        self.search_stale();
    }

    /// Shows the subagent of the Agent call `tool` on the chat's place (one level deeper when a
    /// subagent is in sight already); the keyboard goes to the chat for Esc.
    pub(crate) fn show_agent(
        &mut self,
        tool: EntryId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let known = {
            let model = self.session().read(cx).model();
            claude_transcript::find_tool(&model.entries, tool)
                .is_some_and(|call| matches!(call.call, ToolCall::Agent { .. }))
        };
        if !known || self.agents.last().is_some_and(|agent| agent.tool == tool) {
            return;
        }
        // Opened from the session's card: the levels above it go (a crumb re-opens from there).
        if let Some(level) = self.agents.iter().position(|agent| agent.tool == tool) {
            self.agents.truncate(level + 1);
        } else {
            let transcript = TranscriptState::scoped(cx.weak_entity(), tool);
            self.agents.push(AgentView { tool, transcript });
        }
        self.after_view_change(window, cx);
    }

    /// [`OpenLatest`]: the newest Agent call of the conversation in sight.
    pub(crate) fn open_latest_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let latest = {
            let model = self.session().read(cx).model();
            let entries = claude_transcript::scope_entries(model, self.shown_transcript().scope);
            entries.iter().rev().find_map(|entry| match &entry.kind {
                flux_claude::EntryKind::Tool(tool)
                    if matches!(tool.call, ToolCall::Agent { .. }) =>
                {
                    Some(entry.id)
                }
                _ => None,
            })
        };
        if let Some(tool) = latest {
            self.show_agent(tool, window, cx);
        }
    }

    /// Esc: one level up — the subagent above, or the session's conversation.
    pub(crate) fn agent_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.agents.pop().is_none() {
            return;
        }
        self.after_view_change(window, cx);
    }

    /// A crumb: `level` subagents stay open (0 — back to the session).
    fn agent_crumb(&mut self, level: usize, window: &mut Window, cx: &mut Context<Self>) {
        if level >= self.agents.len() {
            return;
        }
        self.agents.truncate(level);
        self.after_view_change(window, cx);
    }

    fn after_view_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shown_transcript_mut().invalidate();
        // The search goes on in the conversation now in sight.
        self.search_stale();
        if self.agents.is_empty() && !self.search_is_open() {
            self.focus_composer(window, cx);
        } else if !self.search_is_open() {
            window.focus(&self.focus_handle);
        }
        cx.notify();
    }

    /// Stop on the subagent's bar: its task stops (the CLI's `stop_task`); a subagent without a
    /// known task stops with the turn.
    fn stop_agent(&mut self, tool: EntryId, cx: &mut Context<Self>) {
        let session = self.session().clone();
        let task = {
            let model = session.read(cx).model();
            claude_transcript::find_tool(&model.entries, tool)
                .and_then(|call| model.task_id(&call.tool_use_id))
                .map(str::to_string)
        };
        session.update(cx, |session, cx| match task {
            Some(task) => session.stop_task(task, cx),
            None => session.interrupt(cx),
        });
    }
}

/// The bar over a subagent's conversation: the crumbs back, the subagent's kind and model, its
/// totals, Stop while it works.
pub fn render_header(chat: &ClaudeChat, cx: &mut Context<ClaudeChat>) -> Option<AnyElement> {
    let current = chat.agents.last()?;
    let ui = Theme::ui(cx);
    let session = chat.session().read(cx);
    let title: SharedString = session.title();
    let model = session.model();
    let tool = claude_transcript::find_tool(&model.entries, current.tool)?;
    let running = claude_transcript::agent_is_running(tool);
    let mut crumbs: Vec<AnyElement> = vec![
        div()
            .id("claude-agent-crumb-session")
            .flex_shrink()
            .min_w(px(48.))
            .max_w(px(200.))
            .h(px(26.))
            .px(px(6.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(ui::RADIUS_SM))
            .text_color(ui.text_muted)
            .cursor_pointer()
            .hover(move |style| style.bg(ui.hover).text_color(ui.foreground))
            .tooltip(ui::tooltip(tr("Back to the Session"), Some("Esc".into())))
            .child(icon(IconName::ArrowLeft, ui.text_muted).size(px(14.)))
            .child(div().min_w_0().truncate().child(title))
            .on_click(
                cx.listener(|chat, _: &ClickEvent, window, cx| chat.agent_crumb(0, window, cx)),
            )
            .into_any_element(),
    ];
    // Subagents above the one in sight: crumbs too.
    for (level, agent) in chat.agents.iter().enumerate() {
        let Some(call) = claude_transcript::find_tool(&model.entries, agent.tool) else {
            continue;
        };
        crumbs.push(chevron(ui));
        let description = agent_description(call);
        if level + 1 == chat.agents.len() {
            crumbs.push(current_crumb(call, &description, ui));
        } else {
            crumbs.push(
                div()
                    .id(("claude-agent-crumb", level))
                    .flex_shrink()
                    .min_w(px(40.))
                    .max_w(px(160.))
                    .h(px(26.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .rounded(px(ui::RADIUS_SM))
                    .text_color(ui.text_muted)
                    .cursor_pointer()
                    .hover(move |style| style.bg(ui.hover).text_color(ui.foreground))
                    .child(div().min_w_0().truncate().child(description))
                    .on_click(cx.listener(move |chat, _: &ClickEvent, window, cx| {
                        chat.agent_crumb(level + 1, window, cx)
                    }))
                    .into_any_element(),
            );
        }
    }
    let target = current.tool;
    let stop = running.then(|| {
        div()
            .id("claude-agent-stop")
            .flex_none()
            .h(px(24.))
            .px(px(9.))
            .flex()
            .items_center()
            .gap(px(5.))
            .rounded(px(ui::RADIUS_SM))
            .border_1()
            .border_color(UiColors::tint(ui.foreground, 0.09))
            .bg(UiColors::tint(ui.foreground, 0.06))
            .text_size(px(theme::TEXT_SM))
            .text_color(ui.foreground)
            .cursor_pointer()
            .hover(move |style| style.bg(ui.hover))
            .child(icon(IconName::Stop, ui.error).size(px(12.)))
            .child(tr("Stop"))
            .on_click(cx.listener(move |chat, _: &ClickEvent, _, cx| chat.stop_agent(target, cx)))
    });
    Some(
        div()
            .flex_none()
            .h(px(40.))
            .pl(px(8.))
            .pr(px(10.))
            .flex()
            .items_center()
            .gap(px(4.))
            .border_b_1()
            .border_color(ui.divider)
            .text_size(px(theme::TEXT_MD))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .overflow_hidden()
                    .children(crumbs),
            )
            .children(stop)
            .into_any_element(),
    )
}

/// The line under a subagent's conversation: that Claude waits for it (or how it ended), Esc back.
pub fn render_footer(chat: &ClaudeChat, cx: &mut Context<ClaudeChat>) -> Option<AnyElement> {
    let current = chat.agents.last()?;
    let ui = Theme::ui(cx);
    let model = chat.session().read(cx).model();
    let tool = claude_transcript::find_tool(&model.entries, current.tool)?;
    let background = matches!(
        tool.call,
        ToolCall::Agent {
            background: true,
            ..
        }
    );
    let totals = claude_transcript::agent_totals_label(tool);
    let (mark, label): (AnyElement, String) = if claude_transcript::agent_is_running(tool) {
        (
            div()
                .size(px(7.))
                .rounded(px(3.5))
                .bg(ui.accent)
                .into_any_element(),
            if background {
                tr("The agent works in the background").to_string()
            } else {
                tr("The agent works — Claude waits for its answer").to_string()
            },
        )
    } else {
        match tool.state {
            ToolState::Failed | ToolState::Denied => (
                icon(IconName::Error, ui.error)
                    .size(px(14.))
                    .into_any_element(),
                tr("The agent failed").to_string(),
            ),
            ToolState::Interrupted => (
                icon(IconName::Stop, ui.dim)
                    .size(px(13.))
                    .into_any_element(),
                tr("The agent was stopped").to_string(),
            ),
            _ => (
                icon(IconName::CheckCircle, ui.success)
                    .size(px(14.))
                    .into_any_element(),
                tr("The agent is done — its answer went to Claude").to_string(),
            ),
        }
    };
    Some(
        claude_transcript::column(
            div()
                .h(px(44.))
                .flex()
                .items_center()
                .gap(px(8.))
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.text_muted)
                .child(mark)
                .child(div().flex_shrink().min_w_0().truncate().child(label))
                .children(totals.map(|totals| {
                    div()
                        .flex_shrink()
                        .min_w_0()
                        .truncate()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(ui.dim)
                        .child(totals)
                }))
                .child(div().flex_1())
                .child(ui::keycap("Esc", ui))
                .child(
                    div()
                        .flex_none()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(ui.dim)
                        .child(tr("back to the session")),
                ),
        )
        .flex_none()
        .border_t_1()
        .border_color(ui.divider)
        .into_any_element(),
    )
}

/// The crumb of the subagent in sight: its icon, «Agent · its task», its kind and model.
fn current_crumb(tool: &ToolEntry, description: &str, ui: UiColors) -> AnyElement {
    let (kind, model) = match &tool.call {
        ToolCall::Agent { subagent_type, .. } => (
            subagent_type.clone(),
            tool.input["model"].as_str().map(str::to_string),
        ),
        _ => (None, None),
    };
    let meta: Vec<String> = kind.into_iter().chain(model).collect();
    div()
        .flex_1()
        .min_w_0()
        .px(px(4.))
        .flex()
        .items_center()
        .gap(px(7.))
        .child(
            div()
                .flex_none()
                .size(px(20.))
                .rounded(px(5.))
                .flex()
                .items_center()
                .justify_center()
                .bg(UiColors::tint(ui.accent, 0.2))
                .child(icon(IconName::Agent, ui.accent_text).size(px(12.))),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(ui.foreground)
                .child(trf("Agent · {0}", &[&description])),
        )
        .when(!meta.is_empty(), |crumb| {
            crumb.child(
                div()
                    .flex_none()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(ui.dim)
                    .child(meta.join(" · ")),
            )
        })
        .into_any_element()
}

fn agent_description(tool: &ToolEntry) -> String {
    match &tool.call {
        ToolCall::Agent { description, .. } if !description.trim().is_empty() => {
            description.clone()
        }
        _ => tr("Agent").to_string(),
    }
}

fn chevron(ui: UiColors) -> AnyElement {
    div()
        .flex_none()
        .child(icon(IconName::ChevronRight, ui.dim).size(px(12.)))
        .into_any_element()
}
