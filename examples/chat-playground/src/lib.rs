//! The Chat Playground: a chat-like mock of the plugin API 0.5 — the manual check of the
//! conversation, pictures, avatars and QR codes, and an example to copy from.
//!
//! The window "chats", on the left island, lists chats as rows of two lines (avatars, unread
//! badges, label icons) with a QR code under them; a click opens the chat as a tab of the window
//! "chat" (`tabs = true`): the tab `chat/<chat>`, a header (avatar, title, subtitle, a switch for
//! new messages every few seconds) over a conversation. Each chat has its own 10 000 messages of
//! every kind, paged by `load-older`; pictures are drawn by the plugin into its data folder at
//! activation, each with a tiny JPEG preview; the field echoes what is sent as an outgoing message
//! that goes sending → sent → read. A chat's tab in sight reads its messages; the unread ones
//! count on the launchpad icon and put a dot on their tabs. A closed tab forgets its chat (the
//! log says `tab-closed`); a tab Flux restored with the project is filled again from its key.

use std::collections::HashMap;
use std::time::Duration;

use flux_plugin_api::conversation::{
    self as chat, Composed, Delivery, History, ImageShape, Message, Part, clear, code, composer,
    draft, file, link_preview, newer, notice, older, placeholder, put, quote, reply_to, reveal,
    rich, sticker, unread_from,
};
use flux_plugin_api::host::ui;
use flux_plugin_api::view::*;
use flux_plugin_api::{Event, Plugin, UiEvent, log, register_plugin, storage, timers};

const CHATS: &str = "chats";
const CHAT: &str = "chat";
const FEED: &str = "feed";
/// The history: message 0 is the oldest, `TOTAL - 1` the newest.
const TOTAL: u64 = 10_000;
/// The first fill, and each page of `load-older`.
const FIRST: u64 = 60;
const PAGE: u64 = 100;
/// The newest message's date, and the step between messages.
const NEWEST: i64 = 1_763_380_000;
const STEP: i64 = 600;

/// The people of the group: name, color, initials.
const PEOPLE: [(&str, u8, &str); 4] = [
    ("Anna Smirnova", 6, "AS"),
    ("Dima Kim", 4, "DK"),
    ("Petya Smirnov", 0, "PS"),
    ("Oleg", 1, "O"),
];

/// A picture the plugin drew: its file, its size, its preview.
#[derive(Clone)]
struct Picture {
    path: String,
    width: u32,
    height: u32,
    preview: Vec<u8>,
}

/// A chat of the list: key, title, last message, time, unread, muted, avatar color, initials.
struct ChatRow {
    key: &'static str,
    title: &'static str,
    last: &'static str,
    time: &'static str,
    unread: u32,
    muted: bool,
    color: u8,
    initials: &'static str,
}

const ROWS: [ChatRow; 6] = [
    ChatRow {
        key: "team",
        title: "Backend · team",
        last: "Petya: Release on Friday?",
        time: "14:32",
        unread: 3,
        muted: false,
        color: 5,
        initials: "B",
    },
    ChatRow {
        key: "anna",
        title: "Anna Smirnova",
        last: "Sent the mockups",
        time: "13:10",
        unread: 1,
        muted: false,
        color: 6,
        initials: "A",
    },
    ChatRow {
        key: "flux",
        title: "Flux · testing",
        last: "Sasha: Updated over the air",
        time: "12:47",
        unread: 0,
        muted: false,
        color: 3,
        initials: "F",
    },
    ChatRow {
        key: "dima",
        title: "Dima Kim",
        last: "You: Call at 11?",
        time: "yesterday",
        unread: 0,
        muted: false,
        color: 4,
        initials: "D",
    },
    ChatRow {
        key: "oleg",
        title: "Oleg",
        last: "Sticker",
        time: "yesterday",
        unread: 0,
        muted: false,
        color: 1,
        initials: "O",
    },
    ChatRow {
        key: "design",
        title: "Design review",
        last: "Anna: Photo",
        time: "Mon",
        unread: 12,
        muted: true,
        color: 2,
        initials: "D",
    },
];

fn chat_row(key: &str) -> Option<(usize, &'static ChatRow)> {
    ROWS.iter().enumerate().find(|(_, row)| row.key == key)
}

/// A chat open in its tab `chat/<key>`: what its conversation holds.
struct OpenChat {
    /// The tab's window name, `chat/<key>`.
    tab: String,
    /// The oldest message loaded.
    oldest: u64,
    /// New messages every few seconds.
    ticker: Option<u64>,
    /// The page timer of `load-older`.
    page_timer: Option<u64>,
    shown_visible: String,
    /// The tab is the active one: what comes is read.
    in_sight: bool,
}

#[derive(Default)]
struct Playground {
    pictures: Vec<Picture>,
    /// The next id of a message sent or added.
    next: u64,
    /// Unread messages by chat.
    unread: HashMap<String, u32>,
    /// The chats open in tabs, by key; a closed tab forgets its chat.
    open: HashMap<String, OpenChat>,
    /// Delivery timers of sent messages: timer → (chat, message, step).
    deliveries: HashMap<u64, (String, Message, Delivery)>,
    /// Download timers of files: timer → (chat, message, progress).
    downloads: HashMap<u64, (String, u64, f32)>,
}

impl Plugin for Playground {
    fn new() -> Self {
        Playground {
            next: TOTAL,
            unread: ROWS
                .iter()
                .map(|row| (row.key.to_string(), row.unread))
                .collect(),
            ..Default::default()
        }
    }

    fn activate(&mut self) {
        self.pictures = draw_pictures();
        self.render_chats();
        self.update_badges();
    }

    fn run_command(&mut self, command: &str, _: &flux_plugin_api::CommandContext) {
        if command == "open" {
            ui::show(CHATS);
        }
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::ToolWindowShown(window) if window == CHATS => self.render_chats(),
            // A chat's tab came into sight: opened, chosen, or restored with the project (the
            // plugin knows nothing of it then and fills it from its key).
            Event::ToolWindowShown(window) => {
                if let Some(key) = chat_key(&window) {
                    self.ensure_open(key);
                    self.set_in_sight(key, true);
                }
            }
            Event::ToolWindowHidden(window) => {
                if let Some(key) = chat_key(&window) {
                    self.set_in_sight(key, false);
                }
            }
            Event::TabClosed(window) => {
                log::info(&format!("tab-closed: {window}"));
                if let Some(key) = chat_key(&window) {
                    self.forget(key);
                }
            }
            Event::Ui(input) if input.window == CHATS => match input.event {
                UiEvent::Activated(key) => self.open_chat(&key),
                // A scenario's `ui:<plugin>/chats/show`, `…/chats/<chat>` (open it),
                // `…/chats/incoming/<chat>` (a message comes) and `…/chats/close/<chat>`.
                UiEvent::Clicked if input.element == "show" => ui::show(CHATS),
                UiEvent::Clicked => match input.element.split_once('/') {
                    Some(("incoming", key)) => self.incoming(key),
                    Some(("close", key)) => ui::hide(&view_tab(key)),
                    _ => self.open_chat(&input.element),
                },
                _ => {}
            },
            Event::Ui(input) => {
                if let Some(key) = chat_key(&input.window) {
                    let key = key.to_string();
                    self.chat_event(&key, &input.element, input.event)
                }
            }
            Event::Timer(timer) => self.timer(timer),
            _ => {}
        }
    }
}

/// The chat of a tab's window name, `chat/<key>`.
fn chat_key(window: &str) -> Option<&str> {
    window
        .strip_prefix(CHAT)?
        .strip_prefix('/')
        .filter(|key| chat_row(key).is_some())
}

/// The tab's window name of a chat.
fn view_tab(key: &str) -> String {
    format!("{CHAT}/{key}")
}

impl Playground {
    fn render_chats(&self) {
        let mut tree = Tree::new().two_line();
        for row in &ROWS {
            let mut spec = RowSpec::new(row.key, row.title)
                .detail(row.time)
                .subtitle(vec![span(row.last)])
                .avatar(AvatarSpec::initials(row.initials, row.color));
            let unread = self.unread.get(row.key).copied().unwrap_or(0);
            if unread > 0 {
                spec = spec.badge(&unread.to_string()).badge_tone(if row.muted {
                    Tone::Muted
                } else {
                    Tone::Accent
                });
            }
            if row.key == "team" || row.key == "flux" {
                spec = spec.label_icon("claude");
            }
            if row.key == "anna"
                && let Some(picture) = self.pictures.first()
            {
                spec = spec.avatar(AvatarSpec::initials("A", 6).picture(&picture.path));
            }
            tree.add(spec);
        }
        set_view(
            CHATS,
            column(
                "root",
                [
                    tree.into_element("list"),
                    label("qr-hint", "Sign in on a phone:"),
                    qr("qr", "https://example.com/login?token=chat-playground"),
                ],
            ),
        );
    }

    /// The launchpad icon counts the unread messages of the chats that aren't muted; a chat's
    /// tab with unread messages has a dot.
    fn update_badges(&self) {
        let total: u32 = ROWS
            .iter()
            .filter(|row| !row.muted)
            .map(|row| self.unread.get(row.key).copied().unwrap_or(0))
            .sum();
        let total = total.to_string();
        set_badge(CHATS, (total != "0").then_some(total.as_str()));
        for (key, chat) in &self.open {
            let unread = self.unread.get(key).copied().unwrap_or(0);
            let count = unread.to_string();
            set_badge(&chat.tab, (unread > 0).then_some(count.as_str()));
        }
    }

    /// A row of the list: the chat's tab opens (or comes forward) with the keyboard.
    fn open_chat(&mut self, key: &str) {
        let Some((_, row)) = chat_row(key) else {
            return;
        };
        open_tab(CHAT, key, row.title, true);
        self.ensure_open(key);
    }

    /// The chat's state and its tab's content, made when missing: a tab opened now, or one
    /// Flux restored or kept through a restart.
    fn ensure_open(&mut self, key: &str) {
        if self.open.contains_key(key) {
            return;
        }
        let tab = view_tab(key);
        self.open.insert(
            key.to_string(),
            OpenChat {
                tab: tab.clone(),
                oldest: TOTAL - FIRST,
                ticker: None,
                page_timer: None,
                shown_visible: String::new(),
                in_sight: false,
            },
        );
        self.render_chat(key);
        let seed = seed(key);
        let first: Vec<Message> = (TOTAL - FIRST..TOTAL)
            .map(|i| self.message(seed, i))
            .collect();
        let unread = self.unread.get(key).copied().unwrap_or(0) as u64;
        let mut changes = vec![
            clear(),
            put(first),
            older(History::More),
            newer(History::Complete),
        ];
        if unread > 0 {
            changes.push(unread_from(Some(&id(TOTAL - unread))));
        }
        chat::update(&tab, FEED, changes);
        self.update_badges();
    }

    /// The chat's tab came into sight (its messages are read) or went out of it.
    fn set_in_sight(&mut self, key: &str, in_sight: bool) {
        if let Some(chat) = self.open.get_mut(key) {
            chat.in_sight = in_sight;
        }
        if in_sight && self.unread.insert(key.to_string(), 0).unwrap_or(0) > 0 {
            self.render_chats();
            self.update_badges();
        }
    }

    /// The tab closed: the chat's state goes, and its timers with it.
    fn forget(&mut self, key: &str) {
        if let Some(chat) = self.open.remove(key) {
            for timer in chat.ticker.into_iter().chain(chat.page_timer) {
                timers::cancel(timer);
            }
        }
        self.deliveries.retain(|_, (chat, _, _)| chat != key);
        self.downloads.retain(|_, (chat, _, _)| chat != key);
    }

    /// A message comes into a chat: into its tab when open; unread unless the tab is in sight.
    fn incoming(&mut self, key: &str) {
        let index = self.next;
        self.next += 1;
        let author = PEOPLE[(index % 4) as usize];
        if let Some(chat) = self.open.get(key) {
            let message = Message::new(&id(index), index, now())
                .author(
                    author.0,
                    author.1,
                    Some(AvatarSpec::initials(author.2, author.1)),
                )
                .text(&format!(
                    "A new message #{index}: the feed stays at the bottom if it was there."
                ));
            chat::update(&chat.tab, FEED, vec![put(vec![message])]);
            if chat.in_sight {
                return;
            }
        }
        *self.unread.entry(key.to_string()).or_default() += 1;
        self.render_chats();
        self.update_badges();
    }

    fn render_chat(&self, key: &str) {
        let (Some(chat), Some((_, info))) = (self.open.get(key), chat_row(key)) else {
            return;
        };
        let header = row(
            "header",
            [
                avatar("avatar", AvatarSpec::initials(info.initials, info.color)),
                column(
                    "titles",
                    [
                        text("title", [span(info.title).bold()]),
                        text(
                            "subtitle",
                            [span(&format!("5 members · {}", chat.shown_visible))
                                .tone(Tone::Muted)],
                        ),
                    ],
                ),
                switch("ticker", "New messages", chat.ticker.is_some()),
                button("reveal", "Reveal Oldest").into(),
            ],
        );
        let feed = conversation(
            FEED,
            chat::spec()
                .composer(composer("Message…").attachments().action(
                    "claude",
                    "Reply with Claude",
                    Some("claude"),
                ))
                .empty_text("No messages yet")
                .message_action("reply", "Reply", Some("arrow-left"))
                .message_action("claude", "Reply with Claude", Some("claude")),
        );
        set_view(&chat.tab, column("root", [header, feed]));
    }

    fn chat_event(&mut self, key: &str, element: &str, event: UiEvent) {
        let Some(tab) = self.open.get(key).map(|chat| chat.tab.clone()) else {
            return;
        };
        let seed = seed(key);
        match (element, event) {
            (FEED, UiEvent::LoadOlder) => {
                chat::update(&tab, FEED, vec![older(History::Loading)]);
                let timer = timers::after(Duration::from_millis(250));
                if let Some(chat) = self.open.get_mut(key) {
                    chat.page_timer = Some(timer);
                }
            }
            (FEED, UiEvent::Visible((first, last))) => {
                if let Some(chat) = self.open.get_mut(key) {
                    chat.shown_visible = format!("{first} … {last}");
                }
                self.render_chat(key);
            }
            (FEED, UiEvent::MessageAction((message, action))) => {
                let Some(index) = index(&message) else {
                    return;
                };
                let quote = self.message(seed, index).quote("You");
                match action.as_str() {
                    "reply" => chat::update(&tab, FEED, vec![reply_to(Some(quote))]),
                    _ => chat::update(
                        &tab,
                        FEED,
                        vec![
                            reply_to(Some(quote)),
                            draft("Claude's draft: sounds good, let's ship it on Friday."),
                        ],
                    ),
                }
            }
            (FEED, UiEvent::ComposerAction((_, composed))) => {
                let text = format!("{} — rewritten by Claude", composed.text.trim());
                chat::update(&tab, FEED, vec![draft(&text)]);
            }
            (FEED, UiEvent::Composed(composed)) => self.send(key, composed),
            (FEED, UiEvent::PartClicked((message, _))) => {
                if let Some(index) = index(&message) {
                    let timer = timers::after(Duration::from_millis(300));
                    self.downloads.insert(timer, (key.to_string(), index, 0.3));
                    self.put_download(key, index, 0.0);
                }
            }
            ("ticker", UiEvent::Toggled(on)) => {
                if let Some(chat) = self.open.get_mut(key) {
                    if let Some(timer) = chat.ticker.take() {
                        timers::cancel(timer);
                    }
                    if on {
                        chat.ticker = Some(timers::every(Duration::from_secs(3)));
                    }
                }
                self.render_chat(key);
            }
            ("reveal", UiEvent::Clicked) => {
                let oldest = self.open.get(key).map_or(0, |chat| chat.oldest);
                chat::update(&tab, FEED, vec![reveal(&id(oldest))])
            }
            _ => {}
        }
    }

    fn timer(&mut self, timer: u64) {
        let paged = self
            .open
            .iter()
            .find(|(_, chat)| chat.page_timer == Some(timer))
            .map(|(key, _)| key.clone());
        if let Some(key) = paged {
            let seed = seed(&key);
            let Some(chat) = self.open.get_mut(&key) else {
                return;
            };
            chat.page_timer = None;
            let start = chat.oldest.saturating_sub(PAGE);
            let end = chat.oldest;
            chat.oldest = start;
            let tab = chat.tab.clone();
            let page: Vec<Message> = (start..end).map(|i| self.message(seed, i)).collect();
            let history = if start == 0 {
                History::Complete
            } else {
                History::More
            };
            chat::update(&tab, FEED, vec![put(page), older(history)]);
            return;
        }
        let ticked = self
            .open
            .iter()
            .find(|(_, chat)| chat.ticker == Some(timer))
            .map(|(key, _)| key.clone());
        if let Some(key) = ticked {
            self.incoming(&key);
        } else if let Some((key, message, delivery)) = self.deliveries.remove(&timer) {
            let next = match delivery {
                Delivery::Sending => Some(Delivery::Sent),
                Delivery::Sent => Some(Delivery::Read),
                _ => None,
            };
            let updated = Message {
                delivery: Some(delivery),
                ..message.clone()
            };
            chat::update(&view_tab(&key), FEED, vec![put(vec![updated.clone()])]);
            if let Some(next) = next {
                let timer = timers::after(Duration::from_millis(900));
                self.deliveries.insert(timer, (key, updated, next));
            }
        } else if let Some((key, index, done)) = self.downloads.remove(&timer) {
            self.put_download(&key, index, done);
            if done < 1.0 {
                let timer = timers::after(Duration::from_millis(300));
                self.downloads
                    .insert(timer, (key, index, (done + 0.35).min(1.0)));
            }
        }
    }

    /// A file part being downloaded (`done` < 1) or downloaded.
    fn put_download(&self, key: &str, index: u64, done: f32) {
        let mut message = self.message(seed(key), index);
        for part in &mut message.parts {
            if let Part::File(file) = part {
                if done >= 1.0 {
                    file.progress = None;
                    file.path = Some(format!("{}/notes.txt", storage::dir()));
                } else {
                    file.progress = Some(done);
                }
            }
            if let Part::Image(spec) = part {
                if done >= 1.0 {
                    spec.progress = None;
                    spec.path = self.pictures.first().map(|picture| picture.path.clone());
                } else {
                    spec.progress = Some(done);
                }
            }
        }
        chat::update(&view_tab(key), FEED, vec![put(vec![message])]);
    }

    /// The field's message comes back as an outgoing one: sending → sent → read.
    fn send(&mut self, key: &str, composed: Composed) {
        let index = self.next;
        self.next += 1;
        let mut message = Message::new(&id(index), index, now()).outgoing(Delivery::Sending);
        if let Some(reply) = &composed.reply_to
            && let Some(original) = flux_index(reply)
        {
            message = message.reply(self.message(seed(key), original).quote("You"));
        }
        for path in &composed.files {
            let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
            let size = std::fs::metadata(path).map_or(0, |meta| meta.len());
            message = message.part(file(name, size, Some(path)));
        }
        if !composed.text.trim().is_empty() {
            message = message.text(composed.text.trim());
        }
        chat::update(
            &view_tab(key),
            FEED,
            vec![put(vec![message.clone()]), reply_to(None)],
        );
        let timer = timers::after(Duration::from_millis(900));
        self.deliveries
            .insert(timer, (key.to_string(), message, Delivery::Sent));
    }

    /// The `index`th message of a chat's history (`seed` — the chat's): every kind of part in
    /// turn, each chat starting at another.
    fn message(&self, seed: u64, index: u64) -> Message {
        let date = NEWEST - (TOTAL as i64 - 1 - index as i64) * STEP;
        let person = PEOPLE[((index / 3) % 4) as usize];
        let outgoing = index.is_multiple_of(5);
        let mut message = Message::new(&id(index), index, date);
        message = if outgoing {
            message.outgoing(if index + 10 > TOTAL {
                Delivery::Sent
            } else {
                Delivery::Read
            })
        } else {
            message.author(
                person.0,
                person.1,
                Some(AvatarSpec::initials(person.2, person.1)),
            )
        };
        let picture = |n: u64| {
            let picture = &self.pictures[(n as usize) % self.pictures.len().max(1)];
            chat::picture(&picture.path, picture.width, picture.height)
                .preview(picture.preview.clone())
        };
        let kind = (index + seed * 7) % 20;
        message = match kind {
            _ if index.is_multiple_of(97) => {
                return Message::new(&id(index), index, date)
                    .part(notice(&format!("{} joined the group", person.0)));
            }
            1 => message.part(chat::text(vec![
                rich("Styles: "),
                rich("bold").bold(),
                rich(", "),
                rich("italic").italic(),
                rich(", "),
                rich("underline").underline(),
                rich(", "),
                rich("struck").strikethrough(),
                rich(", "),
                rich("code").code(),
                rich(", a "),
                rich("spoiler").spoiler(),
                rich(", a "),
                rich("link").link("https://example.com"),
                rich(" and a "),
                rich("@mention").link("mention:dima"),
                rich("."),
            ])),
            2 => message.text("Here is the fix:").part(code(
                "fn main() {\n    let answer = 42;\n    println!(\"{answer}\");\n}",
                Some("rust"),
            )),
            3 if !self.pictures.is_empty() => message
                .part(chat::image(picture(index)))
                .text("The new map screen — good?")
                .reaction("🔥", 2, true)
                .reaction("👍", 1, false),
            4 => message
                .reply(self.message(seed, index - 3).quote("You"))
                .text("Ok, I'll look at the PR tonight. The env example for staging:")
                .part(file("staging.env.example", 1229, None)),
            5 if !self.pictures.is_empty() => message.part(sticker(picture(index + 1).shape(ImageShape::Square))),
            6 => message.part(placeholder(Some("bell"), "Voice message · 0:42")),
            7 if !self.pictures.is_empty() => message
                .part(chat::image(picture(index)))
                .part(chat::image(picture(index + 1)))
                .part(chat::image(picture(index + 2)))
                .text("An album"),
            8 => message.part(link_preview(
                "https://example.com/blog/release",
                Some("Example Blog"),
                Some("Release notes"),
                Some("What is new in this release: faster scrolling, pictures, chats."),
                self.pictures.get(1).map(|p| chat::picture(&p.path, p.width, p.height)),
            )),
            9 => message
                .forwarded("Anna Smirnova")
                .part(quote(vec![rich("Measure first, then optimize.")]))
                .text("Worth repeating."),
            10 if !self.pictures.is_empty() => message
                .part(chat::image(chat::pending_picture(1200, 800).preview(self.pictures[0].preview.clone())))
                .text("Not downloaded yet: a click downloads it."),
            11 => message.edited().text("Edited: release on Friday? Or Thursday?"),
            _ => message.text(&format!(
                "Message {index}: {}",
                [
                    "Config — env is read before the file.",
                    "Sounds good.",
                    "Can we measure the scroll at 120 Hz?",
                    "I'll check it tomorrow morning, there is a lot to read here and the text wraps over several lines to see the bubble grow.",
                ][(index % 4) as usize]
            )),
        };
        message
    }
}

/// A chat's place in the list: its history starts at another kind of message.
fn seed(key: &str) -> u64 {
    chat_row(key).map_or(0, |(position, _)| position as u64)
}

fn id(index: u64) -> String {
    format!("m{index}")
}

fn index(id: &str) -> Option<u64> {
    id.strip_prefix('m')?.parse().ok()
}

fn flux_index(id: &str) -> Option<u64> {
    index(id)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(NEWEST, |since| since.as_secs() as i64)
}

/// Draws a few pictures into the data folder (gradients with shapes), each with the tiny JPEG
/// preview a message carries, and a text file for the file part.
fn draw_pictures() -> Vec<Picture> {
    let dir = storage::dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(
        format!("{dir}/notes.txt"),
        "Notes of the Chat Playground.\n",
    );
    let sizes = [
        (1280u32, 720u32),
        (800, 800),
        (720, 1080),
        (512, 512),
        (1600, 900),
    ];
    let colors = [
        ([30u8, 42, 68], [133u8, 144, 255]),
        ([60, 20, 40], [255, 126, 182]),
        ([10, 50, 45], [79, 201, 192]),
        ([50, 40, 10], [255, 197, 96]),
        ([25, 25, 60], [180, 140, 255]),
    ];
    sizes
        .iter()
        .zip(colors)
        .enumerate()
        .filter_map(|(n, (&(width, height), (from, to)))| {
            let picture = image::RgbImage::from_fn(width, height, |x, y| {
                let t = (x as f32 / width as f32 + y as f32 / height as f32) / 2.;
                let (cx, cy) = (width as f32 * 0.6, height as f32 * 0.45);
                let r = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
                let ring = (r % 120.) < 6.;
                let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
                if ring {
                    image::Rgb([240, 240, 250])
                } else {
                    image::Rgb([
                        mix(from[0], to[0]),
                        mix(from[1], to[1]),
                        mix(from[2], to[2]),
                    ])
                }
            });
            let path = format!("{dir}/picture-{n}.png");
            picture.save(&path).ok()?;
            let small = image::imageops::thumbnail(&picture, 32, 32 * height / width);
            let mut preview = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut preview, 40)
                .encode_image(&small)
                .ok()?;
            Some(Picture {
                path,
                width,
                height,
                preview,
            })
        })
        .collect()
}

register_plugin!(Playground);
