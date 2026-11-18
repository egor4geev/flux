//! A conversation (API 0.5): a message feed with a field, as a chat. The view holds the element
//! ([`crate::view::conversation`]); its messages come as changes by id — `put`, `remove`, the
//! history's state — through [`update`], never as the whole feed:
//!
//! ```ignore
//! use flux_plugin_api::conversation::*;
//!
//! let hello = Message::new("m1", 1, 1_763_380_000)
//!     .author("Anna", 6, Some(AvatarSpec::initials("A", 6)))
//!     .text("Hi! See the **plan**")
//!     .part(image(picture("/…/plan.jpg", 1280, 720).preview(minithumbnail)))
//!     .reaction("🔥", 2, true);
//! update("chat", "feed", vec![put(vec![hello]), older(History::More)]);
//! ```
//!
//! Scrolling near the oldest message comes as `UiEvent::LoadOlder` (once per `older` state): send
//! `older(History::Loading)`, the page with `put`, then `older(More | Complete)`. The messages on
//! screen come as `UiEvent::Visible((first, last))`; the field as `UiEvent::Composed`.

use crate::host::ui::RowAction;
pub use crate::host::ui::{
    AvatarSpec, CodeBlock, Composed, ComposerSpec, ConversationChange as Change, ConversationSpec,
    Delivery, FileSpec, History, ImageShape, ImageSpec, LinkPreview, Message, MessageAuthor,
    MessageEdit, MessagePart as Part, MessageQuote, PlaceholderSpec, Reaction, RichSpan,
};

/// Sends changes of the `element` conversation of `window`'s view, in order.
pub fn update(window: &str, element: &str, changes: Vec<Change>) {
    crate::host::ui::update_conversation(window, element, &changes);
}

// --- Changes ---

/// Adds messages, or replaces those with the same id; each goes where its `order` puts it.
pub fn put(messages: Vec<Message>) -> Change {
    Change::Put(messages)
}

/// Removes messages by id.
pub fn remove(ids: impl IntoIterator<Item = impl Into<String>>) -> Change {
    Change::Remove(ids.into_iter().map(Into::into).collect())
}

/// Removes every message.
pub fn clear() -> Change {
    Change::Clear
}

/// Whether there are older messages, or they are loading.
pub fn older(history: History) -> Change {
    Change::Older(history)
}

/// Whether there are newer messages (after a jump into the history), or they are loading.
pub fn newer(history: History) -> Change {
    Change::Newer(history)
}

/// The "New messages" line above this message; `None` — no line.
pub fn unread_from(message: Option<&str>) -> Change {
    Change::UnreadFrom(message.map(str::to_string))
}

/// Scrolls to the message and marks it for a moment.
pub fn reveal(message: &str) -> Change {
    Change::Reveal(message.to_string())
}

/// The quote over the field; `None` — no answer.
pub fn reply_to(quote: Option<MessageQuote>) -> Change {
    Change::ReplyTo(quote)
}

/// Replaces the field's text.
pub fn draft(text: &str) -> Change {
    Change::Draft(text.to_string())
}

/// The field edits the message `message` with `text` (API 0.5): ↵ comes as `edited`, Esc as
/// `edit-cancelled`; `None` — back to a new message.
pub fn editing(edit: Option<(&str, &str)>) -> Change {
    Change::Editing(edit.map(|(message, text)| MessageEdit {
        message: message.to_string(),
        text: text.to_string(),
    }))
}

// --- Messages ---

impl Message {
    /// An incoming message without parts: `id` names it, `order` places it, `date` is in seconds
    /// since 1970-01-01 UTC.
    pub fn new(id: &str, order: u64, date: i64) -> Self {
        Message {
            id: id.to_string(),
            order,
            outgoing: false,
            author: None,
            date,
            edited: false,
            delivery: None,
            forwarded: None,
            reply: None,
            parts: Vec::new(),
            reactions: Vec::new(),
            actions: None,
        }
    }

    /// Written by the user: on the right, with a delivery mark.
    pub fn outgoing(mut self, delivery: Delivery) -> Self {
        self.outgoing = true;
        self.delivery = Some(delivery);
        self
    }

    /// Who wrote it: the name in one of the seven avatar colors; with an avatar in a group.
    pub fn author(mut self, name: &str, color: u8, avatar: Option<AvatarSpec>) -> Self {
        self.author = Some(MessageAuthor {
            name: name.to_string(),
            color,
            avatar,
        });
        self
    }

    pub fn edited(mut self) -> Self {
        self.edited = true;
        self
    }

    /// "Forwarded from …".
    pub fn forwarded(mut self, from: &str) -> Self {
        self.forwarded = Some(from.to_string());
        self
    }

    /// The message it answers.
    pub fn reply(mut self, quote: MessageQuote) -> Self {
        self.reply = Some(quote);
        self
    }

    /// Plain text as a part.
    pub fn text(self, text: &str) -> Self {
        self.part(Part::Text(vec![rich(text)]))
    }

    pub fn part(mut self, part: Part) -> Self {
        self.parts.push(part);
        self
    }

    /// The conversation's message actions this message offers, by id (API 0.5): Edit only on the
    /// user's own messages. Without it — every one.
    pub fn actions(mut self, actions: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.actions = Some(actions.into_iter().map(Into::into).collect());
        self
    }

    pub fn reaction(mut self, emoji: &str, count: u32, chosen: bool) -> Self {
        self.reactions.push(Reaction {
            emoji: emoji.to_string(),
            count,
            chosen,
        });
        self
    }

    /// The quote of this message for a reply to it: its author (or `me` for the user's own) and
    /// its text in one line, or what it is.
    pub fn quote(&self, me: &str) -> MessageQuote {
        let (author, color) = match (&self.author, self.outgoing) {
            (Some(author), _) => (author.name.clone(), author.color),
            (None, true) => (me.to_string(), 0),
            (None, false) => (String::new(), 0),
        };
        let text = self
            .parts
            .first()
            .map(|part| match part {
                Part::Text(spans) | Part::Quote(spans) | Part::Notice(spans) => {
                    spans.iter().map(|span| span.text.as_str()).collect()
                }
                Part::Code(code) => code.text.clone(),
                Part::Image(_) => "Photo".to_string(),
                Part::Sticker(_) => "Sticker".to_string(),
                Part::File(file) => file.name.clone(),
                Part::Link(link) => link.url.clone(),
                Part::Placeholder(placeholder) => placeholder.text.clone(),
            })
            .unwrap_or_default();
        MessageQuote {
            message: Some(self.id.clone()),
            author,
            color,
            text: text.lines().next().unwrap_or_default().to_string(),
        }
    }
}

// --- Parts ---

/// A piece of text without a style; style it with [`RichSpan`]'s methods.
pub fn rich(text: &str) -> RichSpan {
    RichSpan {
        text: text.to_string(),
        bold: false,
        italic: false,
        underline: false,
        strikethrough: false,
        code: false,
        spoiler: false,
        highlight: false,
        link: None,
    }
}

impl RichSpan {
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }
    pub fn underline(mut self) -> Self {
        self.underline = true;
        self
    }
    pub fn strikethrough(mut self) -> Self {
        self.strikethrough = true;
        self
    }
    /// The code font.
    pub fn code(mut self) -> Self {
        self.code = true;
        self
    }
    /// Hidden under a veil until clicked.
    pub fn spoiler(mut self) -> Self {
        self.spoiler = true;
        self
    }
    /// Marked as a match of a search.
    pub fn highlight(mut self) -> Self {
        self.highlight = true;
        self
    }
    /// A link: web ones open in the browser, others come as `UiEvent::LinkClicked`.
    pub fn link(mut self, target: &str) -> Self {
        self.link = Some(target.to_string());
        self
    }
}

/// Styled text as a part.
pub fn text(spans: Vec<RichSpan>) -> Part {
    Part::Text(spans)
}

/// A code block, highlighted as the language's files are ("rust"; `None` — plain).
pub fn code(text: &str, language: Option<&str>) -> Part {
    Part::Code(CodeBlock {
        text: text.to_string(),
        language: language.map(str::to_string),
    })
}

/// A quote within the text.
pub fn quote(spans: Vec<RichSpan>) -> Part {
    Part::Quote(spans)
}

/// A photo; pictures in a row make an album.
pub fn image(spec: ImageSpec) -> Part {
    Part::Image(spec)
}

/// A sticker: no bubble, the time under it.
pub fn sticker(spec: ImageSpec) -> Part {
    Part::Sticker(spec)
}

/// A file of `size` bytes; `path` once it is downloaded.
pub fn file(name: &str, size: u64, path: Option<&str>) -> Part {
    Part::File(FileSpec {
        name: name.to_string(),
        size,
        path: path.map(str::to_string),
        progress: None,
    })
}

/// A dim line with an icon: what Flux can't show (yet) — "Voice message · 0:42".
pub fn placeholder(icon: Option<&str>, text: &str) -> Part {
    Part::Placeholder(PlaceholderSpec {
        icon: icon.map(str::to_string),
        text: text.to_string(),
    })
}

/// A line of the chat itself, in the middle: "Anna joined the group".
pub fn notice(text: &str) -> Part {
    Part::Notice(vec![rich(text)])
}

/// A link's preview: the site, the title, the text, a picture.
pub fn link_preview(
    url: &str,
    site: Option<&str>,
    title: Option<&str>,
    description: Option<&str>,
    image: Option<ImageSpec>,
) -> Part {
    Part::Link(LinkPreview {
        url: url.to_string(),
        site: site.map(str::to_string),
        title: title.map(str::to_string),
        description: description.map(str::to_string),
        image,
    })
}

// --- Pictures and avatars ---

/// A picture of `width` × `height` pixels at an absolute path (in the plugin's folders); the size
/// keeps its room before it loads (0 — unknown).
pub fn picture(path: &str, width: u32, height: u32) -> ImageSpec {
    ImageSpec {
        path: Some(path.to_string()),
        full: None,
        preview: None,
        width,
        height,
        progress: None,
        shape: ImageShape::Rounded,
        label: None,
        play: false,
    }
}

/// A picture not downloaded yet: its preview blurred (or a dim box) in its room.
pub fn pending_picture(width: u32, height: u32) -> ImageSpec {
    ImageSpec {
        path: None,
        ..picture("", width, height)
    }
}

impl ImageSpec {
    /// The tiny JPEG (a few dozen pixels, up to 4 KB) shown blurred until the file is there.
    pub fn preview(mut self, jpeg: Vec<u8>) -> Self {
        self.preview = Some(jpeg);
        self
    }
    /// A larger file for the viewer.
    pub fn full(mut self, path: &str) -> Self {
        self.full = Some(path.to_string());
        self
    }
    /// Being downloaded: 0.0–1.0.
    pub fn progress(mut self, done: f32) -> Self {
        self.progress = Some(done);
        self
    }
    pub fn shape(mut self, shape: ImageShape) -> Self {
        self.shape = shape;
        self
    }
    /// A short text in a pill at the corner (API 0.5): a video's length "0:42", "GIF".
    pub fn label(mut self, label: &str) -> Self {
        self.label = Some(label.to_string());
        self
    }
    /// A ▶ in the middle (API 0.5): a video, a GIF, a round video message; a click comes as
    /// `part-clicked`.
    pub fn play(mut self) -> Self {
        self.play = true;
        self
    }
}

impl AvatarSpec {
    /// The initials on one of the seven avatar colors (0–6).
    pub fn initials(initials: &str, color: u8) -> Self {
        AvatarSpec {
            image: None,
            initials: initials.to_string(),
            color,
        }
    }
    /// A picture (absolute, in the plugin's folders); the initials while it loads.
    pub fn picture(mut self, path: &str) -> Self {
        self.image = Some(path.to_string());
        self
    }
}

// --- The field ---

/// The message field: its placeholder; files, buttons by the builder's methods.
pub fn composer(placeholder: &str) -> ComposerSpec {
    ComposerSpec {
        placeholder: placeholder.to_string(),
        attachments: false,
        actions: Vec::new(),
        formatting: false,
    }
}

impl ComposerSpec {
    /// Files and pictures: the paperclip, dropping, pasting.
    pub fn attachments(mut self) -> Self {
        self.attachments = true;
        self
    }
    /// A button beside Send; a press comes as `UiEvent::ComposerAction((id, composed))`.
    pub fn action(mut self, id: &str, label: &str, icon: Option<&str>) -> Self {
        self.actions.push(row_action(id, label, icon));
        self
    }
    /// Formatting (API 0.5): the field's menu and keys (⌘B, ⌘I, ⇧⌘X, ⇧⌘P, ⇧⌘M, ⌘K) wrap the
    /// selection in Markdown marks — `**bold**`, `__italic__`, `~~strikethrough~~`,
    /// `||spoiler||`, `` `monospace` ``, `[text](address)`; the text is sent with them.
    pub fn formatting(mut self) -> Self {
        self.formatting = true;
        self
    }
}

/// A conversation's spec: reading only, no actions; add them with the builder's methods.
pub fn spec() -> ConversationSpec {
    ConversationSpec {
        composer: None,
        empty_text: None,
        message_actions: Vec::new(),
    }
}

impl ConversationSpec {
    pub fn composer(mut self, composer: ComposerSpec) -> Self {
        self.composer = Some(composer);
        self
    }
    /// Shown in the middle while there are no messages.
    pub fn empty_text(mut self, text: &str) -> Self {
        self.empty_text = Some(text.to_string());
        self
    }
    /// An action of every message: in its menu, and with an icon on its hover bar; a choice comes
    /// as `UiEvent::MessageAction((message, id))` — chosen while part of the message's text is
    /// selected, as `UiEvent::MessageActionQuoted((message, id, text, offset))`.
    pub fn message_action(mut self, id: &str, label: &str, icon: Option<&str>) -> Self {
        self.message_actions.push(row_action(id, label, icon));
        self
    }
}

fn row_action(id: &str, label: &str, icon: Option<&str>) -> RowAction {
    RowAction {
        id: id.to_string(),
        label: Some(label.to_string()),
        icon: icon.map(str::to_string),
        tooltip: None,
        primary: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_builds_and_quotes_itself() {
        let message = Message::new("m1", 7, 0)
            .author("Anna", 6, None)
            .text("Hello\nthere")
            .reaction("🔥", 2, true);
        assert_eq!(message.parts.len(), 1);
        let quote = message.quote("You");
        assert_eq!(quote.text, "Hello");
        assert_eq!(quote.author, "Anna");
        assert_eq!(quote.message.as_deref(), Some("m1"));
        let mine = Message::new("m2", 8, 0).outgoing(Delivery::Sending);
        assert_eq!(mine.quote("You").author, "You");
        assert!(matches!(remove(["m1"]), Change::Remove(ids) if ids == ["m1"]));
    }
}
