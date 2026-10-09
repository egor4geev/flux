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
