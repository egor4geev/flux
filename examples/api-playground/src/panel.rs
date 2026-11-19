//! The field's panel (API 0.5, FluxGram part 4): the window "panel" — a conversation whose field
//! has the panel of emoji, stickers and GIFs. The emoji are Flux's own; the stickers and the GIFs
//! are this plugin's, from `panel/` of its data folder (a plugin gives Flux only files in its
//! folders; a scenario copies them there, as the chat playground's media): `sticker.jpg` and
//! `sticker-0.fxa` … `sticker-3.fxa` (animations of 144 px), `gif.mp4` with its poster `gif.jpg`.
//!
//! - Stickers: the favorite and the recent ones, two sets; a search answers with a section of what
//!   was found (by a set's name in English or Russian, or by the emoji).
//! - GIFs: the saved ones, a page at a time — `panel-load-more` shows the spinner, then the next
//!   page comes; a search pages the same way.
//! - A choice posts a message telling what was chosen (a sticker with it); a message sent from the
//!   field comes back as the user's. Every event of the panel goes to the log.

use std::time::Duration;

use flux_plugin_api::conversation::{
    self as chat, Delivery, History, Message, PanelContent, PanelItem, PanelSection, PanelTab,
    composer, panel, panel_content, panel_item, panel_section, picture, put, sticker,
};
use flux_plugin_api::view::{column, conversation, set_view};
use flux_plugin_api::{Event, UiEvent, log, storage, timers, tr};

use crate::Playground;

/// The tool window of the manifest.
pub(crate) const PANEL: &str = "panel";
const FEED: &str = "feed";
/// GIFs come this many to a page, up to this many in all.
const GIF_PAGE: usize = 12;
const GIF_TOTAL: usize = 48;
/// A page more takes this long to come (its spinner first).
const PAGE_DELAY: Duration = Duration::from_millis(500);
/// The date of the first message, and the step between messages.
const START: i64 = 1_763_380_000;

/// The sticker sets: key, title, the words a search finds it by, the emoji of its stickers.
const SETS: [(&str, &str, &[&str], &[&str]); 2] = [
    (
        "cats",
        "Cats",
        &["cat", "кот", "кош"],
        &["😺", "😸", "😹", "😻", "😼", "😽", "🙀", "😿", "😾", "🐱"],
    ),
    (
        "dogs",
        "Dogs",
        &["dog", "пес", "пёс", "собак"],
        &["🐶", "🐕", "🦮", "🐩", "🐺", "🦊", "🐾"],
    ),
];

/// What the panel's window remembers.
#[derive(Default)]
pub(crate) struct State {
    /// Messages posted so far.
    messages: u64,
    /// The GIF tab's search and how many of its GIFs are shown; the timer of the page coming.
    gif_query: String,
    gifs: usize,
    page_timer: Option<u64>,
    /// The sticker tab's search.
    sticker_query: String,
}

/// A file of `panel/` in the plugin's data folder.
fn file(name: &str) -> String {
    format!("{}/panel/{name}", storage::dir())
}

/// A sticker: its picture, its animation (one of four), its emoji.
fn sticker_item(key: &str, index: usize, emoji: &str) -> PanelItem {
    let animation = file(&format!("sticker-{}.fxa", index % 4));
    panel_item(
        key,
        picture(&file("sticker.jpg"), 144, 144).animation(&animation, true),
    )
    .emoji(emoji)
}

/// A GIF: proportions of three kinds, for rows of different widths.
fn gif_item(key: &str, index: usize) -> PanelItem {
    let (width, height) = match index % 3 {
        0 => (320, 240),
        1 => (240, 320),
        _ => (400, 200),
    };
    panel_item(key, picture(&file("gif.jpg"), width, height)).video(&file("gif.mp4"))
}

/// A set's stickers in a section keyed `section` ("set-cats", "found").
fn set_items(section: &str, set: usize) -> Vec<PanelItem> {
    let (key, _, _, emoji) = SETS[set];
    emoji
        .iter()
        .enumerate()
        .map(|(index, emoji)| sticker_item(&format!("{section}:{key}-{index}"), index + set, emoji))
        .collect()
}

fn stickers(query: &str) -> PanelContent {
    let query = query.trim().to_lowercase();
    if !query.is_empty() {
        let mut found: Vec<PanelItem> = Vec::new();
        for (set, (key, title, words, emoji)) in SETS.iter().enumerate() {
            let whole = title.to_lowercase().starts_with(&query)
                || words
                    .iter()
                    .any(|word| query.starts_with(word) || word.starts_with(&query));
            for (index, item) in set_items("found", set).into_iter().enumerate() {
                if whole || emoji[index] == query {
                    found.push(item);
                }
            }
            let _ = key;
        }
        let sections = vec![panel_section("found", "", None, found)];
        return panel_content(PanelTab::Stickers, &query, sections, History::Complete);
    }
    let favorites = vec![
        sticker_item("favorites:cats-3", 3, "😻"),
        sticker_item("favorites:dogs-0", 1, "🐶"),
    ];
    let recent = vec![
        sticker_item("recent:cats-1", 1, "😸"),
        sticker_item("recent:dogs-4", 0, "🐺"),
        sticker_item("recent:cats-6", 2, "🙀"),
    ];
    let mut sections: Vec<PanelSection> = vec![
        panel_section("favorites", &tr("Favorites"), None, favorites),
        panel_section("recent", &tr("Recent"), None, recent),
    ];
    for (set, (key, title, _, _)) in SETS.iter().enumerate() {
        let icon = picture(&file("sticker.jpg"), 144, 144);
        sections.push(panel_section(
            &format!("set-{key}"),
            &tr(title),
            Some(icon),
            set_items(&format!("set-{key}"), set),
        ));
    }
    panel_content(PanelTab::Stickers, "", sections, History::Complete)
}

/// The GIFs shown: the saved ones, or those found, `count` of them; `loading` — a page more is
/// coming.
fn gifs(query: &str, count: usize, loading: bool) -> PanelContent {
    let (section, title) = match query.is_empty() {
        true => ("saved", tr("Saved")),
        false => ("found", String::new()),
    };
    let items = (0..count)
        .map(|index| gif_item(&format!("{section}:gif-{index}"), index))
        .collect();
    let more = if loading {
        History::Loading
    } else if count < GIF_TOTAL {
        History::More
    } else {
        History::Complete
    };
    let sections = vec![panel_section(section, &title, None, items)];
    panel_content(PanelTab::Gifs, query, sections, more)
}

impl Playground {
    /// The panel window's events: answered here (none), or handed back.
    pub(crate) fn handle_panel(&mut self, event: Event) -> Option<Event> {
        match event {
            Event::ToolWindowShown(window) if window == PANEL => self.render_panel(),
            Event::Timer(id) if Some(id) == self.panel.page_timer => {
                self.panel.page_timer = None;
                self.panel.gifs = (self.panel.gifs + GIF_PAGE).min(GIF_TOTAL);
                let content = gifs(&self.panel.gif_query, self.panel.gifs, false);
                chat::update(PANEL, FEED, vec![panel(content)]);
            }
            Event::Ui(input) if input.window == PANEL => self.panel_event(input.event),
            other => return Some(other),
        }
        None
    }

    fn render_panel(&self) {
        let field =
            composer(&tr("Message…")).panel([PanelTab::Emoji, PanelTab::Stickers, PanelTab::Gifs]);
        let feed = conversation(
            FEED,
            chat::spec().composer(field).empty_text(&tr(
                "Choose an emoji, a sticker or a GIF in the panel by the field",
            )),
        );
        set_view(PANEL, column("root", [feed]));
    }

    fn panel_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::PanelOpened(tab) => {
                log::info(&format!("panel-opened {tab:?}"));
                let content = match tab {
                    PanelTab::Stickers => stickers(&self.panel.sticker_query),
                    _ => {
                        self.panel.gifs = self.panel.gifs.max(GIF_PAGE);
                        gifs(&self.panel.gif_query, self.panel.gifs, false)
                    }
                };
                chat::update(PANEL, FEED, vec![panel(content)]);
            }
            UiEvent::PanelSearched((tab, text)) => {
                log::info(&format!("panel-searched {tab:?} «{text}»"));
                let content = match tab {
                    PanelTab::Stickers => {
                        self.panel.sticker_query = text.clone();
                        stickers(&text)
                    }
                    _ => {
                        self.panel.gif_query = text.clone();
                        self.panel.gifs = GIF_PAGE;
                        self.panel.page_timer = None;
                        gifs(&text, GIF_PAGE, false)
                    }
                };
                chat::update(PANEL, FEED, vec![panel(content)]);
            }
            UiEvent::PanelLoadMore(tab) => {
                log::info(&format!(
                    "panel-load-more {tab:?} after {}",
                    self.panel.gifs
                ));
                if tab == PanelTab::Gifs && self.panel.page_timer.is_none() {
                    let content = gifs(&self.panel.gif_query, self.panel.gifs, true);
                    chat::update(PANEL, FEED, vec![panel(content)]);
                    self.panel.page_timer = Some(timers::after(PAGE_DELAY));
                }
            }
            UiEvent::PanelVisible((tab, first, last)) => {
                log::info(&format!("panel-visible {tab:?} {first} … {last}"));
            }
            UiEvent::PanelChosen((tab, key)) => {
                log::info(&format!("panel-chosen {tab:?} {key}"));
                let mut message = self.panel_message().text(&format!("{tab:?}: {key}"));
                if tab == PanelTab::Stickers {
                    let emoji = key.rsplit('-').next().and_then(|n| n.parse::<usize>().ok());
                    message =
                        message.part(sticker(picture(&file("sticker.jpg"), 144, 144).animation(
                            &file(&format!("sticker-{}.fxa", emoji.unwrap_or(0) % 4)),
                            true,
                        )));
                }
                chat::update(PANEL, FEED, vec![put(vec![message])]);
            }
            UiEvent::Composed(composed) => {
                log::info(&format!("composed «{}»", composed.text));
                let message = self.panel_message().text(&composed.text);
                chat::update(PANEL, FEED, vec![put(vec![message])]);
            }
            _ => {}
        }
    }

    /// The next message of the window: the user's, read.
    fn panel_message(&mut self) -> Message {
        self.panel.messages += 1;
        let index = self.panel.messages;
        Message::new(&format!("m{index}"), index, START + index as i64 * 60)
            .outgoing(Delivery::Read)
    }
}
