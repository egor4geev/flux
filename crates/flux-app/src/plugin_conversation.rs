//! A plugin's conversation (API 0.5, FluxGram part 1): the `conversation` element of a tool
//! window — a message feed with a field, as a chat. Its messages come as changes by id
//! (`ui.update-conversation`): put (add or replace, kept sorted by `order`), remove, the state of
//! the history above and below, the "New messages" line, a reveal, the field's quote and text.
//! A new view of the window keeps them (by the element's id): [`Conversation`] is an entity of its
//! own, [`crate::plugin_view::PluginView`] holds one per element and passes its events on.
//!
//! The feed is a gpui `list` of messages of any height, aligned to the bottom: only what is on
//! screen (and a screen around it) is laid out and drawn. Each change is spliced into the list by
//! the message's place, so a page of history put above leaves what the user sees where it is, and
//! a message put below the last one keeps the feed at the bottom if it was there. Scrolling within
//! about a screen of an end asks for more (`load-older`, `load-newer`, once per state of the
//! history); the messages on screen go to the plugin as `visible(first, last)`, at most every
//! [`VISIBLE_EVERY`] while scrolling and once more when it settles.
//!
//! A message: incoming on the left (in a group the author's colored name on the first of a run,
//! the avatar beside the last), outgoing on the right in the accent bubble; the time, "edited" and
//! the delivery marks at the bubble's foot; a reply's quote, "Forwarded from"; styled text with
//! links and spoilers, code blocks highlighted as the language's files are, quotes, pictures (an
//! album as a grid), stickers without a bubble, files, link previews, placeholders, notices in the
//! middle; reactions under it. Its menu holds the plugin's actions (also on a bar over the hovered
//! message) and Flux's own: Copy Text; Open, Show in Finder (Explorer), Save As… for a downloaded
//! picture or file. The field: ↵ sends, ⇧↵ starts a line; the quote being answered with ×; files
//! by the paperclip, by dropping, by pasting (a pasted picture is saved to a file first), a picture
//! file's chip with its thumbnail; a file handed to the conversation is a picture the plugin may
//! show (`plugin_images::grant`). Editing a message (`editing`): a bar over the field with the
//! original text, the field takes the text; ↵ sends `edited`, Esc or × `edit-cancelled`; ↑ in the
//! empty field asks for the last message to edit (`edit-last`). A quote or an edit the plugin puts
//! over the field gives it the keyboard, when the window is the active one.
//!
//! Nothing here asks for frames by itself: an idle conversation draws nothing.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use std::{cell::RefCell, rc::Rc};

use flux_plugin::api::events::UiEvent;
use flux_plugin::api::ui::{
    AvatarSpec, Composed, ConversationChange, ConversationSpec, Delivery, FileSpec, History,
    ImageShape, ImageSpec, LinkPreview, Message, MessageEdit, MessagePart, MessageQuote, RichSpan,
    RowAction,
};
use gpui::{
    Action, AnyElement, App, Bounds, ClickEvent, Context, DismissEvent, Entity, EventEmitter,
    ExternalPaths, FocusHandle, Focusable, FontStyle, FontWeight, HighlightStyle, Hsla,
    InteractiveText, KeyBinding, ListAlignment, ListOffset, ListState, MouseButton, MouseDownEvent,
    ObjectFit, Pixels, Point, Render, SharedString, StrikethroughStyle, StyledText, Subscription,
    Task, TextRun, UnderlineStyle, WeakEntity, Window, actions, div, font, img, list, prelude::*,
    px, relative,
};

use crate::context_menu::ContextMenu;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon, plugin_icon};
use crate::markdown::{self, Block, CodeBlock};
use crate::prompt_input::{PromptInput, PromptInputEvent};
use crate::theme::{self, Theme, UiColors};
use crate::ui;

actions!(
    plugin_conversation,
    [
        /// ↵ in the field: sends it (saves the edited message).
        Send,
        /// Esc in the field: leaves the editing of a message.
        CancelEdit,
    ]
);

/// A plugin's action of a message, from its menu.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = plugin_conversation, no_json)]
struct ChooseAction {
    message: String,
    action: String,
}

/// Copy Text of a message's menu.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = plugin_conversation, no_json)]
struct CopyText {
    message: String,
}

/// Open, Show in Finder, Save As… of a downloaded picture or file (by the part's index).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = plugin_conversation, no_json)]
struct OpenPart {
    message: String,
    part: usize,
}

#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = plugin_conversation, no_json)]
struct RevealPart {
    message: String,
    part: usize,
}

#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = plugin_conversation, no_json)]
struct SavePartAs {
    message: String,
    part: usize,
}

const FIELD_CONTEXT: &str = "PluginConversationField";

pub fn init(cx: &mut App) {
    crate::keymap::bind_keys(
        cx,
        [
            KeyBinding::new("enter", Send, Some(FIELD_CONTEXT)),
            KeyBinding::new("escape", CancelEdit, Some(FIELD_CONTEXT)),
        ],
    );
}

/// The feed's column: centered, no wider than this.
const COLUMN_WIDTH: f32 = 760.;
const COLUMN_PADDING: f32 = 12.;
/// A bubble is no wider than this, and than [`BUBBLE_SHARE`] of the column.
const BUBBLE_WIDTH: f32 = 600.;
const BUBBLE_SHARE: f32 = 0.86;
const BUBBLE_RADIUS: f32 = 12.;
/// The corner of a bubble toward its author's side, on the last of a run.
const TAIL_RADIUS: f32 = 4.;
const BUBBLE_PADDING_X: f32 = 12.;
const BUBBLE_PADDING_Y: f32 = 7.;
const AVATAR_SIZE: f32 = 34.;
const AVATAR_GAP: f32 = 10.;
/// Between messages of different authors, and within a run.
const MESSAGE_GAP: f32 = 10.;
const RUN_GAP: f32 = 3.;
const PHOTO_WIDTH: f32 = 360.;
const PHOTO_HEIGHT: f32 = 360.;
const ALBUM_GAP: f32 = 3.;
const STICKER_SIZE: f32 = 160.;
const PICTURE_RADIUS: f32 = 8.;
const FILE_ICON: f32 = 38.;
/// The thumbnail of a picture file's chip in the field.
const CHIP_THUMBNAIL: f32 = 18.;
/// The files the field shows as pictures: a chip with a thumbnail.
const PICTURE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "gif"];
/// About a screen is laid out above and below what is seen: scrolling into it asks for more.
const OVERDRAW: f32 = 1000.;
/// `visible` goes out at most this often while scrolling.
const VISIBLE_EVERY: Duration = Duration::from_millis(200);
/// `draft-changed` goes out this long after the typing stops.
const DRAFT_DELAY: Duration = Duration::from_millis(300);
/// A revealed message stays marked this long.
const REVEAL_MARK: Duration = Duration::from_millis(1400);
const TEXT_SIZE: f32 = 13.;
const LINE_HEIGHT: f32 = 19.;

/// What the conversation tells its window: an event for the plugin.
pub enum ConversationEvent {
    Ui(UiEvent),
}

/// The open menu of a message.
struct Menu {
    menu: Entity<ContextMenu>,
    position: Point<Pixels>,
    _subscriptions: [Subscription; 2],
}

/// Which messages were on screen in the last frame (indices in the feed), as drawn.
#[derive(Default)]
struct Sight {
    /// The feed's area.
    viewport: Option<Bounds<Pixels>>,
    /// Each drawn message's index and bounds (with the screen around what is seen).
    drawn: Vec<(usize, Bounds<Pixels>)>,
}

/// A code block of a message, highlighted in the background.
struct Code {
    text: String,
    blocks: Vec<Block>,
    _highlight: Task<()>,
}

pub struct Conversation {
    plugin: SharedString,
    spec: ConversationSpec,
    /// The messages by id.
    messages: HashMap<String, Message>,
    /// The ids in the feed's order (by `order`, then by id).
    order: Vec<String>,
    list: ListState,
    older: History,
    newer: History,
    /// `load-older` (`load-newer`) went out in this state of the history.
    asked_older: bool,
    asked_newer: bool,
    unread_from: Option<String>,
    /// The feed had messages: the first fill opened at `unread-from`.
    filled: bool,
    /// The quote over the field.
    reply_to: Option<MessageQuote>,
    /// The message the field edits (`editing`): its bar over the field, ↵ saves it.
    editing: Option<MessageEdit>,
    /// A quote or an edit came over the field: the next frame gives it the keyboard (when the
    /// window is the active one).
    take_keyboard: bool,
    input: Entity<PromptInput>,
    attachments: Vec<PathBuf>,
    /// The text the plugin put in the field last (`draft`): not echoed as `draft-changed`.
    plugin_draft: Option<String>,
    draft_timer: Option<Task<()>>,
    sight: Rc<RefCell<Sight>>,
    /// The last `visible` sent, when, and the one waiting for its turn.
    visible_sent: Option<(String, String)>,
    visible_at: Option<Instant>,
    visible_timer: Option<Task<()>>,
    /// The newest message is on screen: no jump-to-bottom button.
    at_bottom: bool,
    revealed: Option<(String, Instant)>,
    /// Spoilers the user opened: (message, part).
    open_spoilers: HashSet<(String, usize)>,
    code: HashMap<(String, usize), Code>,
    menu: Option<Menu>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ConversationEvent> for Conversation {}

impl Focusable for Conversation {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Conversation {
    pub fn new(plugin: SharedString, spec: ConversationSpec, cx: &mut Context<Self>) -> Self {
        let placeholder = spec
            .composer
            .as_ref()
            .map(|composer| composer.placeholder.clone())
            .unwrap_or_default();
        let input = cx.new(|cx| PromptInput::new(placeholder, cx).max_rows(8).takes_files());
        let subscriptions = vec![cx.subscribe(&input, Self::on_input)];
        let list = ListState::new(0, ListAlignment::Bottom, px(OVERDRAW));
        Self {
            plugin,
            spec,
            messages: HashMap::new(),
            order: Vec::new(),
            list,
            older: History::Complete,
            newer: History::Complete,
            asked_older: false,
            asked_newer: false,
            unread_from: None,
            filled: false,
            reply_to: None,
            editing: None,
            take_keyboard: false,
            input,
            attachments: Vec::new(),
            plugin_draft: None,
            draft_timer: None,
            sight: Rc::default(),
            visible_sent: None,
            visible_at: None,
            visible_timer: None,
            at_bottom: true,
            revealed: None,
            open_spoilers: HashSet::new(),
            code: HashMap::new(),
            menu: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// The element's spec in the window's new view: the field, the actions.
    pub fn set_spec(&mut self, spec: &ConversationSpec, cx: &mut Context<Self>) {
        if self.spec == *spec {
            return;
        }
        let placeholder = spec
            .composer
            .as_ref()
            .map(|composer| composer.placeholder.clone())
            .unwrap_or_default();
        self.input
            .update(cx, |input, _| input.set_placeholder(placeholder));
        self.spec = spec.clone();
        cx.notify();
    }

    fn emit(&self, event: UiEvent, cx: &mut Context<Self>) {
        cx.emit(ConversationEvent::Ui(event));
    }

    // --- Changes ---

    /// Applies the plugin's changes in order.
    pub fn apply(&mut self, changes: Vec<ConversationChange>, cx: &mut Context<Self>) {
        for change in changes {
            match change {
                ConversationChange::Put(messages) => {
                    for message in messages {
                        self.put(message);
                    }
                }
                ConversationChange::Remove(ids) => {
                    for id in ids {
                        self.remove(&id);
                    }
                }
                ConversationChange::Clear => {
                    self.messages.clear();
                    self.order.clear();
                    self.code.clear();
                    self.open_spoilers.clear();
                    self.list.reset(0);
                    self.filled = false;
                    self.visible_sent = None;
                }
                ConversationChange::Older(history) => {
                    if self.older != history {
                        self.older = history;
                        self.asked_older = false;
                    }
                }
                ConversationChange::Newer(history) => {
                    if self.newer != history {
                        self.newer = history;
                        self.asked_newer = false;
                    }
                }
                ConversationChange::UnreadFrom(id) => {
                    let before = self.unread_from.take();
                    self.unread_from = id;
                    // The line moves: the messages it leaves and comes to are measured again.
                    for id in before.iter().chain(self.unread_from.clone().iter()) {
                        if let Some(ix) = self.position(id) {
                            self.remeasure(ix);
                        }
                    }
                }
                ConversationChange::Reveal(id) => self.reveal(&id),
                ConversationChange::ReplyTo(quote) => {
                    // Replying leaves an edit, as the messengers do.
                    if quote.is_some() {
                        if self.editing.is_some() {
                            self.leave_editing(cx);
                            self.emit(UiEvent::EditCancelled, cx);
                        }
                        self.take_keyboard = true;
                    }
                    self.reply_to = quote;
                }
                ConversationChange::Draft(text) => {
                    self.plugin_draft = Some(text.clone());
                    self.input.update(cx, |input, cx| input.set_text(&text, cx));
                }
                ConversationChange::Editing(Some(edit)) => {
                    // Editing drops the quote being answered.
                    if self.reply_to.take().is_some() {
                        self.emit(UiEvent::ReplyCancelled, cx);
                    }
                    self.draft_timer = None;
                    self.input
                        .update(cx, |input, cx| input.set_text(&edit.text, cx));
                    self.editing = Some(edit);
                    self.take_keyboard = true;
                }
                // Flux has left the mode itself on ↵, Esc or ×: a late `None` finds nothing to do
                // (and leaves the text the user may be typing).
                ConversationChange::Editing(None) => {
                    if self.editing.is_some() {
                        self.leave_editing(cx);
                    }
                }
            }
        }
        // The first fill opens at the "New messages" line.
        if !self.filled && !self.order.is_empty() {
            self.filled = true;
            if let Some(ix) = self.unread_from.as_ref().and_then(|id| self.position(id)) {
                self.list.scroll_to(ListOffset {
                    item_ix: ix,
                    offset_in_item: px(0.),
                });
            }
        }
        cx.notify();
    }

    /// The index of a message in the feed.
    fn position(&self, id: &str) -> Option<usize> {
        let message = self.messages.get(id)?;
        let key = (message.order, id);
        let ix = self
            .order
            .partition_point(|other| (self.messages[other].order, other.as_str()) < key);
        (self.order.get(ix).map(String::as_str) == Some(id)).then_some(ix)
    }

    /// Adds a message, or replaces the one with its id.
    fn put(&mut self, message: Message) {
        let id = message.id.clone();
        if let Some(old) = self.messages.get(&id) {
            if *old == message {
                return;
            }
            if old.order == message.order {
                let ix = self.position(&id);
                self.code.retain(|(owner, _), _| *owner != id);
                self.messages.insert(id, message);
                if let Some(ix) = ix {
                    self.remeasure(ix);
                }
                return;
            }
            self.remove(&id);
        }
        let key = (message.order, id.clone());
        let ix = self
            .order
            .partition_point(|other| (self.messages[other].order, other.clone()) < key);
        self.messages.insert(id.clone(), message);
        self.order.insert(ix, id);
        // gpui shifts the scroll by the items put above it: what is seen stays.
        self.list.splice(ix..ix, 1);
        // The neighbors' runs (the name, the avatar, the day's line) may change.
        if ix > 0 {
            self.remeasure(ix - 1);
        }
        if ix + 1 < self.order.len() {
            self.remeasure(ix + 1);
        }
    }

    fn remove(&mut self, id: &str) {
        let Some(ix) = self.position(id) else {
            return;
        };
        self.order.remove(ix);
        self.messages.remove(id);
        self.code.retain(|(owner, _), _| owner != id);
        self.list.splice(ix..ix + 1, 0);
        if ix > 0 {
            self.remeasure(ix - 1);
        }
        if ix < self.order.len() {
            self.remeasure(ix);
        }
    }

    /// Lays a message out again; the scroll stays where it is (gpui would move a splice at the
    /// top of the screen to the item's start).
    fn remeasure(&self, ix: usize) {
        let top = self.list.logical_scroll_top();
        let at_bottom = top.item_ix >= self.list.item_count();
        self.list.splice(ix..ix + 1, 1);
        if !at_bottom {
            self.list.scroll_to(top);
        }
    }

    /// Scrolls to a message and marks it for a moment.
    fn reveal(&mut self, id: &str) {
        if let Some(ix) = self.position(id) {
            self.list.scroll_to_reveal_item(ix);
            self.revealed = Some((id.to_string(), Instant::now()));
        }
    }

    fn scroll_to_bottom(&mut self, cx: &mut Context<Self>) {
        self.list.scroll_to(ListOffset {
            item_ix: self.list.item_count(),
            offset_in_item: px(0.),
        });
        cx.notify();
    }

    // --- What is on screen ---

    /// After the feed is laid out: the messages on screen go to the plugin (coalesced), and an
    /// end in sight asks for more history.
    fn observe_sight(&mut self, cx: &mut Context<Self>) {
        let (first, last, top, bottom) = {
            let sight = self.sight.borrow();
            let Some(viewport) = sight.viewport else {
                return;
            };
            let seen = sight.drawn.iter().filter(|(_, bounds)| {
                bounds.bottom() > viewport.top() && bounds.top() < viewport.bottom()
            });
            let first = seen.clone().map(|(ix, _)| *ix).min();
            let last = seen.map(|(ix, _)| *ix).max();
            let top = sight.drawn.iter().map(|(ix, _)| *ix).min();
            let bottom = sight.drawn.iter().map(|(ix, _)| *ix).max();
            (first, last, top, bottom)
        };
        let count = self.order.len();
        if count == 0 {
            return;
        }
        // An end within the drawn screen around what is seen.
        if top == Some(0) && self.older == History::More && !self.asked_older {
            self.asked_older = true;
            self.emit(UiEvent::LoadOlder, cx);
        }
        if bottom == Some(count - 1) && self.newer == History::More && !self.asked_newer {
            self.asked_newer = true;
            self.emit(UiEvent::LoadNewer, cx);
        }
        let at_bottom = last == Some(count - 1) && self.newer != History::More;
        if at_bottom != self.at_bottom {
            self.at_bottom = at_bottom;
            cx.notify();
        }
        let (Some(first), Some(last)) = (first, last) else {
            return;
        };
        let pair = (self.order[first].clone(), self.order[last].clone());
        if self.visible_sent.as_ref() == Some(&pair) {
            self.visible_timer = None;
            return;
        }
        let since = self.visible_at.map(|at| at.elapsed());
        if since.is_none_or(|since| since >= VISIBLE_EVERY) {
            self.send_visible(pair, cx);
        } else if self.visible_timer.is_none() {
            // The latest pair goes out when its turn comes (and once the scrolling settles).
            let wait = VISIBLE_EVERY - since.unwrap_or_default();
            self.visible_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                this.update(cx, |this, cx| {
                    this.visible_timer = None;
                    this.observe_sight(cx);
                })
                .ok();
            }));
        }
    }

    fn send_visible(&mut self, pair: (String, String), cx: &mut Context<Self>) {
        self.visible_at = Some(Instant::now());
        self.visible_sent = Some(pair.clone());
        self.visible_timer = None;
        self.emit(UiEvent::Visible(pair), cx);
    }

    // --- The field ---

    fn on_input(
        &mut self,
        _: Entity<PromptInput>,
        event: &PromptInputEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            PromptInputEvent::Changed => {
                let text = self.input.read(cx).text();
                // An edited message's text isn't the new message's draft.
                if self.editing.is_some() || self.plugin_draft.as_deref() == Some(text.as_str()) {
                    return;
                }
                self.plugin_draft = None;
                self.draft_timer = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(DRAFT_DELAY).await;
                    this.update(cx, |this, cx| {
                        let text = this.input.read(cx).text();
                        this.emit(UiEvent::DraftChanged(text), cx);
                    })
                    .ok();
                }));
            }
            PromptInputEvent::ImagePasted(image) if self.takes_files() => {
                let image = image.clone();
                let saved = cx
                    .background_executor()
                    .spawn(async move { save_pasted(&image) });
                cx.spawn(async move |this, cx| {
                    if let Some(path) = saved.await {
                        this.update(cx, |this, cx| this.attach(vec![path], cx)).ok();
                    }
                })
                .detach();
            }
            PromptInputEvent::FilesPasted(paths) if self.takes_files() => {
                self.attach(paths.clone(), cx)
            }
            // ↑ in the empty field, as in the messengers: the plugin may answer with the user's
            // last message to edit.
            PromptInputEvent::UpWhenEmpty if self.editing.is_none() => {
                self.emit(UiEvent::EditLast, cx)
            }
            _ => {}
        }
    }

    /// The field takes files: its spec says so, and it isn't editing a message.
    fn takes_files(&self) -> bool {
        self.editing.is_none()
            && self
                .spec
                .composer
                .as_ref()
                .is_some_and(|composer| composer.attachments)
    }

    /// Files handed to the conversation: chips in the field, and pictures the plugin may show —
    /// the message it sends with them shows the photo from wherever it was.
    fn attach(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for path in paths {
            if path.is_file() && !self.attachments.contains(&path) {
                crate::plugin_images::grant(&self.plugin, &path, cx);
                self.attachments.push(path);
            }
        }
        cx.notify();
    }

    /// Leaves the editing of a message: the bar goes, the field is emptied (not echoed as a
    /// draft).
    fn leave_editing(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        self.draft_timer = None;
        self.plugin_draft = Some(String::new());
        self.input.update(cx, |input, cx| input.set_text("", cx));
        cx.notify();
    }

    /// Esc in the field: leaves the editing (`edit-cancelled`); otherwise Esc does what it does
    /// around the field.
    fn cancel_edit(&mut self, _: &CancelEdit, _: &mut Window, cx: &mut Context<Self>) {
        if self.editing.is_none() {
            cx.propagate();
            return;
        }
        self.leave_editing(cx);
        self.emit(UiEvent::EditCancelled, cx);
    }

    fn choose_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(tr("Attach").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                this.update(cx, |this, cx| this.attach(paths, cx)).ok();
            }
        })
        .detach();
    }

    /// What is in the field now.
    fn composed(&self, cx: &App) -> Composed {
        Composed {
            text: self.input.read(cx).text(),
            files: self
                .attachments
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            reply_to: self
                .reply_to
                .as_ref()
                .and_then(|quote| quote.message.clone()),
        }
    }

    fn send(&mut self, _: &Send, _: &mut Window, cx: &mut Context<Self>) {
        // Editing: the message's new text goes out, and the field is back to a new message.
        if let Some(edit) = self.editing.clone() {
            let text = self.input.read(cx).text();
            self.leave_editing(cx);
            self.emit(UiEvent::Edited((edit.message, text)), cx);
            return;
        }
        let composed = self.composed(cx);
        if composed.text.trim().is_empty() && composed.files.is_empty() {
            return;
        }
        self.emit(UiEvent::Composed(composed), cx);
        self.attachments.clear();
        self.reply_to = None;
        self.draft_timer = None;
        self.plugin_draft = Some(String::new());
        self.input.update(cx, |input, cx| input.set_text("", cx));
        self.scroll_to_bottom(cx);
    }

    // --- Clicks ---

    fn click_quote(&mut self, message: &str, quoted: &str, cx: &mut Context<Self>) {
        if self.messages.contains_key(quoted) {
            self.reveal(quoted);
            cx.notify();
        } else {
            self.emit(
                UiEvent::QuoteClicked((message.to_string(), quoted.to_string())),
                cx,
            );
        }
    }

    fn click_link(&mut self, message: &str, link: &str, cx: &mut Context<Self>) {
        let web = ["http://", "https://", "mailto:"]
            .iter()
            .any(|scheme| link.to_ascii_lowercase().starts_with(scheme));
        if web {
            crate::plugin_links::open_url(link, cx);
        } else {
            self.emit(
                UiEvent::LinkClicked((message.to_string(), link.to_string())),
                cx,
            );
        }
    }

    /// A click on a picture: Flux's viewer with the conversation's pictures, or the plugin's
    /// download when it has no file yet.
    fn click_picture(
        &mut self,
        message: &str,
        part: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut pictures = Vec::new();
        let mut index = None;
        for id in &self.order {
            for (ix, spec) in self.messages[id].parts.iter().enumerate() {
                let (MessagePart::Image(spec) | MessagePart::Sticker(spec)) = spec else {
                    continue;
                };
                let Some(path) = spec.full.as_ref().or(spec.path.as_ref()) else {
                    continue;
                };
                let path = PathBuf::from(path);
                if !crate::plugin_images::allowed(&self.plugin, &path, cx) {
                    continue;
                }
                if id == message && ix == part {
                    index = Some(pictures.len());
                }
                pictures.push(crate::image_view::ImageSource::File(path));
            }
        }
        match index {
            Some(index) => crate::image_popup::show(pictures, index, window, cx),
            None => self.emit(UiEvent::PartClicked((message.to_string(), part as u32)), cx),
        }
    }

    fn click_file(&mut self, message: &str, part: usize, cx: &mut Context<Self>) {
        match self.part_path(message, part) {
            Some(path) => open_with_system(&path, cx),
            None => self.emit(UiEvent::PartClicked((message.to_string(), part as u32)), cx),
        }
    }

    /// The file of a downloaded picture or file part.
    fn part_path(&self, message: &str, part: usize) -> Option<PathBuf> {
        let path = match self.messages.get(message)?.parts.get(part)? {
            MessagePart::Image(spec) | MessagePart::Sticker(spec) => {
                spec.full.as_ref().or(spec.path.as_ref())
            }
            MessagePart::File(file) => file.path.as_ref(),
            _ => None,
        }?;
        let path = PathBuf::from(path);
        path.exists().then_some(path)
    }

    // --- The menu ---

    fn open_menu(
        &mut self,
        message: String,
        part: Option<usize>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(found) = self.messages.get(&message) else {
            return;
        };
        let text = message_text(found);
        let part = part
            .filter(|part| self.part_path(&message, *part).is_some())
            .or_else(|| {
                (0..found.parts.len()).find(|part| self.part_path(&message, *part).is_some())
            });
        let actions = self.spec.message_actions.clone();
        window.focus(&self.focus_handle);
        let menu = cx.new(|cx| {
            let mut menu = ContextMenu::new(window, cx);
            for action in &actions {
                menu = menu.entry(
                    action.label.clone().unwrap_or_else(|| action.id.clone()),
                    ChooseAction {
                        message: message.clone(),
                        action: action.id.clone(),
                    },
                );
            }
            menu = menu.separator().entry_if(
                !text.is_empty(),
                tr("Copy Text"),
                CopyText {
                    message: message.clone(),
                },
            );
            if let Some(part) = part {
                menu = menu
                    .separator()
                    .entry(
                        tr("Open"),
                        OpenPart {
                            message: message.clone(),
                            part,
                        },
                    )
                    .entry(
                        if cfg!(target_os = "windows") {
                            tr("Show in Explorer")
                        } else {
                            tr("Show in Finder")
                        },
                        RevealPart {
                            message: message.clone(),
                            part,
                        },
                    )
                    .entry(
                        tr("Save As…"),
                        SavePartAs {
                            message: message.clone(),
                            part,
                        },
                    );
            }
            menu
        });
        let focus = menu.focus_handle(cx);
        let subscriptions = [
            cx.subscribe_in(&menu, window, |this, menu, _: &DismissEvent, _, cx| {
                this.close_menu(menu, cx)
            }),
            cx.on_focus_out(&focus, window, {
                let menu = menu.clone();
                move |this, _, _, cx| this.close_menu(&menu, cx)
            }),
        ];
        window.focus(&focus);
        self.menu = Some(Menu {
            menu,
            position,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    fn close_menu(&mut self, menu: &Entity<ContextMenu>, cx: &mut Context<Self>) {
        if self.menu.as_ref().is_some_and(|open| open.menu == *menu) {
            self.menu = None;
            cx.notify();
        }
    }

    fn choose_action(&mut self, action: &ChooseAction, _: &mut Window, cx: &mut Context<Self>) {
        self.emit(
            UiEvent::MessageAction((action.message.clone(), action.action.clone())),
            cx,
        );
    }

    fn copy_text(&mut self, action: &CopyText, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(message) = self.messages.get(&action.message) {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(message_text(message)));
        }
    }

    fn open_part(&mut self, action: &OpenPart, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.part_path(&action.message, action.part) {
            open_with_system(&path, cx);
        }
    }

    fn reveal_part(&mut self, action: &RevealPart, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.part_path(&action.message, action.part) {
            reveal_path(&path, cx);
        }
    }

    fn save_part_as(&mut self, action: &SavePartAs, window: &mut Window, cx: &mut Context<Self>) {
        let Some(source) = self.part_path(&action.message, action.part) else {
            return;
        };
        let name = match self.messages[&action.message].parts.get(action.part) {
            Some(MessagePart::File(file)) => file.name.clone(),
            _ => source
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        let directory = dirs_downloads();
        let target = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn_in(window, async move |_, cx| {
            if let Ok(Ok(Some(target))) = target.await {
                let copied = cx
                    .background_executor()
                    .spawn(async move { std::fs::copy(&source, &target).map(|_| ()) })
                    .await;
                if let Err(error) = copied {
                    log::warn!("couldn't save a conversation's file: {error}");
                }
            }
        })
        .detach();
    }

    // --- Drawing ---

    /// The code block of a message part, highlighted in the background (plain until then).
    fn code_blocks(
        &mut self,
        message: &str,
        part: usize,
        code: &flux_plugin::api::ui::CodeBlock,
        cx: &mut Context<Self>,
    ) -> Vec<Block> {
        let key = (message.to_string(), part);
        if let Some(known) = self.code.get(&key)
            && known.text == code.text
        {
            return known.blocks.clone();
        }
        let block = CodeBlock {
            language: code
                .language
                .clone()
                .map(|language| language.to_lowercase()),
            text: code.text.trim_end_matches('\n').to_string(),
            highlights: Vec::new(),
        };
        let blocks = vec![Block::Code(block.clone())];
        let scopes = markdown::scopes(cx);
        let task_key = key.clone();
        let text = code.text.clone();
        let highlight = cx.spawn(async move |this, cx| {
            let highlighted = cx
                .background_executor()
                .spawn(async move {
                    let mut block = block;
                    markdown::highlight_code(&mut block, None, &scopes);
                    block
                })
                .await;
            this.update(cx, |this, cx| {
                if let Some(code) = this.code.get_mut(&task_key)
                    && code.text == text
                {
                    code.blocks = vec![Block::Code(highlighted)];
                    if let Some(ix) = this.position(&task_key.0) {
                        this.remeasure(ix);
                    }
                    cx.notify();
                }
            })
            .ok();
        });
        self.code.insert(
            key,
            Code {
                text: code.text.clone(),
                blocks: blocks.clone(),
                _highlight: highlight,
            },
        );
        blocks
    }

    fn render_item(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(id) = self.order.get(ix).cloned() else {
            return div().into_any_element();
        };
        // Code blocks first: they need `self` mutably.
        let codes: Vec<(usize, Vec<Block>)> = self.messages[&id]
            .parts
            .clone()
            .iter()
            .enumerate()
            .filter_map(|(part, kind)| match kind {
                MessagePart::Code(code) => Some((part, self.code_blocks(&id, part, code, cx))),
                _ => None,
            })
            .collect();
        let ui = Theme::ui(cx);
        let message = &self.messages[&id];
        let previous = ix
            .checked_sub(1)
            .and_then(|ix| self.order.get(ix))
            .map(|id| &self.messages[id]);
        let next = self.order.get(ix + 1).map(|id| &self.messages[id]);
        // The feed's width as of the last frame (the list's state is borrowed while it lays out).
        let width = self
            .sight
            .borrow()
            .viewport
            .map_or(0., |viewport| f32::from(viewport.size.width));
        let column = if width > 0. {
            (width.min(COLUMN_WIDTH) - 2. * COLUMN_PADDING).max(120.)
        } else {
            COLUMN_WIDTH - 2. * COLUMN_PADDING
        };
        let now = local_seconds(unix_now());
        let local = local_seconds(message.date);
        let new_day = previous.is_none_or(|previous| {
            local_seconds(previous.date).div_euclid(86_400) != local.div_euclid(86_400)
        });
        let unread_line = self.unread_from.as_deref() == Some(id.as_str());
        let first_of_run = new_day || unread_line || previous.is_none_or(|p| !same_run(p, message));
        let last_of_run = next.is_none_or(|next| {
            !same_run(message, next)
                || local_seconds(next.date).div_euclid(86_400) != local.div_euclid(86_400)
                || self.unread_from.as_deref() == Some(next.id.as_str())
        });
        let marked = self
            .revealed
            .as_ref()
            .filter(|(revealed, at)| *revealed == id && at.elapsed() < REVEAL_MARK)
            .map(|(_, at)| at.elapsed().as_secs_f32() / REVEAL_MARK.as_secs_f32());
        if marked.is_some() {
            window.request_animation_frame();
        }
        let look = Look {
            ui,
            column,
            first_of_run,
            last_of_run,
            marked,
        };
        let body = if let Some(MessagePart::Notice(spans)) = message.parts.first() {
            render_notice(spans, ui)
        } else {
            self.render_message(message, &codes, &look, window, cx)
        };
        let sight = self.sight.clone();
        div()
            .id(SharedString::from(format!("fg-item-{id}")))
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(if first_of_run { MESSAGE_GAP } else { RUN_GAP }))
            .when(ix + 1 == self.order.len(), |item| item.pb(px(MESSAGE_GAP)))
            .when(new_day, |item| item.child(day_divider(local, now, ui)))
            .when(unread_line, |item| item.child(unread_divider(column, ui)))
            .child(
                div()
                    .w(px(column))
                    .on_children_prepainted(move |children, _, _| {
                        if let Some(bounds) = children.first() {
                            sight.borrow_mut().drawn.push((ix, *bounds));
                        }
                    })
                    .child(body),
            )
            .into_any_element()
    }

    fn render_message(
        &self,
        message: &Message,
        codes: &[(usize, Vec<Block>)],
        look: &Look,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = look.ui;
        let id = message.id.clone();
        let author_avatar = (!message.outgoing).then(|| {
            message
                .author
                .as_ref()
                .and_then(|author| author.avatar.clone())
        });
        let has_avatar_column = author_avatar.as_ref().is_some_and(Option::is_some);
        let room = look.column
            - if has_avatar_column {
                AVATAR_SIZE + AVATAR_GAP
            } else {
                0.
            };
        let bubble_width = (room * BUBBLE_SHARE).clamp(160., BUBBLE_WIDTH);
        let inner = bubble_width - 2. * BUBBLE_PADDING_X;
        let sticker_only = message.parts.len() == 1
            && matches!(message.parts[0], MessagePart::Sticker(_))
            && message.reply.is_none();
        let mut content: Vec<AnyElement> = Vec::new();
        // The author's name on the first of a run, in a group.
        if !message.outgoing
            && look.first_of_run
            && !sticker_only
            && let Some(author) = &message.author
            && author.avatar.is_some()
        {
            content.push(
                div()
                    .text_size(px(theme::TEXT_SM))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::avatar_color(author.color))
                    .truncate()
                    .child(author.name.clone())
                    .into_any_element(),
            );
        }
        if let Some(forwarded) = &message.forwarded {
            content.push(
                div()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.accent_text)
                    .truncate()
                    .child(trf("Forwarded from {0}", &[forwarded]))
                    .into_any_element(),
            );
        }
        if let Some(quote) = &message.reply {
            content.push(self.render_quote(&id, quote, ui, cx));
        }
        let mut parts = message.parts.iter().enumerate().peekable();
        while let Some((index, part)) = parts.next() {
            let element = match part {
                MessagePart::Text(spans) => self.render_rich(&id, index, spans, ui, cx),
                MessagePart::Quote(spans) => div()
                    .pl(px(8.))
                    .border_l_2()
                    .border_color(ui.accent)
                    .child(self.render_rich(&id, index, spans, ui, cx))
                    .into_any_element(),
                MessagePart::Code(_) => {
                    let blocks = codes
                        .iter()
                        .find(|(part, _)| *part == index)
                        .map(|(_, blocks)| blocks.clone())
                        .unwrap_or_default();
                    div()
                        .min_w_0()
                        .text_size(px(TEXT_SIZE))
                        .child(markdown::render(&blocks, ui.foreground, Theme::get(cx)))
                        .into_any_element()
                }
                MessagePart::Image(spec) => {
                    // Pictures in a row make an album.
                    let mut album = vec![(index, spec.clone())];
                    while let Some((next, MessagePart::Image(spec))) = parts.peek() {
                        album.push((*next, spec.clone()));
                        parts.next();
                    }
                    self.render_album(&id, &album, inner, window, cx)
                }
                MessagePart::Sticker(spec) => {
                    let size = fit(spec.width, spec.height, STICKER_SIZE, STICKER_SIZE);
                    self.render_picture_part(&id, index, spec, size, window, cx)
                }
                MessagePart::File(file) => self.render_file(&id, index, file, ui, cx),
                MessagePart::Link(preview) => {
                    self.render_link_preview(&id, index, preview, inner, window, cx)
                }
                MessagePart::Placeholder(placeholder) => {
                    let id = id.clone();
                    div()
                        .id(("fg-placeholder", index))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_color(ui.text_muted)
                        .cursor_pointer()
                        .children(
                            placeholder
                                .icon
                                .as_deref()
                                .and_then(|name| plugin_icon(&self.plugin, name, &ui))
                                .map(|icon| icon.render(ui.text_muted)),
                        )
                        .child(placeholder.text.clone())
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.emit(UiEvent::PartClicked((id.clone(), index as u32)), cx)
                        }))
                        .into_any_element()
                }
                MessagePart::Notice(spans) => render_notice(spans, ui),
            };
            content.push(element);
        }
        if !message.reactions.is_empty() {
            content.push(render_reactions(message, ui));
        }
        let meta = render_meta(message, sticker_only, ui);
        let tail = look.last_of_run;
        let corner = |side_is_tail: bool| {
            if side_is_tail && tail {
                px(TAIL_RADIUS)
            } else {
                px(BUBBLE_RADIUS)
            }
        };
        let (background, border) = if message.outgoing {
            (
                UiColors::tint(ui.accent, 0.2),
                UiColors::tint(ui.accent, 0.22),
            )
        } else {
            (
                UiColors::tint(ui.foreground, 0.05),
                UiColors::tint(ui.foreground, 0.063),
            )
        };
        let menu_id = id.clone();
        let bubble = if sticker_only {
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .when(message.outgoing, |bubble| bubble.items_end())
                .children(content)
                .child(meta)
        } else {
            div()
                // A definite width — the content's own, up to the bubble's — so wrapped text is
                // laid out at the width it is drawn at.
                .w(px(natural_width(message, inner, ui, window)
                    + 2. * BUBBLE_PADDING_X
                    + 2.))
                .min_w_0()
                .px(px(BUBBLE_PADDING_X))
                .pt(px(BUBBLE_PADDING_Y))
                .pb(px(BUBBLE_PADDING_Y - 1.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .bg(background)
                .border_1()
                .border_color(border)
                .rounded_tl(px(BUBBLE_RADIUS))
                .rounded_tr(px(BUBBLE_RADIUS))
                .rounded_bl(corner(!message.outgoing))
                .rounded_br(corner(message.outgoing))
                .children(content)
                .child(meta)
        }
        .when_some(look.marked, |bubble, done| {
            bubble.shadow(vec![gpui::BoxShadow {
                color: UiColors::tint(ui.accent, 0.6 * (1. - done)),
                offset: gpui::point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(3.),
            }])
        })
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                this.open_menu(menu_id.clone(), None, event.position, window, cx)
            }),
        );
        let hover_bar = self.render_hover_bar(&id, ui, cx);
        let row = div().w_full().flex().items_end().group("fg-message");
        if message.outgoing {
            row.justify_end()
                .children(hover_bar)
                .child(bubble)
                .into_any_element()
        } else {
            let avatar = match author_avatar.flatten() {
                Some(avatar) if look.last_of_run => Some(
                    render_avatar(
                        &avatar,
                        AVATAR_SIZE,
                        &self.plugin,
                        cx.entity_id(),
                        window,
                        cx,
                    )
                    .into_any_element(),
                ),
                Some(_) => Some(div().flex_none().w(px(AVATAR_SIZE)).into_any_element()),
                None => None,
            };
            row.gap(px(AVATAR_GAP))
                .children(avatar)
                .child(bubble)
                .children(hover_bar)
                .into_any_element()
        }
    }

    /// The plugin's actions with icons, over the hovered message.
    fn render_hover_bar(
        &self,
        id: &str,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let actions: Vec<&RowAction> = self
            .spec
            .message_actions
            .iter()
            .filter(|action| action.icon.is_some())
            .collect();
        if actions.is_empty() {
            return None;
        }
        Some(
            div()
                .flex_none()
                .flex()
                .gap(px(2.))
                .mb(px(2.))
                .opacity(0.)
                .group_hover("fg-message", |style| style.opacity(1.))
                .children(actions.into_iter().map(|action| {
                    let name = action.icon.clone().unwrap_or_default();
                    let glyph = plugin_icon(&self.plugin, &name, &ui);
                    let (message, action_id) = (id.to_string(), action.id.clone());
                    let tooltip = action
                        .tooltip
                        .clone()
                        .or_else(|| action.label.clone())
                        .unwrap_or_default();
                    div()
                        .id(SharedString::from(format!("fg-act-{id}-{}", action.id)))
                        .size(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(ui::RADIUS_SM))
                        .hover(|style| style.bg(ui.hover))
                        .children(glyph.map(|glyph| glyph.render(ui.text_muted)))
                        .tooltip(ui::tooltip(tooltip, None))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.emit(
                                UiEvent::MessageAction((message.clone(), action_id.clone())),
                                cx,
                            )
                        }))
                }))
                .into_any_element(),
        )
    }

    fn render_quote(
        &self,
        message: &str,
        quote: &MessageQuote,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let color = theme::avatar_color(quote.color);
        let (message, quoted) = (message.to_string(), quote.message.clone());
        quote_block(quote, color, ui)
            .id(SharedString::from(format!("fg-quote-{message}")))
            .when_some(quoted, |block, quoted| {
                block
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.click_quote(&message, &quoted, cx)
                    }))
            })
            .into_any_element()
    }

    /// Styled text with links and spoilers: a click on a link opens it, on a spoiler lifts it.
    fn render_rich(
        &self,
        message: &str,
        part: usize,
        spans: &[RichSpan],
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.open_spoilers.contains(&(message.to_string(), part));
        let (text, runs, highlights, links, spoilers) = rich_runs(spans, open, ui);
        let mut clickable: Vec<std::ops::Range<usize>> = Vec::new();
        let mut targets: Vec<Option<String>> = Vec::new();
        for (range, link) in links {
            clickable.push(range);
            targets.push(Some(link));
        }
        if !open {
            for range in spoilers {
                clickable.push(range);
                targets.push(None);
            }
        }
        let styled = StyledText::new(text)
            .with_default_highlights(&gpui::TextStyle::default(), Vec::new())
            .with_runs(runs);
        let styled = if highlights.is_empty() {
            styled
        } else {
            styled.with_highlights(highlights)
        };
        let weak = cx.entity().downgrade();
        let message = message.to_string();
        div()
            .text_size(px(TEXT_SIZE))
            .line_height(px(LINE_HEIGHT))
            .child(
                InteractiveText::new(
                    SharedString::from(format!("fg-text-{message}-{part}")),
                    styled,
                )
                .on_click(clickable, move |index, _, cx| {
                    let target = targets.get(index).cloned().flatten();
                    weak.update(cx, |this, cx| match target {
                        Some(link) => this.click_link(&message, &link, cx),
                        None => {
                            this.open_spoilers.insert((message.clone(), part));
                            cx.notify();
                        }
                    })
                    .ok();
                }),
            )
            .into_any_element()
    }

    fn render_album(
        &self,
        message: &str,
        album: &[(usize, ImageSpec)],
        inner: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let [(part, spec)] = album {
            let size = fit(
                spec.width,
                spec.height,
                inner.min(PHOTO_WIDTH),
                PHOTO_HEIGHT,
            );
            return self.render_picture_part(message, *part, spec, size, window, cx);
        }
        // Two or three in a row, squares cut from the pictures.
        let columns = if album.len() == 2 || album.len() == 4 {
            2
        } else {
            3
        };
        let width = inner.min(PHOTO_WIDTH);
        let side = (width - ALBUM_GAP * (columns - 1) as f32) / columns as f32;
        div()
            .w(px(width))
            .flex()
            .flex_wrap()
            .gap(px(ALBUM_GAP))
            .children(album.iter().map(|(part, spec)| {
                self.render_picture_part(message, *part, spec, (side, side), window, cx)
            }))
            .into_any_element()
    }

    fn render_picture_part(
        &self,
        message: &str,
        part: usize,
        spec: &ImageSpec,
        size: (f32, f32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let picture = render_picture(spec, size, &self.plugin, cx.entity_id(), window, cx);
        let (click_message, menu_message) = (message.to_string(), message.to_string());
        div()
            .id(SharedString::from(format!("fg-pic-{message}-{part}")))
            .flex_none()
            .cursor_pointer()
            .child(picture)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.click_picture(&click_message, part, window, cx)
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_menu(menu_message.clone(), Some(part), event.position, window, cx)
                }),
            )
            .into_any_element()
    }

    fn render_file(
        &self,
        message: &str,
        part: usize,
        file: &FileSpec,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let downloaded = file.path.is_some();
        let glyph = if downloaded {
            crate::icons::file_icon(&file.name, &ui)
                .render()
                .into_any_element()
        } else {
            icon(IconName::Download, ui.accent_text).into_any_element()
        };
        let size = match file.progress {
            Some(done) => format!(
                "{} / {}",
                file_size((file.size as f64 * f64::from(done.clamp(0., 1.))) as u64),
                file_size(file.size)
            ),
            None => file_size(file.size),
        };
        let (click_message, menu_message) = (message.to_string(), message.to_string());
        div()
            .id(SharedString::from(format!("fg-file-{message}-{part}")))
            .flex()
            .items_center()
            .gap(px(11.))
            .py(px(3.))
            .cursor_pointer()
            .child(
                div()
                    .flex_none()
                    .size(px(FILE_ICON))
                    .rounded_full()
                    .bg(ui.accent_soft)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(glyph),
            )
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(file.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.text_muted)
                            .child(size),
                    )
                    .when_some(file.progress, |column, done| {
                        column.child(
                            div()
                                .mt(px(3.))
                                .w(px(140.))
                                .h(px(3.))
                                .rounded_full()
                                .bg(UiColors::tint(ui.foreground, 0.12))
                                .child(
                                    div()
                                        .h_full()
                                        .rounded_full()
                                        .bg(ui.accent)
                                        .w(relative(done.clamp(0., 1.))),
                                ),
                        )
                    }),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.click_file(&click_message, part, cx)
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_menu(menu_message.clone(), Some(part), event.position, window, cx)
                }),
            )
            .into_any_element()
    }

    fn render_link_preview(
        &self,
        message: &str,
        part: usize,
        preview: &LinkPreview,
        inner: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let url = preview.url.clone();
        let message_id = message.to_string();
        div()
            .id(SharedString::from(format!("fg-link-{message}-{part}")))
            .pl(px(8.))
            .py(px(2.))
            .border_l_2()
            .border_color(ui.accent)
            .flex()
            .flex_col()
            .gap(px(2.))
            .cursor_pointer()
            .text_size(px(theme::TEXT_SM))
            .children(preview.site.clone().map(|site| {
                div()
                    .text_color(ui.accent_text)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(site)
            }))
            .children(
                preview
                    .title
                    .clone()
                    .map(|title| div().font_weight(FontWeight::SEMIBOLD).child(title)),
            )
            .children(
                preview
                    .description
                    .clone()
                    .map(|text| div().text_color(ui.text_muted).child(text)),
            )
            .children(preview.image.as_ref().map(|spec| {
                let size = fit(
                    spec.width,
                    spec.height,
                    (inner - 10.).min(PHOTO_WIDTH),
                    220.,
                );
                div().mt(px(4.)).child(render_picture(
                    spec,
                    size,
                    &self.plugin,
                    cx.entity_id(),
                    window,
                    cx,
                ))
            }))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.click_link(&message_id, &url, cx)
            }))
            .into_any_element()
    }

    fn render_composer(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let composer = self.spec.composer.as_ref()?;
        let ui = Theme::ui(cx);
        let focused = self.input.focus_handle(cx).is_focused(window);
        let editing = self.editing.is_some();
        // Over the field: the message being edited, or the quote being answered.
        let bar = match (&self.editing, &self.reply_to) {
            (Some(edit), _) => Some(field_bar(
                IconName::Pencil,
                quote_lines(tr("Editing").into(), &edit.text, ui.accent_text, ui),
                ui::icon_button("fg-edit-cancel", IconName::Close, ui).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.leave_editing(cx);
                        this.emit(UiEvent::EditCancelled, cx);
                    },
                )),
                ui,
            )),
            (None, Some(quote)) => Some(field_bar(
                IconName::ArrowLeft,
                quote_block(quote, theme::avatar_color(quote.color), ui),
                ui::icon_button("fg-reply-cancel", IconName::Close, ui).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.reply_to = None;
                        this.emit(UiEvent::ReplyCancelled, cx);
                        cx.notify();
                    },
                )),
                ui,
            )),
            (None, None) => None,
        };
        // A picture file's chip shows its thumbnail, decoded off the window's thread.
        let thumbnail_side = (CHIP_THUMBNAIL * window.scale_factor()).ceil() as u32;
        let chips =
            (!self.attachments.is_empty() && !editing).then(|| {
                div().flex().flex_wrap().gap(px(6.)).children(
                    self.attachments.iter().enumerate().map(|(index, path)| {
                        let name = path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let thumbnail = is_picture(path)
                            .then(|| {
                                crate::plugin_images::file(
                                    path,
                                    thumbnail_side,
                                    thumbnail_side,
                                    cx.entity_id(),
                                    cx,
                                )
                            })
                            .flatten();
                        let shows_thumbnail = thumbnail.is_some();
                        let glyph = match thumbnail {
                            Some(image) => img(image)
                                .flex_none()
                                .size(px(CHIP_THUMBNAIL))
                                .rounded(px(ui::RADIUS_XS))
                                .object_fit(ObjectFit::Cover)
                                .into_any_element(),
                            None => crate::icons::file_icon(&name, &ui)
                                .render()
                                .into_any_element(),
                        };
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .h(px(24.))
                            .pl(px(if shows_thumbnail { 3. } else { 8. }))
                            .pr(px(4.))
                            .rounded(px(ui::RADIUS_SM))
                            .bg(UiColors::tint(ui.foreground, 0.05))
                            .border_1()
                            .border_color(ui.divider)
                            .text_size(px(theme::TEXT_SM))
                            .child(glyph)
                            .child(div().max_w(px(220.)).truncate().child(name))
                            .child(
                                ui::icon_button(("fg-chip-remove", index), IconName::Close, ui)
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        if index < this.attachments.len() {
                                            this.attachments.remove(index);
                                        }
                                        cx.notify();
                                    })),
                            )
                    }),
                )
            });
        let actions = composer.actions.iter().map(|action| {
            let action_id = action.id.clone();
            let label = action.label.clone().unwrap_or_default();
            let glyph = action
                .icon
                .as_deref()
                .and_then(|name| plugin_icon(&self.plugin, name, &ui));
            div()
                .id(SharedString::from(format!("fg-composer-{}", action.id)))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(6.))
                .h(px(28.))
                .px(px(8.))
                .rounded(px(ui::RADIUS_SM))
                .text_size(px(theme::TEXT_SM))
                .hover(|style| style.bg(ui.hover))
                .children(glyph.map(|glyph| glyph.render(ui.claude)))
                .child(label)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    let composed = this.composed(cx);
                    this.emit(UiEvent::ComposerAction((action_id.clone(), composed)), cx)
                }))
        });
        let attachments = composer.attachments && !editing;
        Some(
            div()
                .w_full()
                .flex()
                .justify_center()
                .px(px(COLUMN_PADDING))
                .pb(px(12.))
                .pt(px(6.))
                .child(
                    div()
                        .id("fg-composer")
                        .key_context(FIELD_CONTEXT)
                        .w_full()
                        .max_w(px(COLUMN_WIDTH))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .px(px(10.))
                        .py(px(8.))
                        .rounded(px(11.))
                        .bg(ui.input_background)
                        .border_1()
                        .border_color(if focused {
                            ui.focus_border
                        } else {
                            ui.input_border
                        })
                        .on_action(cx.listener(Self::send))
                        .on_action(cx.listener(Self::cancel_edit))
                        .when(attachments, |field| {
                            field
                                .drag_over::<ExternalPaths>(move |style, _, _, _| {
                                    style.border_color(ui.focus_border).bg(ui.drop_target)
                                })
                                .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                                    this.attach(paths.paths().to_vec(), cx)
                                }))
                        })
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            window.focus(&this.input.focus_handle(cx))
                        }))
                        .children(bar)
                        .children(chips)
                        .child(
                            div()
                                .flex()
                                .items_end()
                                .gap(px(8.))
                                .when(attachments, |row| {
                                    row.child(
                                        ui::icon_button("fg-attach", IconName::Paperclip, ui)
                                            .tooltip(ui::tooltip(tr("Attach Files"), None))
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, window, cx| {
                                                    this.choose_files(window, cx)
                                                },
                                            )),
                                    )
                                })
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .py(px(4.))
                                        .child(self.input.clone()),
                                )
                                .children(actions)
                                .child(
                                    div()
                                        .id("fg-send")
                                        .flex_none()
                                        .size(px(28.))
                                        .rounded(px(8.))
                                        .bg(ui.accent)
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        // Editing: a check mark — Save.
                                        .child(icon(
                                            if editing {
                                                IconName::Check
                                            } else {
                                                IconName::ArrowUp
                                            },
                                            ui.on_accent,
                                        ))
                                        .tooltip(ui::tooltip(
                                            if editing { tr("Save") } else { tr("Send") },
                                            None,
                                        ))
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.send(&Send, window, cx)
                                            },
                                        )),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}

impl Render for Conversation {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        self.sight.borrow_mut().drawn.clear();
        let feed =
            if self.order.is_empty() {
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(ui.text_muted)
                    .child(self.spec.empty_text.clone().unwrap_or_default())
                    .into_any_element()
            } else {
                let sight = self.sight.clone();
                let weak: WeakEntity<Self> = cx.entity().downgrade();
                let loading = |history: History| history == History::Loading;
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        list(
                            self.list.clone(),
                            cx.processor(|this, ix, window, cx| this.render_item(ix, window, cx)),
                        )
                        .size_full(),
                    )
                    // After the feed is laid out (its messages recorded where they were drawn): what
                    // is on screen, and the ends in sight.
                    .on_children_prepainted(move |children, _, cx| {
                        sight.borrow_mut().viewport = children.first().copied();
                        weak.update(cx, |this, cx| this.observe_sight(cx)).ok();
                    })
                    .when(loading(self.older), |feed| {
                        feed.child(loading_pill(ui).absolute().top(px(8.)))
                    })
                    .when(loading(self.newer), |feed| {
                        feed.child(loading_pill(ui).absolute().bottom(px(8.)))
                    })
                    .when(!self.at_bottom, |feed| {
                        feed.child(
                            div()
                                .id("fg-to-bottom")
                                .absolute()
                                .right(px(16.))
                                .bottom(px(12.))
                                .size(px(34.))
                                .rounded_full()
                                .bg(ui.elevated)
                                .border_1()
                                .border_color(ui.elevated_border)
                                .shadow_md()
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .child(icon(IconName::ArrowDown, ui.text_muted))
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.scroll_to_bottom(cx)
                                })),
                        )
                    })
                    .into_any_element()
            };
        // A quote or an edit the plugin put over the field: the field takes the keyboard — only in
        // the active window (the user may work in another one).
        if std::mem::take(&mut self.take_keyboard) && window_active(window) {
            window.focus(&self.input.focus_handle(cx));
        }
        let composer = self.render_composer(window, cx);
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .on_action(cx.listener(Self::choose_action))
            .on_action(cx.listener(Self::copy_text))
            .on_action(cx.listener(Self::open_part))
            .on_action(cx.listener(Self::reveal_part))
            .on_action(cx.listener(Self::save_part_as))
            .child(feed)
            .children(composer)
            .child(ContextMenu::slot(
                "fg-message-menu",
                self.menu.as_ref().map(|open| (&open.menu, open.position)),
            ))
    }
}

/// How a message is drawn in its place.
struct Look {
    ui: UiColors,
    /// The column's width.
    column: f32,
    first_of_run: bool,
    last_of_run: bool,
    /// Revealed: how far its mark has faded (0–1).
    marked: Option<f32>,
}

/// The width a bubble's content needs, up to `inner`: its widest text line as shaped, its
/// pictures, files and the like; code and link previews take the whole width.
fn natural_width(message: &Message, inner: f32, ui: UiColors, window: &mut Window) -> f32 {
    let regular = |text: &str, size: f32, window: &mut Window| {
        text.lines()
            .map(|line| shaped_width(line, font(theme::UI_FONT), size, window))
            .fold(0., f32::max)
    };
    let mut width: f32 = 64.;
    if let Some(author) = &message.author {
        let mut name = font(theme::UI_FONT);
        name.weight = FontWeight::SEMIBOLD;
        width = width.max(shaped_width(&author.name, name, theme::TEXT_SM, window));
    }
    if let Some(forwarded) = &message.forwarded {
        width = width.max(regular(
            &trf("Forwarded from {0}", &[forwarded]),
            theme::TEXT_SM,
            window,
        ));
    }
    if message.reply.is_some() {
        width = width.max(180.);
    }
    // The time, "edited" and the mark.
    width = width.max(if message.edited { 100. } else { 56. });
    width = width.max(message.reactions.len() as f32 * 52.);
    let mut pictures = 0;
    for part in &message.parts {
        let part_width = match part {
            MessagePart::Text(spans) | MessagePart::Notice(spans) => rich_width(spans, ui, window),
            MessagePart::Quote(spans) => rich_width(spans, ui, window) + 10.,
            MessagePart::Code(_) | MessagePart::Link(_) => inner,
            MessagePart::Image(spec) | MessagePart::Sticker(spec) => {
                pictures += 1;
                if pictures > 1 {
                    inner.min(PHOTO_WIDTH)
                } else {
                    fit(
                        spec.width,
                        spec.height,
                        inner.min(PHOTO_WIDTH),
                        PHOTO_HEIGHT,
                    )
                    .0
                }
            }
            // The name is drawn in the medium weight.
            MessagePart::File(file) => {
                let mut medium = font(theme::UI_FONT);
                medium.weight = FontWeight::MEDIUM;
                FILE_ICON + 11. + shaped_width(&file.name, medium, TEXT_SIZE, window).max(140.)
            }
            MessagePart::Placeholder(placeholder) => {
                22. + regular(&placeholder.text, TEXT_SIZE, window)
            }
        };
        width = width.max(part_width);
    }
    width.ceil().min(inner)
}

/// The width of rich spans as one line each (a line break starts another).
fn rich_width(spans: &[RichSpan], ui: UiColors, window: &mut Window) -> f32 {
    let (text, runs, ..) = rich_runs(spans, true, ui);
    if text.is_empty() {
        return 0.;
    }
    if !text.contains('\n') {
        return f32::from(
            window
                .text_system()
                .shape_line(text.into(), px(TEXT_SIZE), &runs, None)
                .width,
        ) + 1.;
    }
    // Lines in the regular font, with a little room for bold and code.
    text.lines()
        .map(|line| shaped_width(line, font(theme::UI_FONT), TEXT_SIZE, window) * 1.06)
        .fold(0., f32::max)
}

fn shaped_width(text: &str, run_font: gpui::Font, size: f32, window: &mut Window) -> f32 {
    if text.is_empty() {
        return 0.;
    }
    let run = TextRun {
        len: text.len(),
        font: run_font,
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    f32::from(
        window
            .text_system()
            .shape_line(text.to_string().into(), px(size), &[run], None)
            .width,
    ) + 1.
}

/// Messages of one run: the same side and author, no notice between.
fn same_run(a: &Message, b: &Message) -> bool {
    let notice = |message: &Message| matches!(message.parts.first(), Some(MessagePart::Notice(_)));
    a.outgoing == b.outgoing
        && !notice(a)
        && !notice(b)
        && a.author.as_ref().map(|author| &author.name)
            == b.author.as_ref().map(|author| &author.name)
}

/// A picture of `width` × `height` within `max_width` × `max_height`, never larger than itself;
/// an unknown size takes a box of 4:3.
fn fit(width: u32, height: u32, max_width: f32, max_height: f32) -> (f32, f32) {
    if width == 0 || height == 0 {
        let width = max_width.min(240.);
        return (width, (width * 0.75).min(max_height));
    }
    let (width, height) = (width as f32, height as f32);
    let scale = (max_width / width).min(max_height / height).min(1.);
    (
        (width * scale).round().max(1.),
        (height * scale).round().max(1.),
    )
}

/// A picture at `size` (logical pixels): its file decoded at that size, the blurred preview until
/// then, or a dim box; the download's progress over it. Shared with the window's `image` element.
pub fn render_picture(
    spec: &ImageSpec,
    size: (f32, f32),
    plugin: &str,
    view: gpui::EntityId,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let ui = Theme::ui(cx);
    let scale = window.scale_factor();
    let radius = match spec.shape {
        ImageShape::Rounded => px(PICTURE_RADIUS),
        ImageShape::Square => px(0.),
        ImageShape::Circle => px(size.0.min(size.1) / 2.),
    };
    let device = |side: f32| (side * scale).ceil() as u32;
    let file = spec
        .path
        .as_ref()
        .map(PathBuf::from)
        .filter(|path| crate::plugin_images::allowed(plugin, path, cx))
        .and_then(|path| {
            crate::plugin_images::file(&path, device(size.0), device(size.1), view, cx)
        });
    let shown = file.or_else(|| {
        spec.preview
            .as_ref()
            .and_then(|bytes| crate::plugin_images::preview(bytes, view, cx))
    });
    div()
        .relative()
        .flex_none()
        .w(px(size.0))
        .h(px(size.1))
        .rounded(radius)
        .overflow_hidden()
        .bg(UiColors::tint(ui.foreground, 0.06))
        .children(shown.map(|image| {
            img(image)
                .size_full()
                .rounded(radius)
                .object_fit(ObjectFit::Cover)
        }))
        .children(spec.progress.map(|done| {
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .px(px(8.))
                        .h(px(24.))
                        .rounded_full()
                        .bg(gpui::rgba(0x000000a0))
                        .text_size(px(theme::TEXT_XS))
                        .text_color(gpui::white())
                        .child(icon(IconName::Download, gpui::white()).size(px(12.)))
                        .child(format!("{}%", (done.clamp(0., 1.) * 100.).round() as u32)),
                )
        }))
        .into_any_element()
}

/// An avatar `size` across: the picture, or the initials on the avatar color.
pub fn render_avatar(
    spec: &AvatarSpec,
    size: f32,
    plugin: &str,
    view: gpui::EntityId,
    window: &mut Window,
    cx: &mut App,
) -> gpui::Div {
    let scale = window.scale_factor();
    let device = (size * scale).ceil() as u32;
    let picture = spec
        .image
        .as_ref()
        .map(PathBuf::from)
        .filter(|path| crate::plugin_images::allowed(plugin, path, cx))
        .and_then(|path| crate::plugin_images::file(&path, device, device, view, cx));
    let initials: String = spec.initials.chars().take(2).collect();
    div()
        .flex_none()
        .size(px(size))
        .rounded_full()
        .overflow_hidden()
        .bg(theme::avatar_color(spec.color))
        .flex()
        .items_center()
        .justify_center()
        .text_color(gpui::white())
        .text_size(px((size * 0.4).round()))
        .font_weight(FontWeight::SEMIBOLD)
        .map(|avatar| match picture {
            // A pixel radius: gpui clips a picture by its own corners, not by its parent's.
            Some(image) => avatar.child(
                img(image)
                    .size_full()
                    .rounded(px(size / 2.))
                    .object_fit(ObjectFit::Cover),
            ),
            None => avatar.child(initials),
        })
}

/// A quote: the author in their color, one line of the text, a bar on the left.
fn quote_block(quote: &MessageQuote, color: Hsla, ui: UiColors) -> gpui::Div {
    quote_lines(quote.author.clone().into(), &quote.text, color, ui)
}

/// A title in `color` over one line of text, a bar on the left: a quote, the message being
/// edited.
fn quote_lines(title: SharedString, text: &str, color: Hsla, ui: UiColors) -> gpui::Div {
    div()
        .min_w_0()
        .flex()
        .flex_col()
        .px(px(8.))
        .pt(px(3.))
        .pb(px(4.))
        .rounded(px(ui::RADIUS_XS))
        .border_l_2()
        .border_color(color)
        .bg(UiColors::tint(color, 0.08))
        .text_size(px(theme::TEXT_SM))
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(color)
                .truncate()
                .child(title),
        )
        .child(
            div()
                .text_color(ui.text_muted)
                .truncate()
                .child(text.replace('\n', " ")),
        )
}

/// The bar over the field: an icon, what it is about (a quote, the message being edited), ×.
fn field_bar(
    glyph: IconName,
    block: gpui::Div,
    close: gpui::Stateful<gpui::Div>,
    ui: UiColors,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(9.))
        .px(px(4.))
        .pb(px(6.))
        .border_b_1()
        .border_color(ui.divider)
        .child(icon(glyph, ui.accent_text))
        .child(block.flex_1().min_w_0())
        .child(close)
}

/// A file the field shows as a picture (its chip has a thumbnail): PNG, JPEG, WebP, GIF.
fn is_picture(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            PICTURE_EXTENSIONS
                .iter()
                .any(|picture| extension.eq_ignore_ascii_case(picture))
        })
}

/// The window is the one the user works in (a scenario in the background counts as such).
fn window_active(window: &Window) -> bool {
    #[cfg(feature = "scenario")]
    if crate::scenario::background() {
        return true;
    }
    window.is_window_active()
}

fn render_notice(spans: &[RichSpan], ui: UiColors) -> AnyElement {
    let text: String = spans.iter().map(|span| span.text.as_str()).collect();
    div()
        .w_full()
        .flex()
        .justify_center()
        .child(
            div()
                .px(px(10.))
                .py(px(2.))
                .rounded(px(10.))
                .bg(UiColors::tint(ui.foreground, 0.05))
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.text_muted)
                .child(text),
        )
        .into_any_element()
}

fn render_reactions(message: &Message, ui: UiColors) -> AnyElement {
    div()
        .flex()
        .flex_wrap()
        .gap(px(4.))
        .children(message.reactions.iter().map(|reaction| {
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .px(px(7.))
                .py(px(1.))
                .rounded(px(10.))
                .text_size(px(theme::TEXT_SM))
                .map(|pill| {
                    if reaction.chosen {
                        pill.bg(UiColors::tint(ui.accent, 0.25))
                            .border_1()
                            .border_color(UiColors::tint(ui.accent, 0.6))
                            .text_color(ui.accent_text)
                    } else {
                        pill.bg(UiColors::tint(ui.foreground, 0.063))
                            .border_1()
                            .border_color(gpui::transparent_black())
                            .text_color(ui.text_muted)
                    }
                })
                .child(reaction.emoji.clone())
                .child(reaction.count.to_string())
        }))
        .into_any_element()
}

/// The time, "edited" and the delivery mark at a bubble's foot (under a sticker, on a pill).
fn render_meta(message: &Message, under_sticker: bool, ui: UiColors) -> AnyElement {
    let minutes = local_seconds(message.date).rem_euclid(86_400) / 60;
    let clock = format!("{:02}:{:02}", minutes / 60, minutes % 60);
    let mark = message.delivery.map(|delivery| match delivery {
        Delivery::Sending => icon(IconName::Clock, ui.dim),
        Delivery::Sent => icon(IconName::Check, ui.dim),
        Delivery::Read => icon(IconName::CheckAll, ui.accent_text),
        Delivery::Failed => icon(IconName::Error, ui.error),
    });
    let meta = div()
        .flex()
        .items_center()
        .gap(px(4.))
        .text_size(px(10.5))
        .text_color(ui.dim)
        .when(under_sticker, |meta| {
            meta.px(px(6.))
                .py(px(1.))
                .rounded(px(8.))
                .bg(UiColors::tint(ui.shadow, 0.35))
                .text_color(ui.text_muted)
        })
        .when(message.edited, |meta| meta.child(tr("edited")))
        .child(clock)
        .children(mark.map(|mark| mark.size(px(12.))));
    // At the bubble's right; under an incoming sticker, at its left.
    div()
        .flex()
        .map(|row| {
            if under_sticker && !message.outgoing {
                row.justify_start()
            } else {
                row.justify_end()
            }
        })
        .child(meta)
        .into_any_element()
}

/// A day's line between days: "Today", "Yesterday", "17 November", with the year when not this
/// one.
fn day_divider(local: i64, now: i64, ui: UiColors) -> AnyElement {
    let (day, today) = (local.div_euclid(86_400), now.div_euclid(86_400));
    let label = if day == today {
        tr("Today").to_string()
    } else if day + 1 == today {
        tr("Yesterday").to_string()
    } else {
        let (year, month, date) = civil_date(day);
        let (this_year, _, _) = civil_date(today);
        let month = month_day(month, date);
        if year == this_year {
            month
        } else {
            format!("{month} {year}")
        }
    };
    div()
        .w_full()
        .flex()
        .justify_center()
        .pb(px(6.))
        .child(
            div()
                .px(px(10.))
                .py(px(2.))
                .rounded(px(10.))
                .bg(UiColors::tint(ui.foreground, 0.047))
                .text_size(px(11.5))
                .text_color(ui.text_muted)
                .child(label),
        )
        .into_any_element()
}

/// The "New messages" line.
fn unread_divider(column: f32, ui: UiColors) -> AnyElement {
    let line = || div().flex_1().h(px(1.)).bg(UiColors::tint(ui.accent, 0.33));
    div()
        .w(px(column))
        .flex()
        .items_center()
        .gap(px(10.))
        .py(px(4.))
        .text_size(px(11.5))
        .text_color(ui.accent_text)
        .child(line())
        .child(tr("New messages"))
        .child(line())
        .into_any_element()
}

fn loading_pill(ui: UiColors) -> gpui::Div {
    div().left_0().right_0().flex().justify_center().child(
        div()
            .px(px(10.))
            .py(px(2.))
            .rounded(px(10.))
            .bg(ui.elevated)
            .border_1()
            .border_color(ui.elevated_border)
            .text_size(px(theme::TEXT_SM))
            .text_color(ui.text_muted)
            .child(tr("Loading…")),
    )
}

/// "17 November".
fn month_day(month: u32, day: u32) -> String {
    let template = match month {
        1 => "January {0}",
        2 => "February {0}",
        3 => "March {0}",
        4 => "April {0}",
        5 => "May {0}",
        6 => "June {0}",
        7 => "July {0}",
        8 => "August {0}",
        9 => "September {0}",
        10 => "October {0}",
        11 => "November {0}",
        _ => "December {0}",
    };
    trf(template, &[&day])
}

/// The text runs of rich spans: the text, its runs, the highlights (matches), the links and the
/// spoilers (byte ranges).
#[allow(clippy::type_complexity)]
fn rich_runs(
    spans: &[RichSpan],
    spoilers_open: bool,
    ui: UiColors,
) -> (
    String,
    Vec<TextRun>,
    Vec<(std::ops::Range<usize>, HighlightStyle)>,
    Vec<(std::ops::Range<usize>, String)>,
    Vec<std::ops::Range<usize>>,
) {
    let mut text = String::new();
    let mut runs = Vec::new();
    let mut highlights = Vec::new();
    let mut links = Vec::new();
    let mut spoilers = Vec::new();
    for span in spans {
        if span.text.is_empty() {
            continue;
        }
        let start = text.len();
        text.push_str(&span.text);
        let range = start..text.len();
        let mut run_font = font(if span.code {
            theme::code_font()
        } else {
            theme::UI_FONT
        });
        if span.bold {
            run_font.weight = FontWeight::SEMIBOLD;
        }
        if span.italic {
            run_font.style = FontStyle::Italic;
        }
        let hidden = span.spoiler && !spoilers_open;
        let color = if hidden {
            UiColors::tint(ui.text_muted, 0.0)
        } else if span.link.is_some() {
            ui.accent_text
        } else {
            ui.foreground
        };
        runs.push(TextRun {
            len: span.text.len(),
            font: run_font,
            color,
            background_color: if hidden {
                Some(UiColors::tint(ui.text_muted, 0.55))
            } else if span.code {
                Some(UiColors::tint(ui.foreground, 0.08))
            } else {
                None
            },
            underline: (span.underline || span.link.is_some()).then(|| UnderlineStyle {
                thickness: px(1.),
                color: Some(color),
                wavy: false,
            }),
            strikethrough: span.strikethrough.then(|| StrikethroughStyle {
                thickness: px(1.),
                color: Some(color),
            }),
        });
        if span.highlight {
            highlights.push((
                range.clone(),
                HighlightStyle {
                    background_color: Some(ui.search_match),
                    ..Default::default()
                },
            ));
        }
        if let Some(link) = &span.link {
            links.push((range.clone(), link.clone()));
        }
        if span.spoiler {
            spoilers.push(range);
        }
    }
    (text, runs, highlights, links, spoilers)
}

/// A message's text for Copy Text: its text, quotes and code, a part a line.
fn message_text(message: &Message) -> String {
    let spans = |spans: &[RichSpan]| {
        spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>()
    };
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text(text) | MessagePart::Quote(text) | MessagePart::Notice(text) => {
                Some(spans(text))
            }
            MessagePart::Code(code) => Some(code.text.clone()),
            MessagePart::Link(link) => Some(link.url.clone()),
            _ => None,
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// "1,2 KB" as the file tree's sizes read.
fn file_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024. && unit + 1 < UNITS.len() {
        size /= 1024.;
        unit += 1;
    }
    let number = if unit == 0 || size >= 10. {
        format!("{}", size.round() as u64)
    } else {
        format!("{size:.1}")
    };
    format!("{number} {}", tr(UNITS[unit]))
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// Seconds since the epoch in the local time zone.
fn local_seconds(utc: i64) -> i64 {
    utc + crate::git_log::local_offset(utc)
}

/// Year, month, day of a day number since 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_date(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The system's app opens a file — in a scenario with `FLUX_SCENARIO_OPEN_URL`, a line in that
/// file instead.
fn open_with_system(path: &Path, cx: &mut App) {
    #[cfg(feature = "scenario")]
    if let Some(file) = std::env::var_os("FLUX_SCENARIO_OPEN_URL") {
        use std::io::Write as _;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
        {
            let _ = writeln!(file, "open {}", path.display());
        }
        return;
    }
    cx.open_with_system(path);
}

/// Finder (Explorer) with the file selected — in a scenario a line in `FLUX_SCENARIO_OPEN_URL`.
fn reveal_path(path: &Path, cx: &mut App) {
    #[cfg(feature = "scenario")]
    if let Some(file) = std::env::var_os("FLUX_SCENARIO_OPEN_URL") {
        use std::io::Write as _;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
        {
            let _ = writeln!(file, "reveal {}", path.display());
        }
        return;
    }
    cx.reveal_path(path);
}

/// Where Save As… starts: the Downloads folder, else the home folder.
fn dirs_downloads() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    let downloads = home.join("Downloads");
    if downloads.is_dir() { downloads } else { home }
}

/// A pasted picture saved as a PNG file in the temporary folder (the field sends paths).
fn save_pasted(image: &gpui::Image) -> Option<PathBuf> {
    let folder = std::env::temp_dir().join("flux-pasted");
    std::fs::create_dir_all(&folder).ok()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    let path = folder.join(format!("pasted-{stamp}.png"));
    if image.format == gpui::ImageFormat::Png {
        std::fs::write(&path, &image.bytes).ok()?;
    } else {
        let decoded = image::load_from_memory(&image.bytes).ok()?;
        decoded
            .save_with_format(&path, image::ImageFormat::Png)
            .ok()?;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_fit_their_box_and_never_grow() {
        assert_eq!(fit(1000, 500, 360., 360.), (360., 180.));
        assert_eq!(fit(100, 50, 360., 360.), (100., 50.));
        assert_eq!(fit(500, 1000, 360., 360.), (180., 360.));
        assert_eq!(fit(0, 0, 360., 360.), (240., 180.));
    }

    #[test]
    fn days_and_sizes_read_as_people_write_them() {
        assert_eq!(civil_date(0), (1970, 1, 1));
        assert_eq!(civil_date(20_774), (2026, 11, 17));
        assert_eq!(file_size(1200), "1.2 KB");
        assert_eq!(file_size(512), "512 B");
    }

    #[test]
    fn rich_spans_make_runs_links_and_spoilers() {
        let span = |text: &str| RichSpan {
            text: text.to_string(),
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            code: false,
            spoiler: false,
            highlight: false,
            link: None,
        };
        let spans = vec![
            span("see "),
            RichSpan {
                link: Some("https://example.com".into()),
                ..span("this")
            },
            RichSpan {
                spoiler: true,
                ..span(" secret")
            },
        ];
        let ui = Theme::flux_night().ui;
        let (text, runs, _, links, spoilers) = rich_runs(&spans, false, ui);
        assert_eq!(text, "see this secret");
        assert_eq!(runs.len(), 3);
        assert_eq!(links, vec![(4..8, "https://example.com".to_string())]);
        assert_eq!(spoilers, vec![8..15]);
    }
}
