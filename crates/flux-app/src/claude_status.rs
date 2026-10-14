//! A Claude session's status at a glance (part 10.2): what works in it — a command, subagents,
//! background tasks —, whether it waits for the user or finished out of sight. Its pill in the
//! Claude window and its editor tab show a dot with badges ("⌨ 12 s", "agents 2"), the tooltip
//! tells the details, and a click on them shows the session at the place that works or waits.

use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt, AnyElement, AnyView, App, Context, Div, ElementId, Entity, FontWeight,
    Render, Window, div, prelude::*, px,
};

use flux_claude::{Entry, EntryId, EntryKind, PendingKind, Session, Status, ToolCall, ToolState};

use crate::claude_chat::ClaudeChat;
use crate::claude_session::ClaudeSession;
use crate::claude_transcript::first_line;
use crate::i18n::{tr, trf, trn};
use crate::icons::{IconName, icon};
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_SM};

/// A command's line in the tooltip, at most.
const COMMAND_MAX_CHARS: usize = 60;

/// What a session does right now, read from its model.
#[derive(Debug, Clone, PartialEq)]
pub struct Glance {
    pub state: GlanceState,
    /// Working since: the turn's clock (the line under the conversation counts the same).
    pub since: Option<Instant>,
    /// A command of Claude that runs (Bash), the newest.
    pub command: Option<Running>,
    /// Subagents that run (Agent calls of the conversation, not nested ones).
    pub agents: Vec<Running>,
    /// Background tasks still running (commands and subagents started in the background).
    pub background: usize,
    /// What waits for the user: the question, or the tool and what it would do.
    pub waiting: Option<String>,
    /// Where a click goes: what waits, else the command, else a subagent.
    pub target: Option<EntryId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlanceState {
    Starting,
    Working,
    Waiting,
    Idle,
    /// The process ended on its own (exit code 0) or was stopped.
    Stopped,
    /// The process ended with an error.
    Failed,
}

/// A call that runs: its entry, its label (a command's first line, a subagent's task) and what it
/// does now (a subagent's last tool).
#[derive(Debug, Clone, PartialEq)]
pub struct Running {
    pub entry: EntryId,
    pub label: String,
    pub detail: Option<String>,
}

impl Glance {
    /// The session works: the clocks of its badge and tooltip tick (background tasks alone show a
    /// count, not a clock — a dev server may run for hours).
    pub fn is_live(&self) -> bool {
        matches!(self.state, GlanceState::Working | GlanceState::Starting)
    }

    /// How many badges [`glyph`] draws: the command (while working), subagents, background
    /// tasks.
    pub fn badges(&self) -> usize {
        usize::from(self.command.is_some() && self.state == GlanceState::Working)
            + usize::from(!self.agents.is_empty())
            + usize::from(self.background > 0)
    }
}

/// The session's status from its model.
pub fn glance(model: &Session) -> Glance {
    let state = match &model.status {
        Status::Starting => GlanceState::Starting,
        Status::Working { .. } if !model.pending.is_empty() => GlanceState::Waiting,
        Status::Working { .. } => GlanceState::Working,
        Status::WaitingForUser => GlanceState::Waiting,
        Status::Idle => GlanceState::Idle,
        Status::Exited { code } if *code != Some(0) && code.is_some() => GlanceState::Failed,
        Status::Exited { .. } => GlanceState::Stopped,
    };
    let since = match &model.status {
        Status::Working { since, .. } => Some(*since),
        _ => None,
    };
    let running = |entry: &&Entry| matches!(&entry.kind, EntryKind::Tool(tool) if tool.state == ToolState::Running);
    let command = model
        .entries
        .iter()
        .rev()
        .filter(running)
        .find_map(|entry| match &entry.kind {
            EntryKind::Tool(tool) => match &tool.call {
                ToolCall::Bash { command, .. } => Some(Running {
                    entry: entry.id,
                    label: clip(&first_line(command), COMMAND_MAX_CHARS),
                    detail: None,
                }),
                _ => None,
            },
            _ => None,
        });
    let agents: Vec<Running> = model
        .entries
        .iter()
        .filter(running)
        .filter_map(|entry| match &entry.kind {
            EntryKind::Tool(tool) => match &tool.call {
                ToolCall::Agent { description, .. } => Some(Running {
                    entry: entry.id,
                    label: description.clone(),
                    detail: tool
                        .task
                        .as_ref()
                        .and_then(|task| task.last_tool.clone())
                        .or_else(|| last_tool(&tool.children)),
                }),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let pending = model.pending.first();
    let waiting = pending.map(|pending| match &pending.kind {
        PendingKind::Questions(questions) => questions
            .first()
            .map(|question| question.question.clone())
            .unwrap_or_else(|| pending.display_name.clone()),
        PendingKind::Plan { .. } => tr("Claude's plan is ready").to_string(),
        _ => match &pending.description {
            Some(description) => format!("{}: {description}", pending.display_name),
            None => pending.display_name.clone(),
        },
    });
    let target = pending
        .and_then(|pending| find_call(&model.entries, &pending.tool_use_id))
        .or(command.as_ref().map(|command| command.entry))
        .or(agents.first().map(|agent| agent.entry));
    Glance {
        state,
        since,
        command,
        agents,
        background: model.background.len(),
        waiting,
        target,
    }
}

/// The entry of a tool call, by the CLI's id, at any depth (a subagent's question).
fn find_call(entries: &[Entry], tool_use_id: &str) -> Option<EntryId> {
    entries.iter().find_map(|entry| match &entry.kind {
        EntryKind::Tool(tool) if tool.tool_use_id == tool_use_id => Some(entry.id),
        EntryKind::Tool(tool) => find_call(&tool.children, tool_use_id),
        _ => None,
    })
}

/// A subagent's newest tool call, when its progress didn't name one.
fn last_tool(children: &[Entry]) -> Option<String> {
    children.iter().rev().find_map(|entry| match &entry.kind {
        EntryKind::Tool(tool) => Some(tool.name.clone()),
        _ => None,
    })
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(max.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

/// "12 s", "3 min", "2 h": a badge's clock.
pub fn compact_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    match seconds {
        0..60 => trf("{0} s", &[&seconds]),
        60..3600 => trf("{0} min", &[&(seconds / 60)]),
        _ => trf("{0} h", &[&(seconds / 3600)]),
    }
}

/// The dot of a session's status and its badges: a pulsing dot while it works (with what runs —
/// a command and its clock, subagents, background tasks), amber while it waits for the user, the
/// success color when it finished out of sight, a hollow ring when ready.
pub fn glyph(glance: &Glance, unseen: bool, id: impl Into<ElementId>, ui: UiColors) -> Div {
    let dot = div().flex_none().size(px(7.)).rounded(px(4.));
    let dot = match glance.state {
        GlanceState::Working | GlanceState::Starting => dot
            .bg(ui.accent)
            .with_animation(
                id,
                Animation::new(Duration::from_millis(1100)).repeat(),
                |dot, delta| dot.opacity(0.35 + 0.65 * (1. - (2. * delta - 1.).abs())),
            )
            .into_any_element(),
        GlanceState::Waiting => dot.bg(ui.warning).into_any_element(),
        GlanceState::Failed => dot.bg(ui.error).into_any_element(),
        GlanceState::Stopped => dot.bg(ui.dim).into_any_element(),
        GlanceState::Idle if unseen => dot.bg(ui.success).into_any_element(),
        GlanceState::Idle => dot
            .border_1()
            .border_color(UiColors::tint(ui.text_muted, 0.6))
            .into_any_element(),
    };
    let working = glance.state == GlanceState::Working;
    let clock = glance.since.map(|since| compact_elapsed(since.elapsed()));
    let command = glance.command.as_ref().filter(|_| working).map(|_| {
        badge(
            IconName::Terminal,
            clock.clone().unwrap_or_default(),
            true,
            ui,
        )
    });
    let agents = (!glance.agents.is_empty())
        .then(|| badge(IconName::Agent, glance.agents.len().to_string(), false, ui));
    let background = (glance.background > 0)
        .then(|| badge(IconName::Clock, glance.background.to_string(), false, ui));
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .child(
            div()
                .flex_none()
                .size(px(10.))
                .flex()
                .items_center()
                .justify_center()
                .child(dot),
        )
        .children(command)
        .children(agents)
        .children(background)
}

/// A small pill: an icon and a short text (a clock, a count).
fn badge(name: IconName, text: String, accent: bool, ui: UiColors) -> AnyElement {
    let color = if accent {
        ui.accent_text
    } else {
        ui.text_muted
    };
    let fill = if accent {
        UiColors::tint(ui.accent, 0.16)
    } else {
        UiColors::tint(ui.foreground, 0.07)
    };
    div()
        .flex_none()
        .h(px(16.))
        .px(px(5.))
        .flex()
        .items_center()
        .gap(px(3.))
        .rounded(px(8.))
        .bg(fill)
        .text_size(px(theme::TEXT_XS - 0.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(icon(name, color).size(px(10.)))
        .child(text)
        .into_any_element()
}

/// The tooltip of a session's status: its title, what runs or waits; `hint` — over the status
/// itself, where a click on it goes. It reads the session as it draws, so the clocks go on.
pub fn tooltip(
    session: Entity<ClaudeSession>,
    finished: Option<Instant>,
    hint: bool,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_, cx| {
        let tooltip = StatusTooltip {
            session: session.clone(),
            finished,
            hint,
        };
        cx.new(|_| tooltip).into()
    }
}

struct StatusTooltip {
    session: Entity<ClaudeSession>,
    finished: Option<Instant>,
    hint: bool,
}

impl Render for StatusTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let session = self.session.read(cx);
        let title = session.title();
        let glance = glance(session.model());
        let lines = lines(&glance, self.finished);
        let row = |name: IconName, color, text: String, time: Option<String>| {
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(icon(name, color).size(px(13.)))
                .child(div().child(text))
                .children(time.map(|time| div().text_color(ui.dim).child(time)))
        };
        let rows: Vec<AnyElement> = lines
            .into_iter()
            .map(|line| match line {
                Line::Command { label, time } => div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(row(
                        IconName::Terminal,
                        ui.accent_text,
                        trf("Running {0}…", &[&"Bash"]),
                        time,
                    ))
                    .child(
                        div()
                            .pl(px(19.))
                            .font_family(theme::code_font())
                            .text_size(px(theme::TEXT_XS))
                            .text_color(ui.text_muted)
                            .child(label),
                    )
                    .into_any_element(),
                Line::Agents { text, time } => {
                    row(IconName::Agent, ui.accent_text, text, time).into_any_element()
                }
                Line::Background(text) => {
                    row(IconName::Clock, ui.text_muted, text, None).into_any_element()
                }
                Line::Working { time } => row(
                    IconName::Claude,
                    ui.accent_text,
                    tr("Working…").to_string(),
                    time,
                )
                .into_any_element(),
                Line::Waiting(text) => {
                    row(IconName::Question, ui.warning, text, None).into_any_element()
                }
                Line::State(text) => div()
                    .text_color(ui.text_muted)
                    .child(text)
                    .into_any_element(),
            })
            .collect();
        let hint = glance.target.filter(|_| self.hint).map(|_| {
            div()
                .pt_0p5()
                .text_size(px(theme::TEXT_XS))
                .text_color(ui.dim)
                .child(tr("Click — to the place in the conversation"))
        });
        // The chrome of `ui::tooltip`, with lines.
        div().pl(px(10.)).pt(px(18.)).child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .max_w(px(420.))
                .px_2()
                .py_1p5()
                .rounded(px(RADIUS_SM))
                .bg(ui.elevated)
                .border_1()
                .border_color(ui.elevated_border)
                .shadow(ui::popover_shadow(ui))
                .font_family(theme::UI_FONT)
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.foreground)
                .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                .children(rows)
                .children(hint),
        )
    }
}

/// A line of the tooltip.
#[derive(Debug, Clone, PartialEq)]
enum Line {
    Command {
        label: String,
        time: Option<String>,
    },
    Agents {
        text: String,
        time: Option<String>,
    },
    Background(String),
    /// Working, nothing in particular runs (thinking, writing).
    Working {
        time: Option<String>,
    },
    Waiting(String),
    State(String),
}

/// One line for the list of sessions (⌄): what the session does.
pub fn summary(glance: &Glance) -> String {
    let time = glance.since.map(|since| compact_elapsed(since.elapsed()));
    let with_time = |text: String| match &time {
        Some(time) => format!("{text} · {time}"),
        None => text,
    };
    match glance.state {
        GlanceState::Waiting => tr("Waiting for your answer").to_string(),
        GlanceState::Working => match (&glance.command, glance.agents.len()) {
            (Some(_), _) => with_time(trf("Running {0}…", &[&"Bash"])),
            (None, 0) => with_time(tr("Working…").to_string()),
            (None, agents) => with_time(trn(agents, "{n} subagent works", "{n} subagents work")),
        },
        GlanceState::Starting => tr("Starting…").to_string(),
        GlanceState::Idle => tr("Ready").to_string(),
        GlanceState::Stopped | GlanceState::Failed => tr("Stopped").to_string(),
    }
}

fn lines(glance: &Glance, finished: Option<Instant>) -> Vec<Line> {
    let time = glance.since.map(|since| compact_elapsed(since.elapsed()));
    let mut lines = Vec::new();
    match glance.state {
        GlanceState::Waiting => lines.push(Line::Waiting(match &glance.waiting {
            Some(what) => trf("Waiting for your answer: {0}", &[what]),
            None => tr("Waiting for your answer").to_string(),
        })),
        GlanceState::Working => {
            if let Some(command) = &glance.command {
                lines.push(Line::Command {
                    label: command.label.clone(),
                    time: time.clone(),
                });
            }
        }
        _ => {}
    }
    if !glance.agents.is_empty() {
        let tools: Vec<&str> = glance
            .agents
            .iter()
            .filter_map(|agent| agent.detail.as_deref())
            .collect();
        let mut text = trn(
            glance.agents.len(),
            "{n} subagent works",
            "{n} subagents work",
        );
        if !tools.is_empty() {
            text = format!("{text}: {}", tools.join(", "));
        }
        lines.push(Line::Agents {
            text,
            time: time.clone().filter(|_| glance.command.is_none()),
        });
    }
    if glance.background > 0 {
        lines.push(Line::Background(trn(
            glance.background,
            "{n} background task",
            "{n} background tasks",
        )));
    }
    if glance.state == GlanceState::Working && lines.is_empty() {
        lines.push(Line::Working { time });
    }
    match glance.state {
        GlanceState::Idle => lines.push(Line::State(match finished {
            Some(at) => format!(
                "{} · {}",
                tr("Done"),
                trf("{0} ago", &[&compact_elapsed(at.elapsed())])
            ),
            None => tr("Ready").to_string(),
        })),
        GlanceState::Starting => lines.push(Line::State(tr("Starting…").to_string())),
        GlanceState::Stopped | GlanceState::Failed => {
            lines.push(Line::State(tr("Stopped").to_string()))
        }
        _ => {}
    }
    lines
}

impl ClaudeChat {
    /// A click on the session's status: the conversation scrolls to what works or waits (`id`);
    /// a subagent's view in sight goes back to the session first.
    pub(crate) fn reveal_activity(
        &mut self,
        id: EntryId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        while !self.agents.is_empty() {
            self.agent_back(window, cx);
        }
        self.transcript.reveal(id);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use flux_claude::{Activity, ToolEntry};
    use serde_json::json;

    use super::*;

    fn tool(id: EntryId, name: &str, input: serde_json::Value, state: ToolState) -> Entry {
        Entry {
            id,
            kind: EntryKind::Tool(Box::new(ToolEntry {
                tool_use_id: format!("toolu_{id}"),
                name: name.to_string(),
                call: ToolCall::parse(name, &input),
                input,
                state,
                result: None,
                children: Vec::new(),
                task: None,
            })),
        }
    }

    #[test]
    fn a_running_command_and_subagents_show() {
        let mut model = Session::new(PathBuf::from("/project"));
        model.status = Status::Working {
            since: Instant::now(),
            activity: Activity::Tool("Bash".into()),
        };
        model.entries = vec![
            tool(
                1,
                "Read",
                json!({"file_path": "/project/a.rs"}),
                ToolState::Done,
            ),
            tool(
                2,
                "Bash",
                json!({"command": "cargo test\n--quiet"}),
                ToolState::Running,
            ),
            tool(
                3,
                "Agent",
                json!({"description": "Find calls", "prompt": "…"}),
                ToolState::Running,
            ),
            tool(
                4,
                "Agent",
                json!({"description": "Old", "prompt": "…"}),
                ToolState::Done,
            ),
        ];
        let glance = glance(&model);
        assert_eq!(glance.state, GlanceState::Working);
        // A command of several lines: its first one, marked as cut.
        assert_eq!(
            glance.command.as_ref().map(|c| c.label.as_str()),
            Some("cargo test …")
        );
        assert_eq!(glance.agents.len(), 1);
        assert_eq!(glance.agents[0].label, "Find calls");
        assert_eq!(glance.target, Some(2));
        assert!(glance.is_live());
    }

    #[test]
    fn an_idle_session_has_nothing_to_show() {
        let mut model = Session::new(PathBuf::from("/project"));
        model.status = Status::Idle;
        model.entries = vec![tool(1, "Bash", json!({"command": "ls"}), ToolState::Done)];
        let glance = glance(&model);
        assert_eq!(glance.state, GlanceState::Idle);
        assert_eq!(glance.command, None);
        assert_eq!(glance.target, None);
        assert!(!glance.is_live());
        model.status = Status::Exited { code: Some(1) };
        assert_eq!(super::glance(&model).state, GlanceState::Failed);
        model.status = Status::Exited { code: Some(0) };
        assert_eq!(super::glance(&model).state, GlanceState::Stopped);
    }

    #[test]
    fn clocks_read_short() {
        assert_eq!(compact_elapsed(Duration::from_secs(12)), "12 s");
        assert_eq!(compact_elapsed(Duration::from_secs(150)), "2 min");
        assert_eq!(compact_elapsed(Duration::from_secs(7300)), "2 h");
    }
}
