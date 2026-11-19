//! The probe's commands for the API 0.5's runtime (FluxGram part 1): `svc:<name>` — the service and
//! its messages, the plugin's helpers, Claude's tools. The argument of the command's context is
//! the payload.

use std::cell::Cell;

use flux_plugin_api::host::editors;
use flux_plugin_api::tools::{Confirmation, ToolOutput};
use flux_plugin_api::{CommandContext, Event, notify, process, service, tools};

thread_local! {
    /// The service's counter, shared by the windows (`count`).
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

pub fn run(command: &str, context: &CommandContext) {
    let argument = context.argument.clone().unwrap_or_default();
    match command {
        "send" => report("send", service::send(&argument)),
        "is-service" => say(&format!("is-service: {}", service::is_service())),
        "helper" => match process::helper(&argument) {
            Ok(path) => say(&format!("helper: {path}")),
            Err(err) => say(&format!("helper failed: {err}")),
        },
        "run-helper" => {
            let result =
                process::helper(&argument).and_then(|path| process::command(&path).arg("hi").run());
            match result {
                Ok(output) => say(&format!(
                    "ran: {}",
                    String::from_utf8_lossy(&output.stdout).trim()
                )),
                Err(err) => say(&format!("run failed: {err}")),
            }
        }
        "respond" => tools::respond(
            argument.parse().unwrap_or_default(),
            ToolOutput::text("done").summary("Done"),
        ),
        "confirm" => report(
            "confirm",
            tools::confirm(
                argument.parse().unwrap_or_default(),
                &Confirmation::new("Reply", "hi", "Send", "Don't send"),
            ),
        ),
        _ => {}
    }
}

/// The events of this part: true when handled.
pub fn on_event(event: &Event) -> bool {
    match event {
        // In the service: what a window asks.
        Event::ServiceMessage((Some(window), text)) => {
            let window = *window;
            if let Some(rest) = text.strip_prefix("ping ") {
                report("pong", service::send_to(window, &format!("pong {rest}")));
            } else if let Some(rest) = text.strip_prefix("all ") {
                report("all", service::broadcast(rest));
            } else if text == "count" {
                let count = COUNT.get() + 1;
                COUNT.set(count);
                report("count", service::broadcast(&format!("count {count}")));
            } else if text == "windows" {
                say(&format!("windows: {:?}", service::windows()));
            } else if text == "editor" {
                // A window-only call in the service: no documents, no stop.
                let none = editors::active().is_none();
                let failed = editors::open("/a.rs", None).is_err();
                let id = flux_plugin_api::dialog::Ask::info("Sure?").send();
                say(&format!("editor: none={none} failed={failed} ask={id}"));
            } else if let Some(rest) = text.strip_prefix("to ") {
                // "to <window> <text>"
                let (target, message) = rest.split_once(' ').unwrap_or((rest, ""));
                report(
                    "to",
                    service::send_to(target.parse().unwrap_or_default(), message),
                );
            } else {
                say(&format!("service got {window}: {text}"));
            }
        }
        // In a window: the service's answer.
        Event::ServiceMessage((None, text)) => say(&format!("from service: {text}")),
        Event::ServiceStarted => say("service-started"),
        Event::WindowOpened(window) => say(&format!("window-opened {window}")),
        Event::WindowClosed(window) => say(&format!("window-closed {window}")),
        Event::DialogAnswered((id, answer)) => {
            say(&format!("answered {id}: {answer:?}"));
        }
        Event::ToolCalled(call) => say(&format!("tool-called {} {}", call.id, call.tool)),
        Event::ToolCancelled(call) => say(&format!("tool-cancelled {call}")),
        _ => return false,
    }
    true
}

fn report(what: &str, result: Result<(), String>) {
    match result {
        Ok(()) => {}
        Err(err) => say(&format!("{what} failed: {err}")),
    }
}

fn say(title: &str) {
    notify::info(title);
}
