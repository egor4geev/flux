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
