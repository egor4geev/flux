//! Saved sessions: the history's list (the head and the tail of each transcript) and a whole
//! conversation read back, on a synthetic transcript (`fixtures/transcripts`) — never the user's.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use flux_claude::session::{EntryKind, Notice, ToolState};
use flux_claude::transcript::{self, project_dir};
use flux_claude::{Session, Status};

const SAMPLE: &str = "5f0c1d2e-0000-4000-8000-000000000001";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcripts")
}

fn sample() -> Session {
    transcript::load(
        &fixtures().join(format!("{SAMPLE}.jsonl")),
        PathBuf::from("/project/sample"),
    )
    .unwrap()
}

/// What the chat shows, line by line, in short.
fn outline(entries: &[flux_claude::Entry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| match &entry.kind {
            EntryKind::User(user) => format!("user: {}", user.text),
            EntryKind::Text { text, .. } => format!("text: {text}"),
            EntryKind::Thinking { text, .. } => format!("thinking: {text}"),
            EntryKind::Tool(tool) => format!(
                "tool: {} {:?}{}",
                tool.name,
                tool.state,
                if tool.children.is_empty() {
                    String::new()
                } else {
                    format!(" {:?}", outline(&tool.children))
                }
            ),
            EntryKind::Notice(Notice::LocalCommand { text, .. }) => format!("output: {text}"),
            EntryKind::Notice(Notice::Error { kind, .. }) => format!("error: {kind}"),
            EntryKind::Notice(notice) => format!("notice: {notice:?}")
                .split([' ', '{'])
                .take(2)
                .collect::<Vec<_>>()
                .join(" "),
            EntryKind::TurnEnd(summary) => format!(
                "turn end: {} s{}",
                summary.duration.as_secs(),
                if summary.is_error { " stopped" } else { "" }
            ),
        })
        .collect()
}

#[test]
fn a_saved_conversation_reads_back_as_the_chat_shows_it() {
    let session = sample();
    assert_eq!(
        outline(&session.entries),
        [
            "user: Add a greeting to hello.py",
            "thinking: Let me look at the file.",
            "tool: Read Done",
            "tool: Edit Done",
            "text: Done — hello.py now prints hello.",
            "turn end: 25 s",
            "user: /model haiku",
            "output: Set model to Haiku 4.5",
            "user: Now write notes.md",
            "tool: Write Done",
            "text: Created notes.md.",
            "turn end: 12 s",
            "user: Also start a subagent to review",
            "tool: Agent Done [\"tool: Read Done\", \"text: Looks fine.\"]",
            "user: and check notes too",
            "text: Checked notes too.",
            "notice: Interrupted",
            "turn end: 30 s stopped",
            "notice: Compacted",
            "user: What's next?",
            "error: rate_limit",
        ]
    );
    assert_eq!(session.status, Status::Idle);
    assert!(session.pending.is_empty());
    assert_eq!(
        session.info.session_id.as_deref(),
        Some("5f0c1d2e-0000-4000-8000-000000000001")
    );
    // The user's title wins over the CLI's.
    assert_eq!(session.info.title.as_deref(), Some("Greeting in hello.py"));
    assert_eq!(
        session.info.model.as_deref(),
        Some("claude-haiku-4-5-20251001")
    );
}

#[test]
fn tool_results_keep_their_structure_and_files_their_originals() {
    let session = sample();
    let edit = session.tool("toolu_edit").expect("the edit");
    assert_eq!(edit.state, ToolState::Done);
    let structured = edit.result.as_ref().unwrap().structured.as_ref().unwrap();
    assert_eq!(structured["structuredPatch"][0]["lines"][1], "+print('hello')");

    let at = |minutes: u64, seconds: u64| {
        // 2026-10-09T10:MM:SSZ
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_791_540_000 + minutes * 60 + seconds)
    };
    let files: Vec<_> = session
        .changed_files
        .iter()
        .map(|file| {
            (
                file.path.to_string_lossy().into_owned(),
                file.original.clone(),
                file.changed_at,
            )
        })
        .collect();
    assert_eq!(
        files,
        [
            (
                "/project/sample/hello.py".to_string(),
                Some("print('hi')\n".to_string()),
                at(0, 20)
            ),
            ("/project/sample/notes.md".to_string(), None, at(1, 10)),
        ]
    );
}

#[test]
fn a_session_that_went_on_keeps_its_own_entries_after_the_saved_ones() {
    let mut live = Session::new(PathBuf::from("/project/sample"));
    live.push_user(
        Some("u-live".into()),
        &flux_claude::UserInput {
            text: "Go on".into(),
            images: Vec::new(),
            priority: None,
        },
    );
    let saved = sample();
    let saved_count = saved.entries.len();
    live.prepend_history(saved);
    assert_eq!(live.entries.len(), saved_count + 1);
    assert!(matches!(&live.entries.last().unwrap().kind, EntryKind::User(user) if user.text == "Go on"));
    // Every id is still unique, the subagent's entries included.
    let mut ids = Vec::new();
    fn collect(entries: &[flux_claude::Entry], ids: &mut Vec<u64>) {
        for entry in entries {
            ids.push(entry.id);
            if let EntryKind::Tool(tool) = &entry.kind {
                collect(&tool.children, ids);
            }
        }
    }
    collect(&live.entries, &mut ids);
    let count = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), count);
    assert_eq!(live.info.title.as_deref(), Some("Greeting in hello.py"));
    assert_eq!(live.changed_files.len(), 2);
    // The live session is still working on its message.
    assert!(live.is_working());
}

/// A transcript in the history's folder of `root`.
fn write(projects: &Path, root: &Path, id: &str, lines: &[String], modified: SystemTime) {
    let dir = project_dir(projects, root);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{id}.jsonl"));
    let mut file = File::create(&path).unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
    file.set_modified(modified).unwrap();
}

#[test]
fn the_history_lists_sessions_from_their_head_and_tail() {
    let projects = tempfile::tempdir().unwrap();
    let root = Path::new("/Users/me/dev/app");
    let now = SystemTime::now();
    let day = Duration::from_secs(86_400);

    // The sample: the user's title from the tail.
    let sample = fs::read_to_string(fixtures().join(format!("{SAMPLE}.jsonl"))).unwrap();
    let lines: Vec<String> = sample.lines().map(str::to_string).collect();
    write(projects.path(), root, SAMPLE, &lines, now - day);

    // A long one: the first prompt at the start, the title after a megabyte of other lines.
    let mut long = vec![
        r#"{"type":"queue-operation","operation":"enqueue","sessionId":"long"}"#.to_string(),
        r#"{"type":"user","uuid":"x1","parentUuid":null,"isSidechain":false,"gitBranch":"feature/x","message":{"role":"user","content":"Refactor the parser\nso that it streams"}}"#.to_string(),
    ];
    let filler = format!(
        r#"{{"type":"attachment","uuid":"f","parentUuid":"x1","attachment":{{"type":"hook_success","stdout":"{}"}}}}"#,
        "x".repeat(1000)
    );
    long.extend(std::iter::repeat_n(filler, 1200));
    long.push(r#"{"type":"ai-title","aiTitle":"Streaming parser","sessionId":"long"}"#.into());
    write(projects.path(), root, "long", &long, now);

    // Without a title: the first prompt stands in.
    let untitled = vec![
        r#"{"type":"user","uuid":"y1","parentUuid":null,"message":{"role":"user","content":"<command-name>/model</command-name>"}}"#.to_string(),
        r#"{"type":"user","uuid":"y2","parentUuid":"y1","message":{"role":"user","content":[{"type":"text","text":"Explain the build script"}]}}"#.to_string(),
    ];
    write(projects.path(), root, "untitled", &untitled, now - 2 * day);

    // Nothing the user wrote: not listed.
    let empty = vec![r#"{"type":"queue-operation","operation":"enqueue","sessionId":"empty"}"#.to_string()];
    write(projects.path(), root, "empty", &empty, now - 3 * day);

    // Another project's session isn't listed.
    write(projects.path(), Path::new("/Users/me/dev/other"), "other", &untitled, now);

    let sessions = transcript::list(projects.path(), root);
    let summary: Vec<_> = sessions
        .iter()
        .map(|session| {
            (
                session.id.as_str(),
                session.title.as_str(),
                session.prompt.as_deref(),
                session.branch.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (
                "long",
                "Streaming parser",
                Some("Refactor the parser\nso that it streams"),
                Some("feature/x")
            ),
            (
                SAMPLE,
                "Greeting in hello.py",
                Some("Add a greeting to hello.py"),
                Some("main")
            ),
            (
                "untitled",
                "Explain the build script",
                Some("Explain the build script"),
                None
            ),
        ]
    );
    assert!(sessions[0].size > 1_000_000);
    assert_eq!(transcript::list(projects.path(), Path::new("/nowhere")), []);
}

/// How long reading takes on real data — prints only sizes, counts and times:
/// `FLUX_TRANSCRIPT=<a .jsonl> FLUX_TRANSCRIPT_ROOT=<its project root> cargo test --release -p
/// flux-claude --test transcript timing -- --ignored --nocapture`.
#[test]
#[ignore]
fn timing() {
    if let Some(path) = std::env::var_os("FLUX_TRANSCRIPT").map(PathBuf::from) {
        let started = std::time::Instant::now();
        let session = transcript::load(&path, PathBuf::from("/")).unwrap();
        println!(
            "load: {} bytes → {} entries, {} changed files in {:?}",
            fs::metadata(&path).unwrap().len(),
            session.entries.len(),
            session.changed_files.len(),
            started.elapsed()
        );
    }
    if let Some(root) = std::env::var_os("FLUX_TRANSCRIPT_ROOT").map(PathBuf::from) {
        let projects = transcript::projects_dir().unwrap();
        let started = std::time::Instant::now();
        let sessions = transcript::list(&projects, &root);
        println!(
            "list: {} sessions in {:?}",
            sessions.len(),
            started.elapsed()
        );
    }
}
