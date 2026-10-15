//! The probe's commands for the API 0.3's new calls (part 10.3): `v3:<name>`. What reaches the
//! window the tests see as calls; what comes back the probe reports through notifications.

use std::cell::Cell;

use flux_plugin_api::dialog::{Form, Values};
use flux_plugin_api::host::project;
use flux_plugin_api::{Event, claude, contexts, git, links, notify, picker, settings, system};

thread_local! {
    /// The last picker the probe opened.
    static PICKER: Cell<u64> = const { Cell::new(0) };
}

pub fn run(command: &str) {
    let root = project::root().unwrap_or_default();
    match command {
        "settings-set" => report("set", settings::set("greeting", "hey")),
        "settings-set-number" => report("set", settings::set("greeting", &42)),
        "settings-set-status" => report("set", settings::set("account-status", "x")),
        "settings-reset" => report("reset", settings::reset("greeting")),
        "settings-status" => {
            settings::status("account-status", "Signed in", settings::Tone::Success)
        }
        "settings-status-wrong" => settings::status("greeting", "nope", settings::Tone::Error),
        "form" => {
            let id = Form::new("Open Task", "Start")
                .toggle("branch", "Create a branch", true)
                .text("name", "Name", "TRACK-1")
                .enabled_by("branch")
                .send();
            notify::info(&format!("form: {id}"));
        }
        "picker" => {
            let id = picker::Picker::new("Find")
                .live()
                .at_status_item("clock")
                .item(picker::item("a", "Alpha"))
                .open();
            PICKER.set(id);
        }
        "picker-unknown-anchor" => {
            PICKER.set(picker::Picker::menu().at_status_item("nope").open());
        }
        "picker-update" => picker::update(PICKER.get(), [picker::item("b", "Beta")], true),
        "picker-close" => picker::close(PICKER.get()),
        "branches" => {
            let title = match git::branches(&root) {
                Ok(branches) => format!(
                    "branches: {}",
                    branches
                        .iter()
                        .map(|branch| branch.name.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                Err(err) => format!("branches failed: {err}"),
            };
            notify::info(&title);
        }
        "new-branch" => {
            let id = git::new_branch(&root, "TRACK-1-fix", Some("main"));
            notify::info(&format!("new-branch: {id}"));
        }
        "checkout" => {
            let id = git::checkout(&root, "main");
            notify::info(&format!("checkout: {id}"));
        }
        "commit-template" => git::commit_template("TRACK-1 Fix", Some("TRACK-1")),
        "links" => report(
            "links",
            links::set([links::pattern("task", &links::key_regex(&["TRACK"])).tooltip()]),
        ),
        "describe" => links::tooltip(7, "**TRACK-1**"),
        "claude" => report(
            "claude",
            claude::session()
                .title("TRACK-1")
                .attach("TRACK-1", "# Fix")
                .prompt("Plan it")
                .send()
                .plan_mode()
                .start(),
        ),
        "contexts-switch" => report(
            "switch",
            contexts::switch(Some(("TRACK-0", "TRACK-0 Old")), Some("TRACK-1")),
        ),
        "contexts-remove" => contexts::remove("TRACK-0"),
        "utc-offset" => {
            notify::info(&format!("offset: {}", system::utc_offset(1_791_961_200)));
        }
        "time-zone" => {
            notify::info(&format!("zone: {}", system::time_zone()));
        }
        "open-settings" => system::open_settings(),
        _ => {}
    }
}

fn report(what: &str, result: Result<(), String>) {
    let title = match result {
        Ok(()) => format!("{what}: ok"),
        Err(err) => format!("{what} failed: {err}"),
    };
    notify::info(&title);
}

/// The events of this part: true when handled.
pub fn on_event(event: &Event) -> bool {
    match event {
        Event::FormAnswered((id, values)) => {
            let text = match values {
                Some(values) => {
                    let values = Values::from(values.clone());
                    format!("branch={} name={}", values.on("branch"), values.text("name"))
                }
                None => "cancelled".into(),
            };
            notify::info(&format!("form {id}: {text}"));
        }
        Event::PickerQuery((id, query)) => {
            notify::info(&format!("query {id}: {query}"));
        }
        Event::PickerChosen((id, key)) => {
            notify::info(&format!("chosen {id}: {key:?}"));
        }
        Event::GitDone((id, result)) => {
            notify::info(&format!("git-done {id}: {result:?}"));
        }
        Event::LinkActivated((pattern, text)) => {
            notify::info(&format!("link {pattern}: {text}"));
        }
        Event::LinkHovered((request, _, text)) => links::tooltip(*request, &format!("**{text}**")),
        _ => return false,
    }
    true
}
