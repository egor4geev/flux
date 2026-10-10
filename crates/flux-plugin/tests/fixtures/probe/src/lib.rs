//! The runtime's test plugin: a command per part of the API; it reports through notifications
//! (their titles are what the tests check).

use flux_plugin_api::host::{editors, project, storage};
use flux_plugin_api::{CommandContext, Event, Plugin, notify, register_plugin, setting, tr};

/// Commands `net:…`: requests and the server (part 8.2).
mod net;
/// Commands `proc:…`: programs, timers, secrets, folders, the time limit (part 8.2).
mod processes;

struct Probe;

impl Plugin for Probe {
    fn new() -> Self {
        Probe
    }

    fn activate(&mut self) {
        println!("probe: activated");
        eprintln!("probe: on stderr");
    }

    fn deactivate(&mut self) {
        println!("probe: deactivated");
    }

    fn run_command(&mut self, command: &str, context: &CommandContext) {
        if let Some(command) = command.strip_prefix("net:") {
            return net::run(command);
        }
        if let Some(command) = command.strip_prefix("proc:") {
            return processes::run(command);
        }
        match command {
            "context" => {
                notify::info(&format!(
                    "context: {:?} {:?}",
                    context.source, context.paths
                ));
            }
            "hello" => {
                notify::info("hello");
            }
            "echo" => {
                let text = editors::active()
                    .and_then(|editor| editors::text(editor.id))
                    .unwrap_or_else(|| "<none>".into());
                notify::info(&format!("text: {text}"));
            }
            "panic" => panic!("boom {}", 6 * 7),
            "spin" => {
                let mut n: u64 = 0;
                loop {
                    n = std::hint::black_box(n.wrapping_add(1));
                }
            }
            "oom" => {
                let mut blocks = Vec::new();
                loop {
                    blocks.push(vec![1u8; 64 << 20]);
                    std::hint::black_box(&blocks);
                }
            }
            "search" => {
                let query = project::Query {
                    text: "TODO".into(),
                    case_sensitive: true,
                    whole_word: false,
                    regex: false,
                };
                let title = match project::search(&query, 100) {
                    Ok(result) => {
                        let found: Vec<String> = result
                            .files
                            .iter()
                            .flat_map(|file| {
                                file.lines.iter().map(move |line| {
                                    format!("{}:{}:{:?}", file.path, line.line, line.ranges)
                                })
                            })
                            .collect();
                        format!("found: {}", found.join(" "))
                    }
                    Err(err) => format!("search failed: {err}"),
                };
                notify::info(&title);
            }
            "store" => storage::set("greeting", Some("hi there")),
            "load" => {
                let value = storage::get("greeting").unwrap_or_else(|| "<none>".into());
                notify::info(&format!("stored: {value}"));
            }
            "read-project" => {
                let title = match project::root() {
                    Some(root) => match std::fs::read_to_string(format!("{root}/notes.txt")) {
                        Ok(text) => format!("read: {}", text.trim()),
                        Err(err) => format!("can't read: {err}"),
                    },
                    None => "no project".into(),
                };
                notify::info(&title);
            }
            "write-data" => {
                let dir = storage::data_dir();
                let title = match std::fs::write(format!("{dir}/file.txt"), "data") {
                    Ok(()) => "wrote data".to_string(),
                    Err(err) => format!("can't write data: {err}"),
                };
                notify::info(&title);
            }
            "setting" => {
                let greeting: String = setting("greeting").unwrap_or_default();
                notify::info(&format!("setting: {greeting}"));
            }
            "translate" => {
                notify::info(&tr("Hello"));
            }
            _ => {}
        }
    }

    fn on_event(&mut self, event: Event) {
        if net::on_event(&event) || processes::on_event(&event) {
            return;
        }
        if let Event::SettingsChanged = event {
            notify::info("settings changed");
        }
    }
}

register_plugin!(Probe);
