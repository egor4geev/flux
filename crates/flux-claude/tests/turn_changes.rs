//! The files a turn changed, for the "Done" card at its end (part 10.2).

use std::path::PathBuf;

use flux_claude::Session;
use flux_claude::process::ProcessEvent;
use flux_claude::protocol::{self, Incoming};
use flux_claude::session::{EntryKind, TurnFile};

fn replay(name: &str) -> Session {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.jsonl"));
    let mut session = Session::new(PathBuf::from("/project"));
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
    session
}

fn turn_ends(session: &Session) -> Vec<u64> {
    session
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, EntryKind::TurnEnd(_)))
        .map(|entry| entry.id)
        .collect()
}

#[test]
fn an_edit_turn_lists_its_file_with_counts() {
    let session = replay("b_edit_allow");
    let ends = turn_ends(&session);
    let files = session.turn_changes(*ends.last().expect("a turn end"));
    assert_eq!(files.len(), 1, "{files:?}");
    let TurnFile {
        path,
        added,
        removed,
        created,
    } = &files[0];
    assert!(path.ends_with("greeting.txt"), "{}", path.display());
    assert!(*added >= 1 && *removed >= 1, "+{added} −{removed}");
    assert!(!created);
}

#[test]
fn a_turn_without_edits_lists_nothing() {
    let session = replay("a_text");
    for end in turn_ends(&session) {
        assert!(session.turn_changes(end).is_empty());
    }
    // Not a turn end at all: nothing either.
    assert!(session.turn_changes(u64::MAX).is_empty());
}
