//! The probe's commands for the API 0.6 (the bot, parts 1 and 3): `remote:<name>` — Claude for
//! remote work, tasks between plugins and their actions, the quiet Git calls, a session's id. As
//! a source of tasks it gives two made-up ones, and runs an action by answering at once with a
//! made-up session named after its instance ("window-P-1") — or never ("silent").

use flux_plugin_api::tasks::{self, Query, TaskRun};
use flux_plugin_api::{CommandContext, Event, claude, git, notify, remote, service};

pub fn run(command: &str, context: &CommandContext) {
    let argument = context.argument.clone().unwrap_or_default();
    match command {
        // "source|key|action|project"
        "run" => {
            let parts: Vec<&str> = argument.split('|').collect();
            let [source, key, action, project] = parts[..] else {
                return say("run: a bad argument");
            };
            let run = TaskRun {
                source: source.into(),
                key: key.into(),
                action: action.into(),
                project: project.into(),
            };
            match tasks::run_with(&run) {
                Ok(started) => say(&format!(
                    "run: {} [{}]",
                    started.session,
                    started.notes.join("; ")
                )),
                Err(err) => say(&format!("run failed: {err}")),
            }
        }
        // Answers a run that isn't this plugin's: the query's id.
        "started" => {
            let id = argument.parse().unwrap_or_default();
            tasks::started(id, Ok(tasks::opened("stolen")));
            say("started sent");
        }
        "claude" => match claude::session().prompt("Hi").send().start() {
            Ok(id) => say(&format!("claude session: {id}")),
            Err(err) => say(&format!("claude failed: {err}")),
        },
        "new-branch-quietly" => {
            let id = git::new_branch_quietly(&argument, "TRACK-1-fix", None);
            say(&format!("quiet new-branch: {id}"));
        }
        "checkout-quietly" => {
            let id = git::checkout_quietly(&argument, "main");
            say(&format!("quiet checkout: {id}"));
        }
        "watch" => report("watching", remote::watch()),
        "unwatch" => report("unwatched", remote::unwatch()),
        "sessions" => match remote::sessions() {
            Ok(sessions) => say(&format!(
                "sessions: {}",
                sessions
                    .iter()
                    .map(|session| session.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Err(err) => say(&format!("sessions failed: {err}")),
        },
        "find" => match tasks::mine() {
            Ok(found) => say(&format!(
                "found: {}",
                found
                    .iter()
                    .map(|task| format!("{}@{}", task.key, task.source))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Err(err) => say(&format!("find failed: {err}")),
        },
        _ => {}
    }
}

/// The events of this part: true when handled.
pub fn on_event(event: &Event) -> bool {
    match event {
        Event::ClaudeSessionGone(session) => say(&format!("gone {session}")),
        Event::ClaudeAction((session, action)) => say(&format!("action {session} {action}")),
        Event::SystemChanged(change) => say(&format!("system {change:?}")),
        Event::TaskQuery(query) => {
            let found = match &query.query {
                Query::Mine => Ok(vec![
                    tasks::task("P-1", "One").status("Open"),
                    tasks::task("P-2", "Two"),
                ]),
                Query::Search(text) => Err(format!("can't search for {text}")),
                Query::Get(key) => Ok(vec![
                    tasks::task(key, "The one")
                        .markdown("# Text")
                        .action(tasks::action("work", "Take to Work").start()),
                ]),
                Query::Run(run) => {
                    let instance = match service::is_service() {
                        true => "service",
                        false => "window",
                    };
                    match run.action.as_str() {
                        "silent" => say(&format!("run query {}", query.id)),
                        "fail" => tasks::started(query.id, Err("can't run it".into())),
                        _ => tasks::started(
                            query.id,
                            Ok(tasks::opened(&format!("{instance}-{}", run.key))
                                .note(&format!("{} in {}", run.action, run.project))),
                        ),
                    }
                    return true;
                }
            };
            tasks::respond(query.id, found);
        }
        _ => return false,
    }
    true
}

fn report(done: &str, result: Result<(), String>) {
    match result {
        Ok(()) => say(done),
        Err(err) => say(&format!("{done} failed: {err}")),
    }
}

fn say(title: &str) {
    notify::info(title);
}
