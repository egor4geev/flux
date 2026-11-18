//! The API 0.5's sections (FluxGram part 1): the plugin's service — a window pings it, and a
//! counter it keeps is shared by every window — and the system's file dialog. And two tools for
//! Claude (the manifest's `[[tools]]`): `echo` answers at once; `note` asks the user with a card
//! in Claude's chat and writes the line to the plugin's log only when confirmed.

use std::collections::HashMap;
use std::time::Instant;

use flux_plugin_api::host::ui::AvatarSpec;
use flux_plugin_api::picker::{Picker, item};
use flux_plugin_api::tools::{self, ConfirmChoice, Confirmation, ToolCall, ToolOutput};
use flux_plugin_api::view::{Element, SpanStyle, Tone, button, header, row, span};
use flux_plugin_api::{Event, dialog, log, service, tr, trf};
use serde::Deserialize;

use crate::{Playground, Section};

/// What the 0.5 sections remember.
#[derive(Default)]
pub(crate) struct State {
    /// When the last ping went to the service.
    ping: Option<Instant>,
    /// The service's counter (in the service).
    count: u64,
    /// The open file dialog.
    pick: Option<u64>,
    /// The notes waiting for the user's choice on their cards, by Claude's call.
    notes: HashMap<u64, String>,
    /// The Chats section: the inbox's count, the open picker of people.
    unread: u32,
    people: Option<u64>,
}

/// The people of the Chats section's picker: key, name, initials, color, group.
const PEOPLE: [(&str, &str, &str, u8, &str); 5] = [
    ("anna", "Anna Smirnova", "AS", 6, "Contacts"),
    ("dima", "Dima Kim", "DK", 4, "Contacts"),
    ("oleg", "Oleg", "O", 1, "Contacts"),
    ("backend", "Backend · team", "B", 0, "Groups"),
    ("design", "Design review", "D", 5, "Groups"),
];

/// The arguments of both tools (their schemas in `tools/`).
#[derive(Deserialize)]
struct TextArgs {
    text: String,
}

impl Playground {
    /// The 0.5 events: answered here (none), or handed back.
    pub(crate) fn handle_05(&mut self, event: Event) -> Option<Event> {
        match event {
            // In the service: a window's message.
            Event::ServiceMessage((Some(window), text)) => match text.as_str() {
                "ping" => {
                    let _ = service::send_to(window, "pong");
                }
                "count" => {
                    self.v5.count += 1;
                    let _ = service::broadcast(&format!("count {}", self.v5.count));
                }
                _ => {}
            },
            // In a window: the service's answer, or its news for everyone.
            Event::ServiceMessage((None, text)) => {
                if text == "pong" {
                    let micros = self
                        .v5
                        .ping
                        .take()
                        .map_or(0, |sent| sent.elapsed().as_micros());
                    let micros = micros.to_string();
                    self.set(
                        Section::Service,
                        trf("The service answered in {0} µs", &[&micros]),
                    );
                } else if let Some(count) = text.strip_prefix("count ") {
                    self.set(
                        Section::Service,
                        trf("The counter of all windows: {0}", &[&count]),
                    );
                }
            }
            Event::ServiceStarted => {
                self.set(Section::Service, tr("The service started"));
            }
            Event::ToolCalled(call) => self.tool_called(call),
            Event::ToolConfirmed((call, choice)) => self.note_chosen(call, choice),
            // Claude was stopped: the card is gone, the note isn't written.
            Event::ToolCancelled(call) => {
                self.v5.notes.remove(&call);
            }
            Event::PickerChosen((id, key)) if Some(id) == self.v5.people => {
                self.v5.people = None;
                let text = match key {
                    Some(key) => trf("Chose `{0}`.", &[&key]),
                    None => tr("Dismissed."),
                };
                self.set(Section::Chats, text);
            }
            Event::FilesPicked((id, paths)) if Some(id) == self.v5.pick => {
                self.v5.pick = None;
                let text = match paths {
                    Some(paths) => paths.join("\n"),
                    None => tr("Cancelled"),
                };
                self.set(Section::Files, text);
            }
            other => return Some(other),
        }
        None
    }

    /// Claude calls a tool: `echo` answers at once, `note` shows its card first.
    fn tool_called(&mut self, call: ToolCall) {
        let args: TextArgs = match call.parse() {
            Ok(args) => args,
            Err(err) => return tools::respond(call.id, ToolOutput::error(&err)),
        };
        match call.tool.as_str() {
            "echo" => {
                let count = args.text.chars().count().to_string();
                let output = ToolOutput::text(&args.text)
                    .summary(&trf("Echo: «{0}»", &[&args.text]))
                    .detail(&trf("{0} characters", &[&count]))
                    .excerpt(&format!("> {}", args.text));
                tools::respond(call.id, output);
            }
            "note" => {
                let card = Confirmation::new(
                    &tr("Leave a note in the playground"),
                    &args.text,
                    &tr("Write"),
                    &tr("Don't write"),
                )
                .subtitle(&tr("to its log"))
                .quote(&tr("Claude"), &tr("asks to write this line"))
                .edit(&tr("Edit"))
                .hint(&tr("«Edit» only tells Claude you want another text"));
                match tools::confirm(call.id, &card) {
                    Ok(()) => {
                        self.v5.notes.insert(call.id, args.text);
                    }
                    Err(err) => tools::respond(call.id, ToolOutput::error(&err)),
                }
            }
            other => tools::respond(
                call.id,
                ToolOutput::error(&format!("The playground has no tool {other}.")),
            ),
        }
    }

    /// The user chose on the note's card: write it, or tell Claude why not.
    fn note_chosen(&mut self, call: u64, choice: ConfirmChoice) {
        let Some(text) = self.v5.notes.remove(&call) else {
            return;
        };
        let output = match choice {
            ConfirmChoice::Confirmed => {
                log::info(&format!("[note] {text}"));
                ToolOutput::text("The note is written to the API Playground's log.")
                    .summary(&trf("Wrote «{0}»", &[&text]))
            }
            ConfirmChoice::Edit => ToolOutput::text(
                "The user chose to edit the note: nothing is written; ask them for the new text.",
            )
            .summary(&tr("The user chose to edit")),
            ConfirmChoice::Rejected => {
                ToolOutput::text("The user declined: the note isn't written.")
                    .summary(&tr("Declined"))
            }
        };
        tools::respond(call, output);
    }

    pub(crate) fn clicked_05(&mut self, element: &str) {
        match element {
            "service-ping" => {
                self.v5.ping = Some(Instant::now());
                if let Err(err) = service::send("ping") {
                    self.set(Section::Service, err);
                }
            }
            "service-count" => {
                if let Err(err) = service::send("count") {
                    self.set(Section::Service, err);
                }
            }
            "files-pick" => {
                self.v5.pick = Some(dialog::pick_files(Some(&tr("Choose")), true));
            }
            "chats-inbox" => {
                self.v5.unread += 1;
                self.render();
            }
            "chats-work" | "chats-starred" => {
                self.v5.unread = 0;
                self.render();
            }
            "chats-add" => {
                let people = PEOPLE.iter().map(|(key, name, initials, color, group)| {
                    item(key, name)
                        .avatar(AvatarSpec::initials(initials, *color))
                        .group(&tr(group))
                });
                self.v5.people = Some(
                    Picker::new(&tr("Find a person or a group"))
                        .items(people)
                        .under(crate::WINDOW, "chats-add")
                        .open(),
                );
            }
            "chats-header" | "chats-profile" => {
                self.set(Section::Chats, trf("`{0}` clicked.", &[&element]));
            }
            _ => {}
        }
    }

    pub(crate) fn sections_05(&self) -> Vec<(Section, Vec<Element>, Vec<Element>)> {
        let b = |id: &str, label: &str| -> Element { button(id, &tr(label)).into() };
        vec![
            (
                Section::Service,
                vec![
                    b("service-ping", "Ping the Service"),
                    b("service-count", "Count in All Windows"),
                ],
                Vec::new(),
            ),
            (
                Section::Files,
                vec![b("files-pick", "Pick Files…")],
                Vec::new(),
            ),
            (
                Section::Chats,
                vec![
                    button("chats-inbox", &tr("Inbox"))
                        .badge(&self.v5.unread.to_string())
                        .into(),
                    button("chats-work", &tr("Work"))
                        .primary()
                        .badge("12")
                        .into(),
                    button("chats-starred", &tr("Starred"))
                        .icon("star")
                        .primary()
                        .badge("3")
                        .into(),
                    button("chats-add", &tr("Add Chat…"))
                        .icon("plus")
                        .badge("5")
                        .into(),
                ],
                vec![
                    row(
                        "chats-headers",
                        [header("chats-header", &tr("Backend · team"))
                            .avatar(AvatarSpec::initials("B", 0))
                            .subtitle([
                                span(&tr("5 members")).tone(Tone::Dim),
                                span(" · ").tone(Tone::Dim),
                                span(&tr("Petya is typing…")).tone(Tone::Accent),
                            ])
                            .into()],
                    ),
                    header("chats-profile", "Anna Smirnova")
                        .avatar(AvatarSpec::initials("AS", 6))
                        .subtitle([span(&tr("online")).tone(Tone::Accent)])
                        .large()
                        .into(),
                ],
            ),
        ]
    }
}
