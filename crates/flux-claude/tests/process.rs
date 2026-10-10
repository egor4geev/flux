//! A real `Process` against the fake CLI (`fake/fake-claude.py`, a recorded stream replayed):
//! the threads, the control requests and their answers, the permission round trip, the exit.

use std::path::PathBuf;

use flux_claude::process::ProcessEvent;
use flux_claude::protocol::Incoming;
use flux_claude::session::{PendingKind, ToolState};
use flux_claude::{Answer, Cli, LaunchOptions, Process, Session, Status, UserInput};
use futures::StreamExt;
use futures::executor::block_on;

fn fake(
    fixture: &str,
    cwd: PathBuf,
) -> Option<(
    Process,
    futures::channel::mpsc::UnboundedReceiver<ProcessEvent>,
)> {
    // The fake needs python3; without it there is nothing to test against.
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        return None;
    }
    let cli = Cli {
        path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fake/fake-claude.py"),
    };
    let options = LaunchOptions {
        cwd,
        extra_args: vec![
            format!("--fake-fixture={fixture}"),
            "--fake-delay-ms=0".into(),
        ],
        ..LaunchOptions::default()
    };
    Some(Process::spawn(&cli, &options).expect("the fake starts"))
}

#[test]
fn a_question_round_trip_through_the_process() {
    let dir = tempfile::tempdir().unwrap();
    let Some((process, mut events)) = fake("h_ask", dir.path().to_path_buf()) else {
        return;
    };
    let mut session = Session::new(dir.path().to_path_buf());
    block_on(async {
        let initialized = process.request(&Session::initialize_request());
        let response = initialized.await.unwrap().unwrap();
        session.initialized(&response);
        assert!(!session.commands.is_empty());
        let uuid = process.send_user(&UserInput {
            text: "Ask me a color".into(),
            ..UserInput::default()
        });
        session.push_user(Some(uuid), &UserInput::default());
        while let Some(event) = events.next().await {
            let request = match &event {
                ProcessEvent::Frame(frame) => match frame.as_ref() {
                    Incoming::Request { id, .. } => Some(id.clone()),
                    _ => None,
                },
                _ => None,
            };
            let is_result = matches!(&event, ProcessEvent::Frame(frame) if matches!(frame.as_ref(), Incoming::Result(_)));
            session.apply(&event);
            if let Some(id) = request {
                let pending = session
                    .pending(&id)
                    .cloned()
                    .expect("the question is pending");
                let PendingKind::Questions(questions) = &pending.kind else {
                    panic!("questions expected");
                };
                let answer = Answer::Answers(vec![(questions[0].question.clone(), "Green".into())]);
                process.respond(&id, Session::response(&pending, &answer));
                session.resolve(&id, &answer);
            }
            if is_result {
                break;
            }
        }
        let ask = session
            .tool(
                &session
                    .entries
                    .iter()
                    .find_map(|entry| match &entry.kind {
                        flux_claude::EntryKind::Tool(tool) => Some(tool.tool_use_id.clone()),
                        _ => None,
                    })
                    .unwrap(),
            )
            .unwrap()
            .clone();
        assert_eq!(ask.state, ToolState::Done);
        // The control request of the host gets its answer through its channel.
        let usage = process
            .request(&flux_claude::HostRequest::GetUsage)
            .await
            .unwrap()
            .unwrap();
        assert!(usage.get("rate_limits").is_some());
        process.close();
        // stdin closed: the process exits and says so last.
        let exited = loop {
            let event = events.next().await.expect("the events end with an exit");
            session.apply(&event);
            if let ProcessEvent::Exited { code, .. } = event {
                break code;
            }
        };
        assert_eq!(exited, Some(0));
    });
    assert!(matches!(session.status, Status::Exited { code: Some(0) }));
}

/// The fake with more arguments (a resumed session, the hook log).
fn fake_with(
    fixture: &str,
    cwd: PathBuf,
    resume: Option<&str>,
    more: &[String],
) -> Option<(
    Process,
    futures::channel::mpsc::UnboundedReceiver<ProcessEvent>,
)> {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .ok()?;
    let cli = Cli {
        path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fake/fake-claude.py"),
    };
    let mut extra_args = vec![
        format!("--fake-fixture={fixture}"),
        "--fake-delay-ms=0".into(),
    ];
    extra_args.extend_from_slice(more);
    let options = LaunchOptions {
        cwd,
        resume: resume.map(str::to_string),
        extra_args,
        ..LaunchOptions::default()
    };
    Some(Process::spawn(&cli, &options).expect("the fake starts"))
}

/// Flux's `flux` MCP server and the hook that lets its tools run: the call waits for the host,
/// whose answer becomes the tool's result.
#[test]
fn a_flux_tool_call_waits_for_the_host() {
    use flux_claude::mcp::{self, McpReply, McpServer, ToolOutput, ToolSpec};
    use flux_claude::protocol::CliRequest;

    let dir = tempfile::tempdir().unwrap();
    let Some((process, mut events)) = fake_with("s_flux_tool", dir.path().to_path_buf(), None, &[])
    else {
        return;
    };
    let server = McpServer::new(vec![ToolSpec {
        name: "get_diagnostics",
        title: "Get Diagnostics",
        description: "Problems.",
        input_schema: serde_json::json!({ "type": "object" }),
        read_only: true,
    }]);
    let mut session = Session::new(dir.path().to_path_buf());
    let mut hooks_allowed = 0;
    let mut call = None;
    block_on(async {
        let flux_claude::HostRequest::Initialize(mut fields) = Session::initialize_request() else {
            unreachable!()
        };
        fields["sdkMcpServers"] = serde_json::json!([mcp::SERVER]);
        fields["hooks"]["PreToolUse"] = serde_json::json!([{
            "matcher": "mcp__flux__.*",
            "hookCallbackIds": ["flux-tools"],
        }]);
        let initialized = process.request(&flux_claude::HostRequest::Initialize(fields));
        process.send_user(&UserInput {
            text: "What does the language server say?".into(),
            ..UserInput::default()
        });
        session.push_user(None, &UserInput::default());
        while let Some(event) = events.next().await {
            if let ProcessEvent::Frame(frame) = &event
                && let Incoming::Request { id, request } = frame.as_ref()
            {
                match request {
                    CliRequest::McpMessage { message, .. } => match server.handle(message) {
                        McpReply::Respond(response) => {
                            process.respond(id, serde_json::json!({ "mcp_response": response }))
                        }
                        McpReply::Call { id: rpc, name, arguments } => {
                            call = Some((name, arguments));
                            let output = ToolOutput::text("src/main.rs:3:5: error: expected `;`");
                            process.respond(
                                id,
                                serde_json::json!({ "mcp_response": McpServer::result(&rpc, &output) }),
                            );
                        }
                    },
                    CliRequest::HookCallback { callback_id, .. } => {
                        assert_eq!(callback_id, "flux-tools");
                        hooks_allowed += 1;
                        process.respond(
                            id,
                            serde_json::json!({ "hookSpecificOutput": {
                                "hookEventName": "PreToolUse", "permissionDecision": "allow" } }),
                        );
                    }
                    _ => {}
                }
            }
            let is_result = matches!(&event, ProcessEvent::Frame(frame) if matches!(frame.as_ref(), Incoming::Result(_)));
            session.apply(&event);
            if is_result {
                break;
            }
        }
        assert!(initialized.await.unwrap().is_ok());
    });
    assert_eq!(hooks_allowed, 1);
    let (name, arguments) = call.expect("the tool was called");
    assert_eq!(name, "get_diagnostics");
    assert!(arguments["path"].as_str().unwrap().ends_with("/src/main.rs"));
    let tool = session.tool("toolu_flux_diagnostics_1").unwrap();
    assert_eq!(tool.state, ToolState::Done);
    assert_eq!(
        tool.result.as_ref().unwrap().text,
        "src/main.rs:3:5: error: expected `;`"
    );
    assert!(
        session
            .info
            .mcp_servers
            .iter()
            .any(|(name, status)| name == "flux" && status == "connected")
    );
}

/// An allowed edit runs the host's PostToolUse hook before its result; a resumed session keeps its
/// id.
#[test]
fn an_allowed_edit_asks_the_edit_hook() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("hooks.jsonl");
    let Some((process, mut events)) = fake_with(
        "b_edit_allow",
        dir.path().to_path_buf(),
        Some("11111111-2222-4333-8444-555555555555"),
        &[format!("--fake-hook-log={}", log.display())],
    ) else {
        return;
    };
    let mut session = Session::new(dir.path().to_path_buf());
    let mut hook_input = None;
    block_on(async {
        let initialized = process.request(&Session::initialize_request());
        let uuid = process.send_user(&UserInput {
            text: "Edit it".into(),
            ..UserInput::default()
        });
        session.push_user(Some(uuid), &UserInput::default());
        while let Some(event) = events.next().await {
            let request = match &event {
                ProcessEvent::Frame(frame) => match frame.as_ref() {
                    Incoming::Request { id, request } => Some((id.clone(), request.clone())),
                    _ => None,
                },
                _ => None,
            };
            let is_result = matches!(&event, ProcessEvent::Frame(frame) if matches!(frame.as_ref(), Incoming::Result(_)));
            session.apply(&event);
            match request {
                Some((id, flux_claude::protocol::CliRequest::CanUseTool(_))) => {
                    let pending = session.pending(&id).cloned().unwrap();
                    let answer = Answer::Allow { remember: Vec::new() };
                    process.respond(&id, Session::response(&pending, &answer));
                    session.resolve(&id, &answer);
                }
                Some((id, flux_claude::protocol::CliRequest::HookCallback { callback_id, input })) => {
                    assert_eq!(callback_id, flux_claude::session::EDIT_HOOK);
                    hook_input = Some(input);
                    process.respond(
                        &id,
                        serde_json::json!({ "hookSpecificOutput": {
                            "hookEventName": "PostToolUse",
                            "additionalContext": "No problems." } }),
                    );
                }
                _ => {}
            }
            if is_result {
                break;
            }
        }
        assert!(initialized.await.unwrap().is_ok());
    });
    let input = hook_input.expect("the edit hook ran");
    assert_eq!(input["hook_event_name"], "PostToolUse");
    assert_eq!(input["tool_name"], "Edit");
    // The fake works in the real directory (`/private/var/…` behind `/var/…`).
    let root = dir.path().canonicalize().unwrap();
    assert!(
        input["tool_input"]["file_path"]
            .as_str()
            .unwrap()
            .starts_with(&*root.to_string_lossy())
    );
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(logged.contains("No problems."));
    assert_eq!(
        session.info.session_id.as_deref(),
        Some("11111111-2222-4333-8444-555555555555")
    );
}

/// A question without a session: one JSON result.
#[test]
fn a_question_without_a_session() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let cli = Cli {
        path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fake/fake-claude.py"),
    };
    let dir = tempfile::tempdir().unwrap();
    let answer = cli.ask(dir.path(), "Write a commit message", Some("haiku"));
    assert_eq!(
        answer.as_deref(),
        Ok("Make the greeting friendlier\n\nhello.py prints \"hello\" instead of \"hi\".")
    );
}
