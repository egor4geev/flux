//! The Chat Playground: a chat-like mock of the plugin API 0.5 — the manual check of the
//! conversation, pictures, avatars and QR codes, and an example to copy from.
//!
//! The window "chats", on the left island, lists chats as rows of two lines (avatars, unread
//! badges, label icons) with a QR code under them; a click opens the chat as a tab of the window
//! "chat" (`tabs = true`): the tab `chat/<chat>`, a header (avatar, title, subtitle, a switch for
//! new messages every few seconds) over a conversation. Each chat has its own 10 000 messages of
//! every kind, paged by `load-older`; pictures are drawn by the plugin into its data folder at
//! activation, each with a tiny JPEG preview; the field echoes what is sent as an outgoing message
//! that goes sending → sent → read (a picture file as a photo). Edit on the user's own message (or
//! ↑ in the empty field for the last one) puts it in the field; saving replaces its text and marks
//! it edited. A chat's tab in sight reads its messages; the unread ones count on the launchpad
//! icon and put a dot on their tabs. A closed tab forgets its chat (the log says `tab-closed`); a
//! tab Flux restored with the project is filled again from its key.
//!
//! Videos (API 0.5): a video's picture with its length and ▶, a GIF, a round video message — a
//! click on one comes as `part-clicked` (the log says `play`). Edit is offered on the user's own
//! messages only (`Message::actions`).
//!
//! FluxGram part 4 (API 0.5): the newest messages of "Dima Kim" are media — voice messages (one
//! to download first), a round video message and a GIF that play by themselves, a video, an
//! animated sticker — from `media/` of the plugin's folder (`voice.ogg`, `round.mp4` and
//! `round.jpg`, `gif.mp4` and `gif.jpg`, `video.mp4` and `video.jpg`, `sticker.fxa`: copied there
//! by hand or by a scenario; a plugin may give Flux only files in its folders). Their first plays
//! come as `media-played` (the log says so).
//!
//! FluxGram part 3 (API 0.5): the field has formatting (its menu, ⌘B ⌘I ⇧⌘X ⇧⌘P ⇧⌘M ⌘K) and the
//! echo reads the Markdown crudely; Reply over a selected part of a message quotes that part
//! (`message-action-quoted`, the log says `quoted`); a click on a picture without its full size
//! opens the viewer at once and the full size comes a moment later; "Backend · team" is marked
//! with "@" while it has unread messages.
//!
//! FluxGram part 5 (API 0.5): the field shows its Markdown as it is typed; "@" suggests the
//! chat's members (one without a username goes in as a link to the person) and "/" a bot's
//! commands (`suggest` → `suggestions`); a right click on Send offers Send Without Sound (the log
//! says `composed-with: silent`). Every eleventh incoming message has its views at the foot and
//! its topic's label over the text, and buttons under it (a press: the log says `keyboard`).
//! A message's menu has Select: the bar's Forward puts the selected messages' bar over the field
//! (`forwarding`; ↵ — the log says `forwarded`), its Delete removes them from the feed. The
//! conversation owns its links (`own-links`): every click comes as `link-clicked` (the log says
//! `link`), a web one then opens in the browser; a text link shows its address on hover.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use flux_plugin_api::conversation::{
    self as chat, Composed, Delivery, History, ImageShape, Message, MessageQuote, Part, Suggestion,
    clear, code, composer, draft, editing, file, forwarding, link_preview, newer, notice, older,
    placeholder, poll_option, poll_spec, put, quote, remove, reply_to, reveal, rich, sticker,
    suggestions, unread_from,
};
use flux_plugin_api::host::ui;
use flux_plugin_api::view::*;
use flux_plugin_api::{Event, Plugin, UiEvent, log, register_plugin, storage, timers};

/// The field's suggestions for `text` after `trigger` (part 5): the members whose name starts
/// with it for "@", the commands for "/".
fn suggested(trigger: &str, text: &str) -> Vec<Suggestion> {
    let query = text.to_lowercase();
    let members = [
        ("anna", "Anna Smirnova", Some("anna"), 6),
        ("dima", "Dima Kim", Some("dimakim"), 4),
        ("olga", "Ольга Петрова", None, 2),
    ];
    let commands = [
        ("start", "Start the bot"),
        ("week", "The week ahead"),
        ("help", "What the bot can do"),
    ];
    match trigger {
        "@" => members
            .iter()
            .filter(|(_, name, username, _)| {
                name.to_lowercase().starts_with(&query)
                    || username.is_some_and(|username| username.starts_with(&query))
            })
            .enumerate()
            .map(|(index, (key, name, username, color))| {
                let insert = match username {
                    Some(username) => format!("@{username} "),
                    None => format!("[{name}](tg://user?id={}) ", 100 + index),
                };
                let initial: String = name.chars().take(1).collect();
                let item = Suggestion::new(key, name, &insert)
                    .avatar(AvatarSpec::initials(&initial, *color));
                match username {
                    Some(username) => item.detail(&format!("@{username}")),
                    None => item,
                }
            })
            .collect(),
        "/" => commands
            .iter()
            .filter(|(name, _)| name.starts_with(&query))
            .map(|(name, about)| {
                Suggestion::new(name, &format!("/{name}"), &format!("/{name} ")).detail(about)
            })
            .collect(),
        _ => Vec::new(),
    }
}

const CHATS: &str = "chats";
const CHAT: &str = "chat";
/// A chat's profile: a dialog with the chat's media.
const PROFILE: &str = "profile";
/// The profile's gallery holds this many pictures, and pages of this many more.
const MEDIA: usize = 60;
const FEED: &str = "feed";
/// The history: message 0 is the oldest, `TOTAL - 1` the newest.
const TOTAL: u64 = 10_000;
/// The first fill, and each page of `load-older`.
const FIRST: u64 = 60;
const PAGE: u64 = 100;
/// The newest message's date, and the step between messages.
const NEWEST: i64 = 1_763_380_000;
/// A download's step (ms): files and pictures come in three.
const DOWNLOAD_STEP: u64 = 700;
/// The chat whose newest messages are media (part 4), and how many of them.
const MEDIA_CHAT: &str = "dima";
const MEDIA_MESSAGES: u64 = 7;
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
    /// The messages the user sent, by (chat's seed, index).
    sent: HashMap<(u64, u64), Message>,
    /// The new texts of edited messages, by (chat's seed, index).
    edits: HashMap<(u64, u64), String>,
    /// The messages whose files and pictures were downloaded, by (chat's seed, index).
    downloaded: HashSet<(u64, u64)>,
    /// The part of a message the quote over a chat's field quotes (a reply to a selection).
    quoted: HashMap<String, String>,
    /// The messages to forward over a chat's field (part 5), by the chat.
    forwarding: HashMap<String, Vec<String>>,
    /// The chat whose profile is open, and how many pictures its gallery holds.
    profile: Option<(String, usize)>,
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
            Event::ToolWindowHidden(window) if window == PROFILE => self.profile = None,
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
            Event::Ui(input) if input.window == PROFILE => self.profile_event(input.event),
            Event::Ui(input) if input.window == CHATS => match input.event {
                UiEvent::Activated(key) => self.open_chat(&key),
                // A row's ⋯: the chat's menu under its row (API 0.5: "<tree>/<row key>").
                UiEvent::RowAction((key, action)) if action == "menu" => {
                    flux_plugin_api::picker::Picker::menu()
                        .item(flux_plugin_api::picker::item("read", "Mark as Read").icon("check"))
                        .item(flux_plugin_api::picker::item("pin", "Pin").icon("pin"))
                        .item(flux_plugin_api::picker::item("hide", "Hide the Chat").icon("close"))
                        .under(CHATS, &format!("list/{key}"))
                        .open();
                }
                UiEvent::RowAction((key, action)) => log::info(&format!("{action}: {key}")),
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

impl Playground {
    /// A media message of [`MEDIA_CHAT`] (part 4): the files are in `media/` of the plugin's
    /// folder; the second voice message and the video come only after a click downloads them.
    fn media_message(&self, message: Message, index: u64) -> Message {
        let dir = format!("{}/media", storage::dir());
        let file = |name: &str| format!("{dir}/{name}");
        let downloaded = self.downloaded.contains(&(seed(MEDIA_CHAT), index));
        // Speech-like loudness: syllables of a few samples, pauses between words.
        let waveform = |shift: u32| -> Vec<u8> {
            (0..64u32)
                .map(|i| {
                    let word = (i + shift) % 11;
                    match word {
                        0 | 1 => 2,
                        _ => (4 + ((i * 37 + shift * 13) % 23) * (11 - word) / 9).min(31) as u8,
                    }
                })
                .collect()
        };
        let poster =
            |name: &str, width: u32, height: u32| chat::picture(&file(name), width, height);
        match TOTAL - 1 - index {
            6 => message.part(chat::voice(
                chat::voice_spec(Some(&file("voice.ogg")), 5, waveform(0)).unheard(),
            )),
            5 => {
                let path = downloaded.then(|| file("voice.ogg"));
                message.part(chat::voice(chat::voice_spec(
                    path.as_deref(),
                    5,
                    waveform(5),
                )))
            }
            4 => message.part(chat::video(
                chat::video_spec(
                    chat::VideoKind::Round,
                    poster("round.jpg", 384, 384),
                    Some(&file("round.mp4")),
                    6,
                )
                .autoplay()
                .unheard(),
            )),
            3 => message.part(chat::video(
                chat::video_spec(
                    chat::VideoKind::Gif,
                    poster("gif.jpg", 320, 240),
                    Some(&file("gif.mp4")),
                    3,
                )
                .autoplay(),
            )),
            2 => {
                let path = downloaded.then(|| file("video.mp4"));
                message
                    .part(chat::video(chat::video_spec(
                        chat::VideoKind::Video,
                        poster("video.jpg", 640, 360),
                        path.as_deref(),
                        10,
                    )))
                    .text("The demo of the new map screen")
            }
            1 => message.part(sticker(
                poster("sticker.png", 320, 320).animation(&file("sticker.fxa"), true),
            )),
            _ => message.part(sticker(
                poster("sticker.png", 320, 320).animation(&file("sticker.fxa"), false),
            )),
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
            // A mention of the user among the unread messages (API 0.5): "@" by the count.
            if row.key == "team" && unread > 0 {
                spec = spec.mention();
            }
            // A chat's buttons over the row's end while it is hovered or selected (they take no
            // room from the title); ⋯ opens the chat's menu under its row.
            spec = spec
                .action("mute", None, Some("bell"), false)
                .action_tooltip("mute", "Mute")
                .action("hide", None, Some("close"), false)
                .action_tooltip("hide", "Hide")
                .action("menu", None, Some("more"), false)
                .action_tooltip("menu", "More");
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
        // The header opens the chat's profile (API 0.5: a `header` is a button).
        let header = row(
            "header",
            [
                header("titles", info.title)
                    .avatar(AvatarSpec::initials(info.initials, info.color))
                    .subtitle([
                        span(&format!("5 members · {}", chat.shown_visible)).tone(Tone::Muted)
                    ])
                    .into(),
                switch("ticker", "New messages", chat.ticker.is_some()),
                button("reveal", "Reveal Oldest").into(),
                icon_button("more", "more", "More"),
            ],
        );
        let feed = conversation(
            FEED,
            chat::spec()
                .composer(
                    composer("Message…")
                        .attachments()
                        .formatting()
                        .action("claude", "Reply with Claude", Some("claude"))
                        .triggers(["@", "/"])
                        .send_option("silent", "Send Without Sound"),
                )
                .empty_text("No messages yet")
                .message_action("reply", "Reply", Some("arrow-left"))
                .message_action("edit", "Edit", Some("pencil"))
                .message_action("claude", "Reply with Claude", Some("claude"))
                .selection_action("forward", "Forward", true)
                .selection_action("delete", "Delete", false)
                .own_links()
                // The reactions the user may put (part 5): ☺ on the hover bar, the chips toggle.
                .reactions(["👍", "❤", "🔥", "🎉", "😁", "🤔", "😢", "👏", "🙏", "👌"]),
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
                self.message_action(key, &message, &action, None)
            }
            // An action over a selected part of the message (API 0.5): the quote quotes the part.
            (FEED, UiEvent::MessageActionQuoted((message, action, text, offset))) => {
                log::info(&format!("quoted: {message} {action} @{offset}: {text}"));
                self.message_action(key, &message, &action, Some(&text))
            }
            // A scenario's `ui:<plugin>/chat/<chat>/reply/<message>` (and `…/edit/…`, `…/claude/…`):
            // the message's action, as its menu would send it.
            (element, UiEvent::Clicked) if element.contains('/') => {
                if let Some((action, message)) = element.split_once('/') {
                    self.message_action(key, message, action, None);
                }
            }
            (FEED, UiEvent::EditLast) => {
                if let Some(index) = self.last_own(key) {
                    self.message_action(key, &id(index), "edit", None);
                }
            }
            (FEED, UiEvent::Edited((message, text))) => {
                if let Some(index) = index(&message) {
                    log::info(&format!("edited: {message}"));
                    self.edits.insert((seed, index), text);
                    chat::update(&tab, FEED, vec![put(vec![self.message(seed, index)])]);
                }
            }
            (FEED, UiEvent::EditCancelled) => log::info("edit-cancelled"),
            (FEED, UiEvent::ReplyCancelled) => {
                self.quoted.remove(key);
                log::info("reply-cancelled")
            }
            (FEED, UiEvent::ComposerAction((_, composed))) => {
                let text = format!("{} — rewritten by Claude", composed.text.trim());
                chat::update(&tab, FEED, vec![draft(&text)]);
            }
            // Selecting messages (part 5): Forward puts their bar over the field, Delete removes
            // them.
            (FEED, UiEvent::SelectionAction((action, messages))) if action == "forward" => {
                let quote = MessageQuote {
                    message: None,
                    author: format!("Forwarding {} messages", messages.len()),
                    color: 0,
                    text: messages.join(", "),
                };
                self.forwarding.insert(key.to_string(), messages);
                chat::update(&tab, FEED, vec![forwarding(Some(quote))]);
            }
            (FEED, UiEvent::SelectionAction((_, messages))) => {
                chat::update(&tab, FEED, vec![remove(messages)]);
            }
            (FEED, UiEvent::ForwardCancelled) => {
                self.forwarding.remove(key);
                log::info("forward-cancelled")
            }
            (FEED, UiEvent::Composed(composed)) => {
                if let Some(messages) = self.forwarding.remove(key) {
                    log::info(&format!("forwarded: {}", messages.join(", ")));
                }
                if !composed.text.trim().is_empty() || !composed.files.is_empty() {
                    self.send(key, composed)
                }
            }
            (FEED, UiEvent::ComposedWith((option, composed))) => {
                log::info(&format!("composed-with: {option}"));
                self.send(key, composed)
            }
            // A button under a message (part 5).
            (FEED, UiEvent::KeyboardPressed((message, button))) => {
                log::info(&format!("keyboard: {message}/{button}"));
            }
            // A vote, a reaction (part 5): a real chat would send them and put the message again.
            (FEED, UiEvent::PollVoted((message, part, options))) => {
                log::info(&format!("poll-voted: {message}/{part} {options:?}"));
            }
            (FEED, UiEvent::ReactionToggled((message, emoji))) => {
                log::info(&format!("reaction-toggled: {message} {emoji}"));
            }
            // The field's suggestions (part 5): the members for "@", the bot's commands for "/".
            (FEED, UiEvent::Suggest((trigger, text))) => {
                let items = suggested(&trigger, &text);
                chat::update(&tab, FEED, vec![suggestions(&text, items)]);
            }
            // A voice message or a round one played for the first time (part 4): it is heard.
            (FEED, UiEvent::MediaPlayed((message, part))) => {
                log::info(&format!("media-played: {message}/{part}"));
            }
            // Every link comes here (`own-links`): a web one opens in the browser, as a plugin
            // that asks first would after its question.
            (FEED, UiEvent::LinkClicked((message, link))) => {
                log::info(&format!("link: {message} {link}"));
                if link.starts_with("https://") || link.starts_with("http://") {
                    let _ = flux_plugin_api::system::open_url(&link);
                }
            }
            // A video's picture: the plugin would play it.
            (FEED, UiEvent::PartClicked((message, part)))
                if index(&message).is_some_and(|index| {
                    matches!(
                        self.message(seed, index).parts.get(part as usize),
                        Some(Part::Image(spec)) if spec.play
                    )
                }) =>
            {
                log::info(&format!("play: {message}/{part}"));
            }
            (FEED, UiEvent::PartClicked((message, _))) => {
                if let Some(index) = index(&message) {
                    // A download of a few steps: the viewer opened on a picture waits for it.
                    let timer = timers::after(Duration::from_millis(DOWNLOAD_STEP));
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
            ("titles", UiEvent::Clicked) => {
                self.profile = Some((key.to_string(), MEDIA));
                self.render_profile();
                ui::show(PROFILE);
            }
            // ⌘F, ⌘G, ⇧⌘G in the tab (API 0.5): the chat's search would show.
            (_, event @ (UiEvent::Find | UiEvent::FindNext | UiEvent::FindPrevious)) => {
                log::info(&format!("{event:?}: {key}"));
            }
            // The chat's menu, under its ⋯ in the tab (a picker anchored to an element of a tab).
            ("more", UiEvent::Clicked) => {
                flux_plugin_api::picker::Picker::menu()
                    .item(flux_plugin_api::picker::item("profile", "Profile").icon("info"))
                    .item(flux_plugin_api::picker::item("search", "Search").icon("search"))
                    .item(flux_plugin_api::picker::item("hide", "Hide the Chat").icon("close"))
                    .under(&tab, "more")
                    .open();
            }
            ("reveal", UiEvent::Clicked) => {
                let oldest = self.open.get(key).map_or(0, |chat| chat.oldest);
                chat::update(&tab, FEED, vec![reveal(&id(oldest))])
            }
            _ => {}
        }
    }

    /// The profile of the chat: its large header over its media by month — photos, videos (the
    /// length and ▶), some not downloaded yet (a click downloads them: `activated`).
    fn render_profile(&self) {
        let (Some((key, count)), false) = (self.profile.as_ref(), self.pictures.is_empty()) else {
            return;
        };
        let Some((_, info)) = chat_row(key) else {
            return;
        };
        const MONTHS: [&str; 4] = [
            "November 2026",
            "October 2026",
            "September 2026",
            "August 2026",
        ];
        let items = (0..*count)
            .map(|n| {
                let p = &self.pictures[n % self.pictures.len()];
                // The square shows the picture; its full size is there too: a click opens the
                // viewer (a messenger would download it first).
                let mut image = chat::picture(&p.path, p.width, p.height)
                    .full(&p.path)
                    .preview(p.preview.clone());
                if n % 7 == 3 {
                    image = image.label(&format!("0:{:02}", 10 + n % 50)).play();
                } else if n % 11 == 5 {
                    image = chat::pending_picture(p.width, p.height).preview(p.preview.clone());
                }
                gallery_item(&format!("media-{n}"), image, Some(MONTHS[(n / 16).min(3)]))
            })
            .collect();
        let more = if *count < 200 {
            History::More
        } else {
            History::Complete
        };
        set_view(
            PROFILE,
            column(
                "root",
                [
                    header("profile-header", info.title)
                        .avatar(AvatarSpec::initials(info.initials, info.color))
                        .subtitle([span("5 members").tone(Tone::Dim)])
                        .large()
                        .into(),
                    gallery("media", items, more, Some("No photos or videos yet")),
                ],
            ),
        );
    }

    fn profile_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::LoadOlder => {
                if let Some((_, count)) = self.profile.as_mut() {
                    *count += MEDIA;
                }
                self.render_profile();
            }
            UiEvent::Activated(key) => log::info(&format!("activated: {key}")),
            UiEvent::Visible((first, last)) => {
                log::info(&format!("media visible: {first} … {last}"))
            }
            UiEvent::Clicked => log::info("profile header clicked"),
            _ => {}
        }
    }

    /// A message's action: Reply puts its quote over the field — of the selected part (`quoted`)
    /// when there is one; Edit — on the user's own message — puts it in the field; Reply with
    /// Claude adds Claude's draft.
    fn message_action(&mut self, key: &str, message: &str, action: &str, quoted: Option<&str>) {
        let (Some(index), Some(chat)) = (index(message), self.open.get(key)) else {
            return;
        };
        let message = self.message(seed(key), index);
        let mut quote = message.quote("You");
        match quoted {
            Some(text) if action != "edit" => {
                quote.text = text.replace('\n', " ");
                self.quoted.insert(key.to_string(), text.to_string());
            }
            _ => {
                self.quoted.remove(key);
            }
        }
        let changes = match action {
            "reply" => vec![reply_to(Some(quote))],
            "edit" if message.outgoing => {
                vec![editing(Some((&message.id, &message_text(&message))))]
            }
            "edit" => {
                log::info("edit: only the user's own messages are edited");
                return;
            }
            _ => vec![
                reply_to(Some(quote)),
                draft("Claude's draft: sounds good, let's ship it on Friday."),
            ],
        };
        chat::update(&chat.tab, FEED, changes);
    }

    /// The user's last message in a chat: the last one sent, or the newest own one of the
    /// history.
    fn last_own(&self, key: &str) -> Option<u64> {
        let seed = seed(key);
        let sent = self
            .sent
            .keys()
            .filter(|(chat, _)| *chat == seed)
            .map(|(_, index)| *index)
            .max();
        sent.or_else(|| {
            (0..TOTAL)
                .rev()
                .find(|&index| self.message(seed, index).outgoing)
        })
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
            // The message as it is now: edited meanwhile, perhaps.
            let seed = seed(&key);
            let shown = match index(&message.id) {
                Some(index) => {
                    self.sent.insert((seed, index), updated.clone());
                    self.message(seed, index)
                }
                None => updated.clone(),
            };
            chat::update(&view_tab(&key), FEED, vec![put(vec![shown])]);
            if let Some(next) = next {
                let timer = timers::after(Duration::from_millis(900));
                self.deliveries.insert(timer, (key, updated, next));
            }
        } else if let Some((key, index, done)) = self.downloads.remove(&timer) {
            self.put_download(&key, index, done);
            if done < 1.0 {
                let timer = timers::after(Duration::from_millis(DOWNLOAD_STEP));
                self.downloads
                    .insert(timer, (key, index, (done + 0.35).min(1.0)));
            }
        }
    }

    /// A message's files and pictures being downloaded (`done` < 1) or downloaded: a picture's
    /// full size too (the viewer opened on it swaps to it).
    fn put_download(&mut self, key: &str, index: u64, done: f32) {
        if done >= 1.0 {
            self.downloaded.insert((seed(key), index));
        }
        let mut message = self.message(seed(key), index);
        if done < 1.0 {
            for part in &mut message.parts {
                match part {
                    Part::File(file) if file.path.is_none() => file.progress = Some(done),
                    Part::Image(spec) if !spec.play => spec.progress = Some(done),
                    Part::Voice(spec) if spec.path.is_none() => spec.progress = Some(done),
                    Part::Video(spec) if spec.path.is_none() => spec.poster.progress = Some(done),
                    _ => {}
                }
            }
        }
        chat::update(&view_tab(key), FEED, vec![put(vec![message])]);
    }

    /// The field's message comes back as an outgoing one: sending → sent → read. A picture file
    /// comes as a photo (Flux shows it: the user handed it to the conversation), another file as a
    /// file.
    fn send(&mut self, key: &str, composed: Composed) {
        let index = self.next;
        self.next += 1;
        let mut message = Message::new(&id(index), index, now()).outgoing(Delivery::Sending);
        if let Some(reply) = &composed.reply_to
            && let Some(original) = flux_index(reply)
        {
            let mut quote = self.message(seed(key), original).quote("You");
            if let Some(text) = self.quoted.get(key) {
                quote.text = text.replace('\n', " ");
            }
            message = message.reply(quote);
        }
        self.quoted.remove(key);
        for path in &composed.files {
            let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
            let picture = [".png", ".jpg", ".jpeg", ".webp", ".gif"]
                .iter()
                .any(|extension| name.to_ascii_lowercase().ends_with(extension));
            message = if picture {
                // The plugin can't read the file (it is outside its folders): the size is
                // Flux's to guess.
                message.part(chat::image(chat::picture(path, 0, 0)))
            } else {
                let size = std::fs::metadata(path).map_or(0, |meta| meta.len());
                message.part(file(name, size, Some(path)))
            };
        }
        // The field's formatting comes as Markdown (API 0.5): read crudely.
        for part in markdown_parts(composed.text.trim()) {
            message = message.part(part);
        }
        self.sent.insert((seed(key), index), message.clone());
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
        let message = match self.sent.get(&(seed, index)) {
            Some(sent) => sent.clone(),
            None => self.history_message(seed, index),
        };
        let mut message = match self.edits.get(&(seed, index)) {
            Some(text) => with_text(message, text),
            None => message,
        };
        // Downloaded: the files on disk, the pictures in full size.
        if self.downloaded.contains(&(seed, index)) {
            for part in &mut message.parts {
                match part {
                    Part::File(file) => {
                        file.progress = None;
                        file.path = Some(format!("{}/notes.txt", storage::dir()));
                    }
                    Part::Image(spec) if !spec.play => {
                        spec.progress = None;
                        if spec.path.is_none() {
                            spec.path = self.pictures.first().map(|picture| picture.path.clone());
                        }
                        spec.full = spec.path.clone();
                    }
                    _ => {}
                }
            }
        }
        // Edit only on the user's own messages (API 0.5); a notice offers nothing.
        if matches!(message.parts.first(), Some(Part::Notice(_))) {
            return message.actions(Vec::<String>::new());
        }
        // Part 5: views, a forum topic's label, buttons under the message.
        if !message.outgoing && index % 11 == 3 {
            message = message
                .views(1_200 + index as u32 * 37)
                .topic("Design", (index % 7) as u8)
                .keyboard_row(vec![
                    chat::keyboard_button("yes", "Yes", chat::KeyboardKind::Action),
                    chat::keyboard_button("no", "No", chat::KeyboardKind::Action),
                ])
                .keyboard_row(vec![chat::keyboard_button(
                    "docs",
                    "Open the docs",
                    chat::KeyboardKind::Link,
                )]);
        }
        match message.outgoing {
            true => message.actions(["reply", "edit", "claude"]),
            false => message.actions(["reply", "claude"]),
        }
    }

    /// The `index`th message of a chat's generated history.
    fn history_message(&self, seed: u64, index: u64) -> Message {
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
        if seed == crate::seed(MEDIA_CHAT) && index + MEDIA_MESSAGES >= TOTAL {
            return self.media_message(message, index);
        }
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
            // Videos (API 0.5): the length and ▶ over the first frame, a GIF, a round one.
            12 if !self.pictures.is_empty() => message
                .part(chat::image(picture(index).label("0:42").play()))
                .text("The demo of the new map screen"),
            13 if !self.pictures.is_empty() => {
                message.part(chat::image(picture(index).label("GIF").play()))
            }
            14 if !self.pictures.is_empty() => {
                let p = &self.pictures[(index as usize) % self.pictures.len()];
                message.part(chat::image(
                    chat::picture(&p.path, 240, 240)
                        .preview(p.preview.clone())
                        .shape(ImageShape::Circle)
                        .label("0:07")
                        .play(),
                ))
            }
            // Polls (part 5): one to vote in, a quiz answered with its explanation.
            15 => message.part(chat::poll(poll_spec(
                vec![rich("Where do we meet on Friday?")],
                "Anonymous poll",
                vec![
                    poll_option(vec![rich("The office")], 0),
                    poll_option(vec![rich("The café by the river")], 0),
                    poll_option(vec![rich("Online")], 0),
                ],
                "7 votes",
            ))),
            16 => message.part(chat::poll(
                poll_spec(
                    vec![rich("Which keeps an idle editor at 0 % CPU?")],
                    "Quiz",
                    vec![
                        poll_option(vec![rich("Redrawing every frame")], 25).chosen(),
                        poll_option(vec![rich("Frames only for what plays")], 75).correct(),
                    ],
                    "4 answers",
                )
                .answered()
                .explanation(vec![rich("Nothing on screen changes — nothing is drawn.")]),
            )),
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

/// The field's Markdown (API 0.5 formatting) as parts, crudely: a ``` block is a code part; in the
/// rest `**bold**`, `__italic__`, `~~strikethrough~~`, `||spoiler||`, `` `code` `` and
/// `[text](address)` are styled spans (marks don't nest).
fn markdown_parts(text: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    for (index, piece) in text.split("```").enumerate() {
        let piece = piece.trim_matches('\n');
        if piece.is_empty() {
            continue;
        }
        if index % 2 == 1 {
            parts.push(code(piece, None));
        } else {
            parts.push(chat::text(markdown_spans(piece)));
        }
    }
    parts
}

/// One paragraph's inline marks as spans.
fn markdown_spans(text: &str) -> Vec<flux_plugin_api::conversation::RichSpan> {
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    'scan: while !rest.is_empty() {
        for mark in ["**", "__", "~~", "||", "`"] {
            if let Some(after) = rest.strip_prefix(mark)
                && let Some(end) = after.find(mark)
                && end > 0
            {
                let inner = &after[..end];
                let span = match mark {
                    "**" => rich(inner).bold(),
                    "__" => rich(inner).italic(),
                    "~~" => rich(inner).strikethrough(),
                    "||" => rich(inner).spoiler(),
                    _ => rich(inner).code(),
                };
                if !plain.is_empty() {
                    spans.push(rich(&std::mem::take(&mut plain)));
                }
                spans.push(span);
                rest = &after[end + mark.len()..];
                continue 'scan;
            }
        }
        if let Some(after) = rest.strip_prefix('[')
            && let Some((label, tail)) = after.split_once("](")
            && let Some((address, tail)) = tail.split_once(')')
            && !label.contains(']')
        {
            if !plain.is_empty() {
                spans.push(rich(&std::mem::take(&mut plain)));
            }
            spans.push(rich(label).link(address));
            rest = tail;
            continue;
        }
        let mut chars = rest.chars();
        if let Some(c) = chars.next() {
            plain.push(c);
        }
        rest = chars.as_str();
    }
    if !plain.is_empty() {
        spans.push(rich(&plain));
    }
    spans
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

/// The text of a message's text parts, as the field takes it to edit.
fn message_text(message: &Message) -> String {
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text(spans) => Some(spans.iter().map(|span| span.text.as_str()).collect()),
            _ => None,
        })
        .collect::<Vec<String>>()
        .join("\n")
}

/// The message with its text replaced by `text` (in place of its first text part, or after its
/// other parts), marked edited.
fn with_text(mut message: Message, text: &str) -> Message {
    let first = message
        .parts
        .iter()
        .position(|part| matches!(part, Part::Text(_)));
    message.parts.retain(|part| !matches!(part, Part::Text(_)));
    let part = chat::text(vec![rich(text)]);
    match first {
        Some(at) => message.parts.insert(at.min(message.parts.len()), part),
        None => message.parts.push(part),
    }
    message.edited = true;
    message
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
