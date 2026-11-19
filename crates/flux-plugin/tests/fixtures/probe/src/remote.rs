//! The probe's commands for the API 0.6 (the bot, part 1): `remote:<name>` — Claude for remote
//! work and tasks between plugins. As a source of tasks it gives two made-up ones.

use flux_plugin_api::tasks::{self, Query};
use flux_plugin_api::{Event, notify, remote};

pub fn run(command: &str) {
    match command {
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
                    .map(|task| task.key.as_str())
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
                Query::Get(key) => Ok(vec![tasks::task(key, "The one").markdown("# Text")]),
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
