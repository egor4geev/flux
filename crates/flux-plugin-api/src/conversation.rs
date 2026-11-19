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
    AnimationSpec, AvatarSpec, CodeBlock, Composed, ComposerSpec, ConversationChange as Change,
    ConversationSpec, Delivery, FileSpec, History, ImageShape, ImageSpec, KeyboardButton,
    KeyboardKind, LinkPreview, Message, MessageAuthor, MessageEdit, MessagePart as Part,
    MessageQuote, PanelContent, PanelItem, PanelSection, PanelTab, PinnedSpec, PlaceholderSpec,
    PollOption, PollSpec, Reaction, ReplyKeyboard, RichSpan, Suggestion, TopicLabel, VideoKind,
    VideoSpec, VoiceSpec,
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

/// What a tab of the field's panel shows (API 0.5, part 4): replaces the tab's content.
pub fn panel(content: PanelContent) -> Change {
    Change::Panel(content)
}

/// Replaces items of a panel's tab by their keys (API 0.5, part 4): a sticker downloaded.
pub fn panel_put(tab: PanelTab, items: Vec<PanelItem>) -> Change {
    Change::PanelPut((tab, items))
}

/// The bar of the chat's pinned message over the feed (API 0.5, part 5); `None` — no bar. Build
/// it with [`pinned_spec`]; a click on it comes as `UiEvent::PinnedClicked(message)`, one on its
/// action as `UiEvent::PinnedAction((message, id))`.
pub fn pinned(spec: Option<PinnedSpec>) -> Change {
    Change::Pinned(spec)
}

/// The field's suggestions (API 0.5, part 5) for `text` — what `UiEvent::Suggest((trigger,
/// text))` asked: the list over the field; an answer for another text than the one at the caret
/// now is dropped; empty — the list closes. Build the items with [`Suggestion::new`].
pub fn suggestions(text: &str, items: Vec<Suggestion>) -> Change {
    Change::Suggestions((text.to_string(), items))
}

/// The bot's keyboard over the field (API 0.5, part 5); `None` — no keyboard (and no ⌨). Build it
/// with [`reply_keyboard_spec`]; a press comes as `UiEvent::KeyboardPressed(("", id))`.
pub fn reply_keyboard(keyboard: Option<ReplyKeyboard>) -> Change {
    Change::ReplyKeyboard(keyboard)
}

/// The messages to forward, over the field (API 0.5, part 5): a bar like the reply's quote — its
/// `author` the title ("Forwarding 3 messages"), its `text` the line ("Anna, Boris: Hi…"); `None`
/// — no bar. Its × comes as `UiEvent::ForwardCancelled`; ↵ as `UiEvent::Composed` (the field may
/// be empty). Putting it clears the quote being answered, and a quote clears it.
pub fn forwarding(quote: Option<MessageQuote>) -> Change {
    Change::Forwarding(quote)
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
            views: None,
            topic: None,
            sponsored: None,
            keyboard: Vec::new(),
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

    /// How many have seen it (API 0.5, part 5): a channel's post — an eye and "1.2K" at its foot.
    pub fn views(mut self, views: u32) -> Self {
        self.views = Some(views);
        self
    }

    /// The forum topic it belongs to (API 0.5, part 5): its name in one of the seven avatar
    /// colors, over the text.
    pub fn topic(mut self, title: &str, color: u8) -> Self {
        self.topic = Some(TopicLabel {
            title: title.to_string(),
            color,
        });
        self
    }

    /// An advertisement (API 0.5, part 5): `label` ("Ad") at its foot in place of the time; the
    /// sponsor goes as its [`Message::author`].
    pub fn sponsored(mut self, label: &str) -> Self {
        self.sponsored = Some(label.to_string());
        self
    }

    /// A row of buttons under the message (API 0.5, part 5), sharing the row's width; a press
    /// comes as `UiEvent::KeyboardPressed((message, id))`. Build them with [`keyboard_button`].
    pub fn keyboard_row(mut self, buttons: Vec<KeyboardButton>) -> Self {
        if !buttons.is_empty() {
            self.keyboard.push(buttons);
        }
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
                Part::Voice(_) => "Voice message".to_string(),
                Part::Video(video) => match video.kind {
                    VideoKind::Video => "Video",
                    VideoKind::Gif => "GIF",
                    VideoKind::Round => "Video message",
                }
                .to_string(),
                Part::File(file) => file.name.clone(),
                Part::Link(link) => link.url.clone(),
                Part::Placeholder(placeholder) => placeholder.text.clone(),
                Part::Poll(poll) => poll
                    .question
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect(),
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
    /// A link: web ones open in the browser, others come as `UiEvent::LinkClicked` — every one
    /// with the conversation's [`ConversationSpec::own_links`]. A web link whose text isn't its
    /// address shows the address in a tooltip on hover.
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

/// A voice message (API 0.5, part 4): Flux plays it; build it with [`voice_spec`].
pub fn voice(spec: VoiceSpec) -> Part {
    Part::Voice(spec)
}

/// A video, a GIF, a round video message (API 0.5, part 4): Flux plays it; build it with
/// [`video_spec`].
pub fn video(spec: VideoSpec) -> Part {
    Part::Video(spec)
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

/// A poll (API 0.5, part 5): build it with [`poll_spec`]; a vote comes as
/// `UiEvent::PollVoted((message, part, options))`.
pub fn poll(spec: PollSpec) -> Part {
    Part::Poll(spec)
}

/// A poll: its question, the dim line under it ("Anonymous poll", "Quiz"), its options (build
/// them with [`poll_option`]) and the line under them ("12 votes"); not answered, one choice —
/// set the rest with the builder's methods.
pub fn poll_spec(
    question: Vec<RichSpan>,
    kind: &str,
    options: Vec<PollOption>,
    footer: &str,
) -> PollSpec {
    PollSpec {
        question,
        kind: kind.to_string(),
        options,
        multiple: false,
        answered: false,
        closed: false,
        footer: footer.to_string(),
        explanation: None,
    }
}

impl PollSpec {
    /// Several options may be chosen: checkboxes and Vote.
    pub fn multiple(mut self) -> Self {
        self.multiple = true;
        self
    }
    /// The results show: the user voted, or the poll is closed.
    pub fn answered(mut self) -> Self {
        self.answered = true;
        self
    }
    /// Closed: no voting (the results show).
    pub fn closed(mut self) -> Self {
        self.closed = true;
        self.answered = true;
        self
    }
    /// A quiz's explanation, behind 💡 once answered.
    pub fn explanation(mut self, spans: Vec<RichSpan>) -> Self {
        self.explanation = Some(spans);
        self
    }
}

/// An option of a poll: its text and its share of the votes (0–100).
pub fn poll_option(text: Vec<RichSpan>, percent: u8) -> PollOption {
    PollOption {
        text,
        percent: percent.min(100),
        chosen: false,
        correct: false,
    }
}

impl PollOption {
    /// The user chose it.
    pub fn chosen(mut self) -> Self {
        self.chosen = true;
        self
    }
    /// A quiz's right answer.
    pub fn correct(mut self) -> Self {
        self.correct = true;
        self
    }
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
        animation: None,
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
    /// An animation over the picture (API 0.5, part 4): an FXA file (`flux-media`'s frame
    /// format), played while on screen; `looped` — again and again, else once each time it comes
    /// into sight.
    pub fn animation(mut self, path: &str, looped: bool) -> Self {
        self.animation = Some(AnimationSpec {
            path: path.to_string(),
            looped,
        });
        self
    }
}

// --- Voice messages and videos (API 0.5, part 4) ---

/// A voice message of `duration` seconds: the OGG/Opus file once downloaded, the waveform
/// (values 0–31, any count; empty — an even line).
pub fn voice_spec(path: Option<&str>, duration: u32, waveform: Vec<u8>) -> VoiceSpec {
    VoiceSpec {
        path: path.map(str::to_string),
        duration,
        waveform,
        progress: None,
        unheard: false,
    }
}

impl VoiceSpec {
    /// Being downloaded: 0.0–1.0.
    pub fn progress(mut self, done: f32) -> Self {
        self.progress = Some(done);
        self
    }
    /// Not listened to yet: an accent dot; its first play comes as `UiEvent::MediaPlayed`.
    pub fn unheard(mut self) -> Self {
        self.unheard = true;
        self
    }
}

/// A video of `duration` seconds: its kind, the picture standing for it (its size is the
/// video's room), the MP4 file once downloaded.
pub fn video_spec(
    kind: VideoKind,
    poster: ImageSpec,
    path: Option<&str>,
    duration: u32,
) -> VideoSpec {
    VideoSpec {
        kind,
        poster,
        path: path.map(str::to_string),
        duration,
        autoplay: false,
        unheard: false,
    }
}

impl VideoSpec {
    /// A GIF, a round message: plays by itself without sound while on screen.
    pub fn autoplay(mut self) -> Self {
        self.autoplay = true;
        self
    }
    /// A round message not watched yet: an accent dot; its first play with sound comes as
    /// `UiEvent::MediaPlayed`.
    pub fn unheard(mut self) -> Self {
        self.unheard = true;
        self
    }
}

// --- The field's panel (API 0.5, part 4) ---

/// What a tab of the panel shows: its sections, for `query` (empty — the tab's own content).
pub fn panel_content(
    tab: PanelTab,
    query: &str,
    sections: Vec<PanelSection>,
    more: History,
) -> PanelContent {
    PanelContent {
        tab,
        query: query.to_string(),
        sections,
        more,
    }
}

/// A section of a panel's tab: a sticker set, "Recent"; `icon` — its picture in the tab's strip.
pub fn panel_section(
    key: &str,
    title: &str,
    icon: Option<ImageSpec>,
    items: Vec<PanelItem>,
) -> PanelSection {
    PanelSection {
        key: key.to_string(),
        title: title.to_string(),
        icon,
        items,
    }
}

/// An item of the panel: a sticker's picture (with its animation), a GIF's poster.
pub fn panel_item(key: &str, image: ImageSpec) -> PanelItem {
    PanelItem {
        key: key.to_string(),
        image,
        video: None,
        emoji: None,
    }
}

impl PanelItem {
    /// A GIF's MP4: plays looped without sound while on screen.
    pub fn video(mut self, path: &str) -> Self {
        self.video = Some(path.to_string());
        self
    }
    /// The emoji it stands for: its tooltip.
    pub fn emoji(mut self, emoji: &str) -> Self {
        self.emoji = Some(emoji.to_string());
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

// --- Buttons under a message (API 0.5, part 5) ---

/// A button under a message: `id` names it in `UiEvent::KeyboardPressed`; `kind` gives its mark
/// — none for an action the plugin answers, ↗ for a page, ↪ for a text put into the field.
pub fn keyboard_button(id: &str, text: &str, kind: KeyboardKind) -> KeyboardButton {
    KeyboardButton {
        id: id.to_string(),
        text: text.to_string(),
        kind,
    }
}

/// A bot's keyboard over the field: its rows of buttons (their `kind` is `Action`), shown at once
/// (`open`) or behind the field's ⌨ button.
pub fn reply_keyboard_spec(rows: Vec<Vec<KeyboardButton>>, open: bool) -> ReplyKeyboard {
    ReplyKeyboard {
        rows,
        placeholder: None,
        open,
    }
}

impl ReplyKeyboard {
    /// The field's placeholder while the keyboard is there ("Choose a city").
    pub fn placeholder(mut self, placeholder: &str) -> Self {
        self.placeholder = Some(placeholder.to_string());
        self
    }
}

// --- The pinned message's bar (API 0.5, part 5) ---

/// The bar of the pinned message `message`: its title ("Pinned message"), its line, which of the
/// `count` pinned messages it is (`position`, 0 — the newest).
pub fn pinned_spec(
    message: &str,
    title: &str,
    text: &str,
    position: u32,
    count: u32,
) -> PinnedSpec {
    PinnedSpec {
        message: message.to_string(),
        title: title.to_string(),
        text: text.to_string(),
        image: None,
        position,
        count,
        actions: Vec::new(),
    }
}

impl PinnedSpec {
    /// The small picture before the text: the message's photo, a video's poster.
    pub fn image(mut self, image: ImageSpec) -> Self {
        self.image = Some(image);
        self
    }
    /// An icon button at the bar's right end ("Unpin", ×); a press comes as
    /// `UiEvent::PinnedAction((message, id))`.
    pub fn action(mut self, id: &str, tooltip: &str, icon: &str) -> Self {
        self.actions.push(RowAction {
            id: id.to_string(),
            label: None,
            icon: Some(icon.to_string()),
            tooltip: Some(tooltip.to_string()),
            primary: false,
        });
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
        panel: Vec::new(),
        triggers: Vec::new(),
        send_options: Vec::new(),
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
    /// The field's panel (API 0.5, part 4): a ☺ button opens these tabs — Flux's emoji, the
    /// plugin's stickers and GIFs (their content comes as [`panel`] changes).
    pub fn panel(mut self, tabs: impl IntoIterator<Item = PanelTab>) -> Self {
        self.panel = tabs.into_iter().collect();
        self
    }
    /// The characters that ask for suggestions (API 0.5, part 5): "@" at a word's start, "/" at
    /// the field's start. Typing one comes as `UiEvent::Suggest((trigger, text))`; answer with
    /// [`suggestions`].
    pub fn triggers<'a>(mut self, triggers: impl IntoIterator<Item = &'a str>) -> Self {
        self.triggers = triggers.into_iter().map(str::to_string).collect();
        self
    }
    /// Another way to send (API 0.5, part 5), in the Send button's menu (a right click on it):
    /// "Send Without Sound"; a choice sends the field and comes as
    /// `UiEvent::ComposedWith((id, composed))`.
    pub fn send_option(mut self, id: &str, label: &str) -> Self {
        self.send_options.push(row_action(id, label, None));
        self
    }
}

impl Suggestion {
    /// An item of the field's suggestions (API 0.5, part 5): `key` names it, `title` shows it,
    /// `insert` replaces the trigger and the text typed after it ("@anna ", "/start ").
    pub fn new(key: &str, title: &str, insert: &str) -> Self {
        Suggestion {
            key: key.to_string(),
            avatar: None,
            title: title.to_string(),
            detail: None,
            insert: insert.to_string(),
        }
    }
    pub fn avatar(mut self, avatar: AvatarSpec) -> Self {
        self.avatar = Some(avatar);
        self
    }
    /// Dim after the title: "@anna", "Start the bot".
    pub fn detail(mut self, detail: &str) -> Self {
        self.detail = Some(detail.to_string());
        self
    }
}

/// A conversation's spec: reading only, no actions; add them with the builder's methods.
pub fn spec() -> ConversationSpec {
    ConversationSpec {
        composer: None,
        empty_text: None,
        message_actions: Vec::new(),
        field_button: None,
        selection_actions: Vec::new(),
        field_note: None,
        own_links: false,
        reactions: Vec::new(),
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
    /// A button in place of the field (API 0.5, part 5), where the user can't write: "Join",
    /// "Mute"; a press comes as `UiEvent::ComposerAction((id, composed))` with an empty
    /// `composed`.
    pub fn field_button(mut self, id: &str, label: &str) -> Self {
        self.field_button = Some(row_action(id, label, None));
        self
    }
    /// A line in place of the field where the user can't write (API 0.5, part 5): "You can't
    /// write in this chat"; shown when there is neither the field nor a [`Self::field_button`].
    pub fn field_note(mut self, text: &str) -> Self {
        self.field_note = Some(text.to_string());
        self
    }
    /// The plugin opens every link of the feed (API 0.5, part 5): web links and link previews
    /// come as `UiEvent::LinkClicked` too, so it can ask first or open its own pages
    /// ([`crate::system::open_url`] opens one in the browser). Without it, web links open in the
    /// browser at once.
    pub fn own_links(mut self) -> Self {
        self.own_links = true;
        self
    }
    /// An action on selected messages (API 0.5, part 5): "Forward", "Delete" — the first one with
    /// `primary` is the accent button. With one, the message menu's "Select" starts selecting;
    /// a choice comes as `UiEvent::SelectionAction((id, messages))`, in the feed's order.
    pub fn selection_action(mut self, id: &str, label: &str, primary: bool) -> Self {
        let mut action = row_action(id, label, None);
        action.primary = primary;
        self.selection_actions.push(action);
        self
    }
    /// The reactions the user may put (API 0.5, part 5), emoji in order: a ☺ on the message's
    /// hover bar opens them; a click on one of them under a message toggles the user's own. Both
    /// come as `UiEvent::ReactionToggled((message, emoji))`.
    pub fn reactions(mut self, emoji: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.reactions = emoji.into_iter().map(Into::into).collect();
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
        let post = Message::new("m3", 9, 0)
            .views(1_200)
            .topic("Design", 3)
            .sponsored("Ad")
            .keyboard_row(vec![keyboard_button("open", "Open", KeyboardKind::Link)])
            .keyboard_row(Vec::new());
        assert_eq!(post.views, Some(1_200));
        assert_eq!(post.topic.as_ref().map(|topic| topic.color), Some(3));
        assert_eq!(post.keyboard.len(), 1);
        let mine = Message::new("m2", 8, 0).outgoing(Delivery::Sending);
        assert_eq!(mine.quote("You").author, "You");
        assert!(matches!(remove(["m1"]), Change::Remove(ids) if ids == ["m1"]));
    }
}
