//! The session model on recorded streams of `claude` 2.1.285 (`fixtures/*.jsonl`: the CLI's stdout
//! as it came, sanitized; `*.stdin.jsonl`: what the recording host wrote). The recordings' own
//! answers to permission prompts aren't on stdout, so a replay sees the CLI go on as if the user
//! had answered.

use std::path::PathBuf;

use flux_claude::process::ProcessEvent;
use flux_claude::protocol::{self, CliRequest, Incoming};
use flux_claude::session::{
    EntryKind, Notice, PendingKind, TaskStatus, ToolCall, ToolEntry, ToolState,
};
use flux_claude::{Answer, PermissionMode, Session, Status, UserInput};

fn frames(name: &str) -> Vec<Incoming> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.jsonl"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("{}: {err}", path.display()))
        .lines()
        .filter_map(protocol::parse_line)
        .collect()
}

/// Applies a frame as the window does (the initialize answer through `initialized`).
fn feed(session: &mut Session, frame: Incoming) {
    if let Incoming::Response {
        result: Ok(response),
        ..
    } = &frame
        && response.get("commands").is_some()
    {
        session.initialized(response);
        return;
    }
    session.apply(&ProcessEvent::Frame(Box::new(frame)));
}

fn replay(name: &str) -> Session {
    let mut session = Session::new(PathBuf::from("/project"));
    for frame in frames(name) {
        feed(&mut session, frame);
    }
    session
}

/// Replays up to and including the first frame `stop` accepts.
fn replay_until(name: &str, stop: impl Fn(&Incoming) -> bool) -> Session {
    let mut session = Session::new(PathBuf::from("/project"));
    for frame in frames(name) {
        let last = stop(&frame);
        feed(&mut session, frame);
        if last {
            break;
        }
    }
    session
}

fn is_request(frame: &Incoming, tool: &str) -> bool {
    matches!(frame, Incoming::Request { request: CliRequest::CanUseTool(permission), .. } if permission.tool_name == tool)
}

fn texts(session: &Session) -> Vec<String> {
    session
        .entries
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn tools<'a>(session: &'a Session, name: &str) -> Vec<&'a ToolEntry> {
    fn collect<'a>(entries: &'a [flux_claude::Entry], name: &str, out: &mut Vec<&'a ToolEntry>) {
        for entry in entries {
            if let EntryKind::Tool(tool) = &entry.kind {
                if tool.name == name {
                    out.push(tool);
                }
                collect(&tool.children, name, out);
            }
        }
    }
    let mut out = Vec::new();
    collect(&session.entries, name, &mut out);
    out
}

fn notices(session: &Session) -> Vec<&Notice> {
    session
        .entries
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::Notice(notice) => Some(notice),
            _ => None,
        })
        .collect()
}

fn turn_ends(session: &Session) -> usize {
    session
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, EntryKind::TurnEnd(_)))
        .count()
}

#[test]
fn a_plain_answer_streams_into_one_text() {
    let session = replay("a_text");
    assert!(!texts(&session).is_empty());
    assert!(texts(&session).iter().all(|text| !text.is_empty()));
    assert!(session.entries.iter().all(|entry| !matches!(
        entry.kind,
        EntryKind::Text {
            streaming: true,
            ..
        } | EntryKind::Thinking {
            streaming: true,
            ..
        }
    )));
    assert!(
        !session.commands.is_empty(),
        "commands come with initialize"
    );
    assert!(session.info.model.is_some());
    assert_eq!(session.status, Status::Idle);
    assert!(session.limits.is_some());
    assert_eq!(turn_ends(&session), 1);
    assert!(session.context.is_some());
}

#[test]
fn a_tool_s_input_shows_while_it_streams() {
    let session = replay_until("b_edit_allow", |frame| {
        matches!(frame, Incoming::Stream(event) if matches!(event.kind, protocol::StreamKind::InputDelta { .. }))
            && !matches!(frame, Incoming::Stream(event) if event.parent_tool_use_id.is_some())
    });
    let streaming = session
        .entries
        .iter()
        .find_map(|entry| match &entry.kind {
            EntryKind::Tool(tool) if tool.state == ToolState::Streaming => Some(tool),
            _ => None,
        })
        .expect("a tool call being streamed");
    assert!(
        matches!(&streaming.call, ToolCall::Read { path, .. } | ToolCall::Edit { path, .. } if !path.as_os_str().is_empty()),
        "{:?}",
        streaming.call
    );
}

#[test]
fn an_edit_runs_and_keeps_the_original() {
    let session = replay("b_edit_allow");
    let edit = tools(&session, "Edit")[0];
    assert!(matches!(edit.call, ToolCall::Edit { .. }));
    assert_eq!(edit.state, ToolState::Done);
    let structured = edit
        .result
        .as_ref()
        .and_then(|result| result.structured.as_ref());
    assert!(structured.is_some_and(|result| result.get("structuredPatch").is_some()));
    assert_eq!(session.changed_files.len(), 1);
    assert!(session.changed_files[0].original.is_some());
}

#[test]
fn a_changed_edit_runs_as_the_host_gave_it() {
    let session = replay("c_edit_modified");
    let edit = tools(&session, "Edit")[0];
    assert_eq!(edit.state, ToolState::Done);
    let result = edit.result.as_ref().unwrap().structured.as_ref().unwrap();
    assert_eq!(result["newString"], "color = green");
}

#[test]
fn a_denied_write_is_marked_denied() {
    let session = replay_until("d_write_deny", |frame| is_request(frame, "Write"));
    let pending = session.pending.last().expect("the write waits");
    assert!(matches!(&pending.kind, PendingKind::Edit(proposal) if proposal.proposed.is_ok()));
    assert_eq!(session.status, Status::WaitingForUser);
    let session = replay("d_write_deny");
    let write = tools(&session, "Write")[0];
    assert_eq!(write.state, ToolState::Denied);
}

#[test]
fn don_t_ask_mode_denies_with_a_notice() {
    let session = replay("d2_dontask");
    assert!(
        notices(&session)
            .iter()
            .any(|notice| matches!(notice, Notice::Denied { .. }))
    );
    assert!(
        tools(&session, "Write")
            .iter()
            .chain(tools(&session, "Bash").iter())
            .any(|tool| tool.state == ToolState::Denied)
    );
}

#[test]
fn an_always_allow_rule_skips_the_second_question() {
    let session = replay_until("e_bash_rule", |frame| is_request(frame, "Bash"));
    let pending = session.pending.last().expect("the first touch asks");
    assert!(!pending.suggestions.is_empty());
    assert!(matches!(pending.kind, PendingKind::Tool));
    let session = replay("e_bash_rule");
    let bash = tools(&session, "Bash");
    assert!(bash.len() >= 2);
    assert!(bash.iter().all(|tool| tool.state == ToolState::Done));
}

#[test]
fn an_interrupt_cuts_the_text_and_ends_the_turn() {
    let session = replay("f_interrupt");
    assert!(
        notices(&session)
            .iter()
            .any(|notice| matches!(notice, Notice::Interrupted))
    );
    let end = session.entries.iter().find_map(|entry| match &entry.kind {
        EntryKind::TurnEnd(summary) => Some(summary),
        _ => None,
    });
    assert_eq!(end.unwrap().reason.as_deref(), Some("aborted_streaming"));
}

#[test]
fn an_interrupt_withdraws_the_pending_question() {
    let session = replay("f2_interrupt_pending_permission");
    assert!(session.pending.is_empty());
    let write = tools(&session, "Write")[0];
    assert_eq!(write.state, ToolState::Interrupted);
    assert!(
        notices(&session)
            .iter()
            .any(|notice| matches!(notice, Notice::Interrupted))
    );
    assert_eq!(session.status, Status::Idle);
}

#[test]
fn tasks_follow_create_and_update() {
    let session = replay("g_tasks");
    assert!(session.tasks.len() >= 3, "tasks: {:?}", session.tasks);
    assert!(
        session
            .tasks
            .iter()
            .any(|task| task.status != TaskStatus::Pending)
    );
}

#[test]
fn todo_write_replaces_the_task_list() {
    let session = replay("g_todowrite_legacy");
    assert!(!session.tasks.is_empty());
}

#[test]
fn claude_s_question_waits_then_runs_with_the_answer() {
    let session = replay_until("h_ask", |frame| is_request(frame, "AskUserQuestion"));
    let pending = session.pending.first().expect("a pending question");
    let PendingKind::Questions(questions) = &pending.kind else {
        panic!("questions expected, got {:?}", pending.kind);
    };
    assert_eq!(questions[0].options.len(), 3);
    let response = Session::response(
        pending,
        &Answer::Answers(vec![(questions[0].question.clone(), "Green".into())]),
    );
    assert_eq!(
        response["updatedInput"]["answers"][questions[0].question.as_str()],
        "Green"
    );
    let session = replay("h_ask");
    assert_eq!(tools(&session, "AskUserQuestion")[0].state, ToolState::Done);
}

#[test]
fn the_plan_waits_for_approval() {
    let session = replay_until("i_plan", |frame| is_request(frame, "ExitPlanMode"));
    let pending = session.pending.last().expect("the plan");
    assert!(matches!(&pending.kind, PendingKind::Plan { plan, .. } if !plan.is_empty()));
    assert_eq!(session.info.permission_mode, PermissionMode::Plan);
    let approve = Session::response(pending, &Answer::ApprovePlan { accept_edits: true });
    assert_eq!(approve["updatedPermissions"][0]["mode"], "acceptEdits");
    let keep = Session::response(pending, &Answer::KeepPlanning("more".into()));
    assert_eq!(keep["behavior"], "deny");
    let session = replay("i_plan");
    assert_ne!(session.info.permission_mode, PermissionMode::Plan);
}

#[test]
fn a_rejected_plan_comes_again() {
    let session = replay("i_plan_feedback");
    let plans = tools(&session, "ExitPlanMode");
    assert!(plans.len() >= 2);
    assert_eq!(plans[0].state, ToolState::Denied);
}

#[test]
fn a_subagent_s_messages_nest_in_its_call() {
    let session = replay("j_subagent");
    let agent = session
        .entries
        .iter()
        .find_map(|entry| match &entry.kind {
            EntryKind::Tool(tool) if matches!(tool.call, ToolCall::Agent { .. }) => Some(tool),
            _ => None,
        })
        .expect("an Agent call");
    assert!(!agent.children.is_empty());
    let task = agent.task.as_ref().expect("the subagent's progress");
    assert_eq!(task.status.as_deref(), Some("completed"));
    assert!(task.last_tool.is_some());
    // The subagent's own tools are inside, not in the main conversation.
    assert!(session.entries.iter().all(|entry| !matches!(
        &entry.kind,
        EntryKind::Tool(tool) if tool.name == "Read"
    )));
}

#[test]
fn background_work_keeps_the_session_busy_until_idle() {
    let mut session = Session::new(PathBuf::from("/project"));
    let mut busy_after_result = false;
    for frame in frames("j_background") {
        let result = matches!(frame, Incoming::Result(_));
        feed(&mut session, frame);
        if result && turn_ends(&session) == 1 {
            busy_after_result = session.is_working();
        }
    }
    assert!(
        busy_after_result,
        "the first result isn't the end: background work goes on"
    );
    assert!(
        turn_ends(&session) >= 2,
        "the follow-up turn runs by itself"
    );
    assert!(session.background.is_empty());
    assert_eq!(session.status, Status::Idle);
    let bash = tools(&session, "Bash")[0];
    assert!(bash.task.as_ref().is_some_and(|task| task.background));
}

#[test]
fn local_commands_reply_as_notices() {
    let session = replay("l_slash");
    let locals: Vec<_> = notices(&session)
        .into_iter()
        .filter_map(|notice| match notice {
            Notice::LocalCommand { raw, .. } => Some(raw),
            _ => None,
        })
        .collect();
    assert!(locals.iter().any(|raw| raw.get("usage_report").is_some()));
    assert!(locals.iter().any(|raw| raw.get("context_usage").is_some()));
    assert!(
        notices(&session).iter().any(
            |notice| matches!(notice, Notice::Compacted { trigger, .. } if trigger == "manual")
        )
    );
    // Two model turns (the unknown command, the mention); local commands make none.
    assert_eq!(turn_ends(&session), 2);
}

#[test]
fn summarized_thinking_is_readable() {
    let session = replay("m_image");
    assert!(session.entries.iter().any(|entry| matches!(
        &entry.kind,
        EntryKind::Thinking { text, duration: Some(_), .. } if !text.is_empty()
    )));
}

#[test]
fn an_image_result_keeps_its_picture() {
    let session = replay("m2_read_image");
    let read = tools(&session, "Read")[0];
    assert!(!read.result.as_ref().unwrap().images.is_empty());
}

#[test]
fn hooks_and_mcp_requests_are_parsed() {
    assert!(frames("p_hooks").iter().any(|frame| matches!(
        frame,
        Incoming::Request {
            request: CliRequest::HookCallback { .. },
            ..
        }
    )));
    assert!(frames("q_sdk_mcp").iter().any(|frame| matches!(
        frame,
        Incoming::Request {
            request: CliRequest::McpMessage { .. },
            ..
        }
    )));
}

#[test]
fn web_tools_run() {
    let session = replay("r_web");
    assert!(!tools(&session, "WebFetch").is_empty() || !tools(&session, "WebSearch").is_empty());
}

#[test]
fn a_resumed_session_keeps_its_id() {
    let created = replay("n_resume_1_create");
    let resumed = replay("n_resume_2_resume");
    assert!(created.info.session_id.is_some());
    assert_eq!(created.info.session_id, resumed.info.session_id);
}

#[test]
fn queued_messages_stay_below_the_running_turn() {
    let mut session = Session::new(PathBuf::from("/project"));
    for frame in frames("o_queue").into_iter().take(1) {
        feed(&mut session, frame);
    }
    let first = UserInput {
        text: "count to 30".into(),
        ..UserInput::default()
    };
    session.push_user(Some("u1".into()), &first);
    let second = UserInput {
        text: "then say 4".into(),
        ..UserInput::default()
    };
    session.push_user(Some("u2".into()), &second);
    // Claude's answer to the first message goes above the queued second one.
    let text = |json: &str| protocol::parse_line(json).unwrap();
    feed(
        &mut session,
        text(r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}}}"#),
    );
    feed(
        &mut session,
        text(
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#,
        ),
    );
    feed(
        &mut session,
        text(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"1 2 3"}}}"#,
        ),
    );
    let kinds: Vec<&str> = session
        .entries
        .iter()
        .map(|entry| match &entry.kind {
            EntryKind::User(user) if user.queued => "queued",
            EntryKind::User(_) => "user",
            EntryKind::Text { .. } => "text",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["user", "text", "queued"]);
    // Its turn starts: the message is no longer queued and stays after the answer.
    feed(
        &mut session,
        text(r#"{"type":"command_lifecycle","command_uuid":"u2","state":"started"}"#),
    );
    let last = session.entries.last().unwrap();
    assert!(
        matches!(&last.kind, EntryKind::User(user) if !user.queued && user.text == "then say 4")
    );
    // A started message that ends "cancelled" (its turn was interrupted) isn't taken back.
    feed(
        &mut session,
        text(r#"{"type":"command_lifecycle","command_uuid":"u2","state":"cancelled"}"#),
    );
    assert!(
        matches!(&session.entries.last().unwrap().kind, EntryKind::User(user) if !user.cancelled)
    );
}

/// Synthetic frames of one message, in the shapes the CLI sends (`m_image`; Opus's thinking split
/// into blocks, as its transcripts show it: an empty block, then the summary 2–18 ms later).
mod wire {
    use flux_claude::protocol::{self, Incoming};
    use serde_json::{Value, json};

    fn stream(event: Value) -> Incoming {
        protocol::parse(json!({
            "type": "stream_event",
            "event": event,
            "parent_tool_use_id": null,
            "session_id": "s",
            "uuid": "u",
        }))
    }

    pub fn message_start() -> Incoming {
        stream(json!({
            "type": "message_start",
            "message": { "id": "msg_1", "type": "message", "role": "assistant", "content": [] },
        }))
    }

    pub fn block_start(index: usize, kind: &str) -> Incoming {
        let block = match kind {
            "thinking" => json!({ "type": "thinking", "thinking": "", "signature": "" }),
            _ => json!({ "type": "text", "text": "" }),
        };
        stream(json!({ "type": "content_block_start", "index": index, "content_block": block }))
    }

    pub fn thinking_delta(index: usize, text: &str) -> Incoming {
        stream(json!({
            "type": "content_block_delta",
            "index": index,
            "delta": { "type": "thinking_delta", "thinking": text },
        }))
    }

    pub fn text_delta(index: usize, text: &str) -> Incoming {
        stream(json!({
            "type": "content_block_delta",
            "index": index,
            "delta": { "type": "text_delta", "text": text },
        }))
    }

    pub fn block_stop(index: usize) -> Incoming {
        stream(json!({ "type": "content_block_stop", "index": index }))
    }

    /// One finished block, as the CLI sends each one (`parent` — a subagent's call).
    pub fn assistant(block: Value, parent: Option<&str>) -> Incoming {
        protocol::parse(json!({
            "type": "assistant",
            "message": {
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-5-5",
                "content": [block],
            },
            "parent_tool_use_id": parent,
            "session_id": "s",
            "uuid": "u",
        }))
    }

    pub fn thinking(text: &str) -> Value {
        json!({ "type": "thinking", "thinking": text, "signature": "sig" })
    }

    pub fn text(text: &str) -> Value {
        json!({ "type": "text", "text": text })
    }
}

/// The thoughts of the main conversation: their text and time.
fn thoughts(session: &Session) -> Vec<(String, Option<std::time::Duration>)> {
    session
        .entries
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::Thinking { text, duration, .. } => Some((text.clone(), *duration)),
            _ => None,
        })
        .collect()
}

fn feed_all(session: &mut Session, frames: Vec<Incoming>) {
    for frame in frames {
        feed(session, frame);
    }
}

#[test]
fn split_thinking_is_one_thought_that_took_the_whole_time() {
    let mut session = Session::new(PathBuf::from("/project"));
    // The model thinks in the first block (only token counts come), then the summary follows in
    // a second block a few milliseconds later.
    feed_all(
        &mut session,
        vec![wire::message_start(), wire::block_start(0, "thinking")],
    );
    std::thread::sleep(std::time::Duration::from_millis(40));
    feed_all(
        &mut session,
        vec![
            wire::assistant(wire::thinking(""), None),
            wire::block_stop(0),
            wire::block_start(1, "thinking"),
            wire::thinking_delta(1, "Checked the tests."),
            wire::assistant(wire::thinking("Checked the tests."), None),
            wire::block_stop(1),
            wire::block_start(2, "text"),
            wire::text_delta(2, "Done."),
            wire::assistant(wire::text("Done."), None),
            wire::block_stop(2),
        ],
    );
    let thoughts = thoughts(&session);
    assert_eq!(thoughts.len(), 1, "{thoughts:?}");
    let (text, duration) = &thoughts[0];
    assert_eq!(text, "Checked the tests.");
    assert!(duration.unwrap() >= std::time::Duration::from_millis(40));
    assert_eq!(texts(&session), ["Done."]);
}

#[test]
fn two_summaries_in_a_row_read_as_one_thought() {
    let mut session = Session::new(PathBuf::from("/project"));
    feed_all(
        &mut session,
        vec![
            wire::message_start(),
            wire::block_start(0, "thinking"),
            wire::thinking_delta(0, "First part."),
            wire::assistant(wire::thinking("First part."), None),
            wire::block_stop(0),
            wire::block_start(1, "thinking"),
            wire::thinking_delta(1, "Second"),
        ],
    );
    // While the second block streams, the thought streams again after a paragraph break.
    assert!(matches!(
        &session.entries[..],
        [flux_claude::Entry { kind: EntryKind::Thinking { text, streaming: true, .. }, .. }]
            if text == "First part.\n\nSecond"
    ));
    // The block's final text replaces only what streamed of it.
    feed_all(
        &mut session,
        vec![
            wire::assistant(wire::thinking("Second part."), None),
            wire::block_stop(1),
        ],
    );
    let thoughts = thoughts(&session);
    assert_eq!(thoughts.len(), 1);
    assert_eq!(thoughts[0].0, "First part.\n\nSecond part.");
    assert!(thoughts[0].1.is_some());
}

#[test]
fn an_empty_last_block_leaves_no_paragraph_break() {
    let mut session = Session::new(PathBuf::from("/project"));
    feed_all(
        &mut session,
        vec![
            wire::message_start(),
            wire::block_start(0, "thinking"),
            wire::thinking_delta(0, "A plan."),
            wire::assistant(wire::thinking("A plan."), None),
            wire::block_stop(0),
            wire::block_start(1, "thinking"),
            wire::assistant(wire::thinking(""), None),
            wire::block_stop(1),
        ],
    );
    assert_eq!(thoughts(&session)[0].0, "A plan.");
    // Cut off before the block's message: the turn's end drops the break as well.
    let mut session = Session::new(PathBuf::from("/project"));
    feed_all(
        &mut session,
        vec![
            wire::message_start(),
            wire::block_start(0, "thinking"),
            wire::assistant(wire::thinking("A plan."), None),
            wire::block_stop(0),
            wire::block_start(1, "thinking"),
            wire::message_start(),
        ],
    );
    let thoughts = thoughts(&session);
    assert_eq!(thoughts.len(), 1);
    assert_eq!(thoughts[0].0, "A plan.");
}

#[test]
fn thoughts_with_something_between_stay_apart() {
    let mut session = Session::new(PathBuf::from("/project"));
    feed_all(
        &mut session,
        vec![
            wire::message_start(),
            wire::block_start(0, "thinking"),
            wire::assistant(wire::thinking("First."), None),
            wire::block_stop(0),
            wire::block_start(1, "text"),
            wire::assistant(wire::text("Looking."), None),
            wire::block_stop(1),
            wire::message_start(),
            wire::block_start(0, "thinking"),
            wire::assistant(wire::thinking("Second."), None),
            wire::block_stop(0),
        ],
    );
    let thoughts: Vec<String> = thoughts(&session)
        .into_iter()
        .map(|(text, _)| text)
        .collect();
    assert_eq!(thoughts, ["First.", "Second."]);
}

#[test]
fn a_thought_that_came_whole_has_no_time() {
    // No stream (a saved conversation is read this way): the empty block and the summary make one
    // thought, and nothing says how long it took.
    let mut session = Session::new(PathBuf::from("/project"));
    feed_all(
        &mut session,
        vec![
            wire::assistant(wire::thinking(""), None),
            wire::assistant(wire::thinking("Summary."), None),
        ],
    );
    assert_eq!(thoughts(&session), [("Summary.".to_string(), None)]);
}

#[test]
fn a_subagent_s_split_thinking_is_one_thought_too() {
    let mut session = Session::new(PathBuf::from("/project"));
    let call = serde_json::json!({
        "type": "tool_use",
        "id": "toolu_agent",
        "name": "Agent",
        "input": { "description": "Review", "prompt": "Review the change" },
    });
    feed_all(
        &mut session,
        vec![
            wire::assistant(call, None),
            wire::assistant(wire::thinking(""), Some("toolu_agent")),
            wire::assistant(wire::thinking("Reading the diff."), Some("toolu_agent")),
        ],
    );
    let agent = tools(&session, "Agent")[0];
    let children: Vec<_> = agent
        .children
        .iter()
        .map(|entry| match &entry.kind {
            EntryKind::Thinking { text, duration, .. } => (text.as_str(), *duration),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(children, [("Reading the diff.", None)]);
}

#[test]
fn a_recorded_split_thought_replays_as_one() {
    // `t_split_thinking`: a synthetic stream in the shape of Opus's split thinking (token counts
    // only in the first block, the summary in the second), for the scenarios as well.
    let session = replay("t_split_thinking");
    let thoughts = thoughts(&session);
    assert_eq!(thoughts.len(), 1);
    assert_eq!(
        thoughts[0].0,
        "The tests fail on the date parser. The fixture uses a local time, so the parser needs an \
         explicit offset."
    );
    assert!(thoughts[0].1.is_some());
    assert_eq!(turn_ends(&session), 1);
}

/// A frame written the way the CLI writes it.
fn wire(json: &str) -> Incoming {
    protocol::parse_line(json).expect(json)
}

/// What the CLI (2.1.287) answers a message of context only (`shouldQuery: false`), as recorded.
fn context_taken(uuid: &str) -> Vec<Incoming> {
    vec![
        wire(&format!(
            r#"{{"type":"command_lifecycle","command_uuid":"{uuid}","state":"queued"}}"#
        )),
        wire(&format!(
            r#"{{"type":"command_lifecycle","command_uuid":"{uuid}","state":"started"}}"#
        )),
        wire(r#"{"type":"system","subtype":"init","session_id":"s1","model":"claude-haiku-4-5"}"#),
        wire(
            r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":16,"num_turns":0,"result":"","total_cost_usd":0}"#,
        ),
        wire(&format!(
            r#"{{"type":"command_lifecycle","command_uuid":"{uuid}","state":"completed"}}"#
        )),
    ]
}

#[test]
fn a_shell_command_s_records_are_context_not_a_turn() {
    use flux_claude::Change;
    use flux_claude::session::ShellState;
    let mut session = Session::new(PathBuf::from("/project"));
    feed(
        &mut session,
        wire(r#"{"type":"system","subtype":"session_state_changed","state":"idle"}"#),
    );
    let (id, _) = session.start_shell("cargo test".into());
    session.shell_output(id, "running 3 tests\n");
    session.shell_output(id, "10%\r50%\r100%\n");
    session.shell_ended(
        id,
        ShellState::Done,
        Some(101),
        Some(std::time::Duration::from_secs(2)),
    );
    session.shell_delivered(id, 2);
    let mut changes = Vec::new();
    let mut frames = vec![wire(
        r#"{"type":"system","subtype":"session_state_changed","state":"running"}"#,
    )];
    frames.extend(context_taken("in"));
    frames.extend(context_taken("out"));
    frames.push(wire(
        r#"{"type":"system","subtype":"session_state_changed","state":"idle"}"#,
    ));
    for frame in frames {
        changes.extend(session.apply(&ProcessEvent::Frame(Box::new(frame))));
    }
    // Claude didn't work: no line of a turn's end, no "finished", the status never moved.
    assert_eq!(turn_ends(&session), 0);
    assert_eq!(session.status, Status::Idle);
    assert!(
        !changes.iter().any(|change| matches!(
            change,
            Change::Status | Change::Idle | Change::TurnFinished { .. }
        )),
        "{changes:?}"
    );
    let EntryKind::Shell(shell) = &session.entries[0].kind else {
        panic!("{:?}", session.entries[0].kind);
    };
    assert_eq!(shell.output, "running 3 tests\n100%\n");
    assert_eq!(
        (shell.state, shell.exit_code, shell.delivered),
        (ShellState::Done, Some(101), true)
    );

    // The next message is a turn as always.
    session.push_user(
        Some("u1".into()),
        &UserInput {
            text: "Fix the failing test".into(),
            ..UserInput::default()
        },
    );
    let mut changes = Vec::new();
    for frame in [
        r#"{"type":"system","subtype":"session_state_changed","state":"running"}"#,
        r#"{"type":"system","subtype":"status","status":"requesting"}"#,
        r#"{"type":"assistant","message":{"id":"m1","role":"assistant","content":[{"type":"text","text":"Done."}]},"parent_tool_use_id":null}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":900,"num_turns":1,"result":"Done.","total_cost_usd":0.01}"#,
        r#"{"type":"system","subtype":"session_state_changed","state":"idle"}"#,
    ] {
        changes.extend(session.apply(&ProcessEvent::Frame(Box::new(wire(frame)))));
    }
    assert_eq!(turn_ends(&session), 1);
    assert!(changes.contains(&Change::Idle));
}

#[test]
fn a_long_shell_output_keeps_its_end() {
    let mut session = Session::new(PathBuf::from("/project"));
    let (id, _) = session.start_shell("yes".into());
    let line = "y".repeat(99) + "\n";
    for _ in 0..4000 {
        session.shell_output(id, &line);
    }
    session.shell_output(id, "the end\n");
    let EntryKind::Shell(shell) = &session.entries[0].kind else {
        unreachable!()
    };
    assert!(shell.output.len() < 300 * 1024, "{}", shell.output.len());
    assert!(shell.output.starts_with("…\n"));
    assert!(shell.output.ends_with("y\nthe end\n"));
}
