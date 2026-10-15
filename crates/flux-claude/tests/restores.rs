//! Putting files back as they were before Claude's edits (part 10.2): rewinding the code to a
//! message, rolling back a turn or one edit; and cutting the conversation at an edited message.

use std::path::PathBuf;

use flux_claude::process::ProcessEvent;
use flux_claude::protocol::{self, Incoming};
use flux_claude::session::EntryKind;
use flux_claude::{Session, Status, UserInput};

/// A recorded session after a message of the user (the CLI doesn't echo it).
fn replay_after_prompt(name: &str) -> (Session, u64) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.jsonl"));
    let mut session = Session::new(PathBuf::from("/project"));
    session.push_user(
        Some("u1".into()),
        &UserInput {
            text: "go".into(),
            ..UserInput::default()
        },
    );
    let prompt = session.entries[0].id;
    for frame in std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("{}: {err}", path.display()))
        .lines()
        .filter_map(protocol::parse_line)
    {
        if let Incoming::Response {
            result: Ok(response),
            ..
        } = &frame
            && response.get("commands").is_some()
        {
            session.initialized(response);
            continue;
        }
        session.apply(&ProcessEvent::Frame(Box::new(frame)));
    }
    (session, prompt)
}

fn find(session: &Session, pick: impl Fn(&EntryKind) -> bool) -> u64 {
    session
        .entries
        .iter()
        .find(|entry| pick(&entry.kind))
        .map(|entry| entry.id)
        .expect("the entry")
}

#[test]
fn an_edit_is_known_before_and_after() {
    let (session, prompt) = replay_after_prompt("b_edit_allow");
    let edit = find(
        &session,
        |kind| matches!(kind, EntryKind::Tool(tool) if tool.name == "Edit"),
    );
    let restore = session.edit_restore(edit).expect("the edit's texts");
    assert!(
        restore.path.ends_with("greeting.txt"),
        "{}",
        restore.path.display()
    );
    let before = restore.before.as_deref().expect("the original");
    let after = restore.after.as_deref().expect("the result");
    assert!(before.contains("Hello world"), "{before:?}");
    assert!(
        after.contains("Hello Flux") && !after.contains("Hello world"),
        "{after:?}"
    );
    // The whole turn, and everything after the prompt: the same one file.
    let end = find(&session, |kind| matches!(kind, EntryKind::TurnEnd(_)));
    assert_eq!(session.turn_restores(end), vec![restore.clone()]);
    assert_eq!(session.restores_after(prompt), vec![restore]);
    // Not an edit, not an entry.
    assert_eq!(session.edit_restore(end), None);
    assert_eq!(session.edit_restore(9_999), None);
}

#[test]
fn a_refused_edit_changes_nothing() {
    let (session, prompt) = replay_after_prompt("d_write_deny");
    assert!(session.restores_after(prompt).is_empty());
}

#[test]
fn the_conversation_is_cut_at_an_edited_message() {
    let (mut session, prompt) = replay_after_prompt("b_edit_allow");
    assert!(session.entries.len() > 2);
    let changes = session.truncate_from(prompt);
    assert!(!changes.is_empty());
    assert!(session.entries.is_empty());
    assert!(session.pending.is_empty());
    assert_eq!(session.status, Status::Starting);
    // An id that isn't there changes nothing.
    assert!(session.truncate_from(prompt).is_empty());
}

#[test]
fn a_message_s_parent_is_read_from_the_transcript() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcripts");
    let path = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .expect("the synthetic transcript");
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let user = lines
        .iter()
        .find(|line| line["type"] == "user" && line["parentUuid"].is_string())
        .expect("a user line with a parent");
    let uuid = user["uuid"].as_str().unwrap();
    assert_eq!(
        flux_claude::transcript::parent_of(&path, uuid).unwrap(),
        Some(Some(user["parentUuid"].as_str().unwrap().to_string()))
    );
    let first = lines
        .iter()
        .find(|line| line["uuid"].is_string() && line["parentUuid"].is_null())
        .expect("the first line of the chain");
    assert_eq!(
        flux_claude::transcript::parent_of(&path, first["uuid"].as_str().unwrap()).unwrap(),
        Some(None)
    );
    assert_eq!(
        flux_claude::transcript::parent_of(&path, "no-such-uuid").unwrap(),
        None
    );
}
