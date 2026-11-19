//! A made-up hub for scenarios (`FLUX_HUB_FAKE=1`): sessions, events and replies without Claude, so
//! the plugin side of API 0.6 can be tried before the real hub ([`crate::claude_hub`]) has them.
//!
//! Three sessions in two projects; every call gets a canned reply. The first `watch` starts a
//! short story in the first session — it works, reads files, writes a message (the text grows),
//! asks to run a command and waits; an `answer` resolves the request and ends the turn with two
//! files changed. A message (`send`) or a new session with a prompt plays a shorter turn. The
//! story runs on a thread of its own that ends with it: nothing ticks while nothing happens.
//! `FLUX_HUB_FAKE_ACTION=<id>` plays the plugin's pult action at the second session after the
//! first watch; `FLUX_HUB_FAKE_PROJECTS` names the two projects (paths split by `:`).

use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flux_plugin::claude_remote::{
    AwakeMode, Limits, PermissionRequest, RegionState, RemoteAnswer, RemoteCall, RemoteEvent,
    RemoteMode, RemoteProject, RemoteReply, RemoteRequest, RemoteSession, RemoteSteps, RequestKind,
    RollbackCheck, SessionState, SystemEvent, SystemState, TranscriptItem, TurnFile, TurnSummary,
    broadcast, send_to,
};
use gpui::App;

/// The fake answers instead of the hub.
pub(crate) fn enabled() -> bool {
    std::env::var_os("FLUX_HUB_FAKE").is_some()
}

/// The made-up world.
struct Fake {
    sessions: Vec<RemoteSession>,
    /// The first watch started the story.
    told: bool,
    awake: AwakeMode,
    /// The session ids given so far.
    next: u32,
}

fn fake() -> &'static Mutex<Fake> {
    static FAKE: OnceLock<Mutex<Fake>> = OnceLock::new();
    FAKE.get_or_init(|| {
        Mutex::new(Fake {
            sessions: vec![
                session("fake-1", &project(0), "Кэш поиска", SessionState::Idle),
                session("fake-2", &project(0), "Тесты парсера", SessionState::Idle),
                RemoteSession {
                    window: None,
                    ..session(
                        "fake-3",
                        &project(1),
                        "Обзор README",
                        SessionState::Unloaded,
                    )
                },
            ],
            told: false,
            awake: AwakeMode::Off,
            next: 4,
        })
    })
}

/// The two projects: `FLUX_HUB_FAKE_PROJECTS` (paths split by `:`), else made-up ones.
fn project(index: usize) -> String {
    let projects: Vec<String> = std::env::var("FLUX_HUB_FAKE_PROJECTS")
        .ok()
        .map(|paths| paths.split(':').map(str::to_string).collect())
        .unwrap_or_else(|| {
            vec![
                "/Users/me/dev/project".into(),
                "/Users/me/dev/project2".into(),
            ]
        });
    projects
        .get(index)
        .or(projects.first())
        .cloned()
        .unwrap_or_default()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

fn session(id: &str, project: &str, title: &str, state: SessionState) -> RemoteSession {
    RemoteSession {
        id: id.into(),
        project: project.into(),
        title: title.into(),
        state,
        activity: String::new(),
        mode: RemoteMode::Ask,
        window: Some(1),
        updated: now() - 600,
        request: None,
        unseen: false,
    }
}

/// Changes a session and tells the watchers.
fn update(id: &str, change: impl FnOnce(&mut RemoteSession)) {
    let changed = {
        let mut fake = fake().lock().unwrap();
        let Some(session) = fake.sessions.iter_mut().find(|session| session.id == id) else {
            return;
        };
        change(session);
        session.updated = now();
        session.clone()
    };
    broadcast(RemoteEvent::Session(changed));
}

pub(crate) fn handle(plugin: &str, call: RemoteCall, reply: Sender<RemoteReply>, cx: &mut App) {
    let _ = cx;
    let answer = match call {
        RemoteCall::Watch(on) => {
            let first = on && !std::mem::replace(&mut fake().lock().unwrap().told, true);
            if first {
                play(story);
                // The pult's action of the plugin (`FLUX_HUB_FAKE_ACTION=<its id>`), as if the
                // user chose it at the second session.
                if let Ok(action) = std::env::var("FLUX_HUB_FAKE_ACTION") {
                    let plugin = plugin.to_string();
                    play(move || {
                        pause(1500);
                        send_to(
                            &plugin,
                            RemoteEvent::Action {
                                session: "fake-2".into(),
                                action,
                            },
                        );
                    });
                }
            }
            RemoteReply::Done
        }
        RemoteCall::Sessions => RemoteReply::Sessions(fake().lock().unwrap().sessions.clone()),
        RemoteCall::Transcript { session, count } => {
            let items = [
                (true, "Найди, почему поиск тормозит на больших проектах"),
                (
                    false,
                    "Поиск читает каждый файл заново. Добавлю кэш по времени изменения.",
                ),
                (true, "Давай, и прогони тесты"),
            ];
            let items: Vec<TranscriptItem> = items
                .iter()
                .map(|(user, text)| TranscriptItem {
                    user: *user,
                    text: format!("{text} ({session})"),
                })
                .collect();
            let skip = items.len().saturating_sub(count as usize);
            RemoteReply::Transcript(items.into_iter().skip(skip).collect())
        }
        RemoteCall::Send {
            session,
            text,
            images,
        } => {
            if !known(&session) {
                RemoteReply::Failed("No such session".into())
            } else {
                play(move || short_turn(&session, &text, images.len()));
                RemoteReply::Done
            }
        }
        RemoteCall::Answer {
            session,
            request,
            answer,
        } => {
            let waiting = fake().lock().unwrap().sessions.iter().any(|known| {
                known.id == session && known.request.as_ref().is_some_and(|r| r.id == request)
            });
            if waiting {
                play(move || answered(&session, &request, answer));
                RemoteReply::Done
            } else {
                RemoteReply::Failed("The request is gone".into())
            }
        }
        RemoteCall::Interrupt { session } => {
            update(&session, |session| {
                session.state = SessionState::Idle;
                session.activity.clear();
                session.request = None;
            });
            broadcast(RemoteEvent::TurnEnded {
                session,
                summary: TurnSummary {
                    turn: 9,
                    files: Vec::new(),
                    failed: true,
                    text: "Прервано.".into(),
                    seconds: 3,
                },
            });
            RemoteReply::Done
        }
        RemoteCall::SetMode { session, mode } => {
            if mode == RemoteMode::Other {
                RemoteReply::Failed("This mode can't be set".into())
            } else {
                update(&session, |session| session.mode = mode);
                RemoteReply::Done
            }
        }
        RemoteCall::Rename { session, title } => {
            if !known(&session) {
                RemoteReply::Failed("No such session".into())
            } else {
                update(&session, |session| session.title = title.trim().to_string());
                RemoteReply::Done
            }
        }
        RemoteCall::Close { session } => {
            if !known(&session) {
                RemoteReply::Failed("No such session".into())
            } else {
                fake()
                    .lock()
                    .unwrap()
                    .sessions
                    .retain(|known| known.id != session);
                broadcast(RemoteEvent::SessionGone(session));
                RemoteReply::Done
            }
        }
        RemoteCall::NewSession(spec) => {
            let id = {
                let mut fake = fake().lock().unwrap();
                let id = format!("fake-{}", fake.next);
                fake.next += 1;
                let mut new = session(
                    &id,
                    &spec.project,
                    spec.title.as_deref().unwrap_or("Новая сессия"),
                    SessionState::Idle,
                );
                new.mode = spec.mode;
                new.updated = now();
                fake.sessions.insert(0, new);
                id
            };
            update(&id, |_| {});
            if let Some(prompt) = spec.prompt {
                let session = id.clone();
                play(move || short_turn(&session, &prompt, spec.images.len()));
            }
            RemoteReply::Session(id)
        }
        RemoteCall::TurnDiff { session, turn } => RemoteReply::Text(format!(
            "--- a/src/search.rs\n+++ b/src/search.rs\n@@ -10,3 +10,5 @@\n fn search() {{\n-    \
             read_all();\n+    // {session}, turn {}\n+    cache.get_or_read();\n+    \
             cache.trim();\n }}\n",
            turn.map_or("last".to_string(), |turn| turn.to_string())
        )),
        RemoteCall::CheckRollback { .. } => RemoteReply::Rollback(RollbackCheck {
            files: vec!["src/search.rs".into(), "src/cache.rs".into()],
            changed_since: vec!["src/search.rs".into()],
            created: vec!["src/cache.rs".into()],
        }),
        RemoteCall::Rollback { session, .. } => {
            update(&session, |session| session.unseen = false);
            RemoteReply::Done
        }
        RemoteCall::Limits => RemoteReply::Limits(limits(37)),
        RemoteCall::Region { again } => {
            if again {
                broadcast(RemoteEvent::Region(RegionState::Checking));
                play(|| {
                    pause(800);
                    broadcast(RemoteEvent::Region(RegionState::Allowed));
                });
                RemoteReply::Region(RegionState::Checking)
            } else {
                RemoteReply::Region(RegionState::Allowed)
            }
        }
        RemoteCall::Projects => RemoteReply::Projects(
            [0, 1]
                .iter()
                .map(|index| {
                    let path = project(*index);
                    RemoteProject {
                        name: path.rsplit(['/', '\\']).next().unwrap_or("").to_string(),
                        open: *index == 0,
                        path,
                    }
                })
                .collect(),
        ),
        RemoteCall::SetAwake(mode) => {
            fake().lock().unwrap().awake = mode;
            RemoteReply::Done
        }
        RemoteCall::System => RemoteReply::System(SystemState {
            screen_locked: false,
            on_power: true,
        }),
    };
    let _ = reply.send(answer);
}

fn known(session: &str) -> bool {
    fake()
        .lock()
        .unwrap()
        .sessions
        .iter()
        .any(|known| known.id == session)
}

fn limits(five_hour: u8) -> Limits {
    Limits {
        five_hour: Some((five_hour, now() + 2 * 3600)),
        seven_day: Some((12, now() + 3 * 86400)),
        rejected: false,
    }
}

/// Plays a story on its own thread.
fn play(story: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new()
        .name("fake hub".into())
        .spawn(story);
}

fn pause(millis: u64) {
    std::thread::sleep(Duration::from_millis(millis));
}

/// Claude's message growing, then written to the end.
fn write(session: &str, message: u64, text: &str) {
    let words: Vec<&str> = text.split(' ').collect();
    let step = words.len().div_ceil(4).max(1);
    let mut shown = 0;
    while shown < words.len() {
        shown = (shown + step).min(words.len());
        broadcast(RemoteEvent::Text {
            session: session.into(),
            message,
            text: words[..shown].join(" "),
            done: shown == words.len(),
        });
        pause(260);
    }
}

/// The first session works, writes and asks to run the tests.
fn story() {
    let id = "fake-1";
    broadcast(RemoteEvent::System(SystemEvent::DidWake));
    pause(500);
    update(id, |session| {
        session.state = SessionState::Working;
        session.activity = "Читает 3 файла…".into();
    });
    let mut steps = RemoteSteps {
        group: 1,
        summary: "Прочитал 3 файла".into(),
        lines: vec![
            "src/search.rs".into(),
            "src/cache.rs".into(),
            "src/main.rs".into(),
        ],
        done: false,
    };
    broadcast(RemoteEvent::Steps {
        session: id.into(),
        steps: steps.clone(),
    });
    pause(400);
    write(
        id,
        1,
        "Поиск читает каждый файл заново. Добавил кэш по времени изменения файла — теперь \
         повторный поиск не трогает диск. Осталось прогнать тесты.",
    );
    steps.done = true;
    broadcast(RemoteEvent::Steps {
        session: id.into(),
        steps,
    });
    let request = RemoteRequest {
        id: "fake-request-1".into(),
        kind: RequestKind::Permission(PermissionRequest {
            title: "Claude хочет выполнить команду".into(),
            tool: "Bash".into(),
            subject: Some("cargo test -p flux-search".into()),
            diff: None,
            choices: vec![
                ("session".into(), "Разрешать в этой сессии".into()),
                (
                    "rule".into(),
                    "Разрешить и больше не спрашивать: cargo test".into(),
                ),
            ],
        }),
    };
    update(id, |session| {
        session.state = SessionState::Waiting;
        session.activity = "Ждёт разрешения".into();
        session.request = Some(request.clone());
    });
    broadcast(RemoteEvent::Request {
        session: id.into(),
        request,
    });
}

/// The answer to the story's request (or another one): resolved, then the turn ends.
fn answered(session: &str, request: &str, answer: RemoteAnswer) {
    update(session, |known| {
        known.request = None;
        known.state = SessionState::Working;
        known.activity = "Выполняет команду…".into();
    });
    broadcast(RemoteEvent::Resolved {
        session: session.into(),
        request: request.into(),
    });
    pause(600);
    let allowed = matches!(
        answer,
        RemoteAnswer::Allow { .. } | RemoteAnswer::ApprovePlan { .. } | RemoteAnswer::Answers(_)
    );
    let text = if allowed {
        "Тесты прошли: 48 из 48. Кэш на месте."
    } else {
        "Хорошо, тесты не запускаю. Изменения готовы к проверке."
    };
    write(session, 2, text);
    end_turn(session, 1, text, 42);
}

/// A message to a session: a short turn that echoes it.
fn short_turn(session: &str, text: &str, images: usize) {
    update(session, |known| {
        known.state = SessionState::Working;
        known.activity = "Думает…".into();
    });
    pause(400);
    let pictures = if images > 0 {
        format!(" (картинок: {images})")
    } else {
        String::new()
    };
    let reply = format!("Принято: «{text}»{pictures}. Сделано.");
    write(session, 10, &reply);
    end_turn(session, 2, &reply, 5);
}

fn end_turn(session: &str, turn: u32, text: &str, seconds: u32) {
    broadcast(RemoteEvent::TurnEnded {
        session: session.into(),
        summary: TurnSummary {
            turn,
            files: vec![
                TurnFile {
                    path: "src/search.rs".into(),
                    added: 12,
                    removed: 3,
                    created: false,
                },
                TurnFile {
                    path: "src/cache.rs".into(),
                    added: 40,
                    removed: 0,
                    created: true,
                },
            ],
            failed: false,
            text: text.into(),
            seconds,
        },
    });
    update(session, |known| {
        known.state = SessionState::Idle;
        known.activity.clear();
        known.unseen = true;
    });
    broadcast(RemoteEvent::Limits(limits(41)));
}
