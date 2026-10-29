//! The conversation of a Claude chat, in the "chat" look the author chose for part 10.2: the
//! user's messages are bubbles on the right (mentions at their ends as chips above, pictures as
//! thumbnails, the time under them), Claude's turns are on the left under its avatar — Markdown,
//! the minor steps (reads, searches, thinking, task updates) folded into a group, the major ones
//! as cards (a command, an edit, a subagent) — and a turn that changed files ends with the "Done"
//! card. `!` commands are terminal cards on the user's side; notices and the line of what Claude
//! is doing now complete it.
//!
//! The conversation is a gpui `list` anchored to the bottom, as a chat log: it follows new content
//! while the user is at the end and stays put when they scrolled up (a "↓" button brings them
//! back). Its items ([`Item`]) are the shown entries, minor steps in a row being one item; only the
//! visible items are drawn, and an item whose content changed is measured again
//! (`TranscriptState::sync` compares a fingerprint of each). Claude's Markdown is parsed once per
//! change of the text and highlighted off the UI thread when the text is complete.

use std::cell::{Cell, RefCell};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{
    Animation, AnimationExt, AnyElement, App, BoxShadow, ClipboardItem, Context, Div, Entity,
    FontWeight, Hsla, Image, ImageFormat, InteractiveText, ListAlignment, ListOffset, ListState,
    Pixels, SharedString, Stateful, StyledImage, StyledText, Svg, TextRun, Transformation,
    WeakEntity, Window, div, font, img, linear_color_stop, linear_gradient, list, point,
    prelude::*, px, radians,
};
use serde_json::Value;

use flux_claude::session::{
    ShellEntry, ShellState, TaskProgress, TurnFile, TurnSummary, UserEntry,
};
use flux_claude::{
    Activity, Entry, EntryId, EntryKind, Notice, Session, Status, ToolCall, ToolEntry, ToolState,
};

use crate::claude_chat::{ClaudeChat, ClaudeChatEvent};
use crate::claude_search::SearchMarks;
use crate::claude_select::{self, Collect};
use crate::claude_session::ClaudeSession;
use crate::i18n::{tr, trf, trn};
use crate::icons::{IconName, IconSource, file_icon, icon, source_icon};
use crate::image_view::ImageSource;
use crate::markdown::{self, Block, Marks};
use crate::motion::{self, Appear, Curve, TransitionExt};
use crate::region_guard::{self, Verdict};
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_MD, RADIUS_SM};
use crate::workspace::Location;

/// The conversation's column: centered, at most this wide (the chat as an editor tab); the field
/// and the cards under it keep to the same column.
pub const COLUMN_MAX_WIDTH: f32 = 780.;
/// The column's inset from the island's edges.
pub const COLUMN_INSET: f32 = 14.;
/// Claude's avatar, and the room its side of the conversation keeps for it.
const AVATAR: f32 = 26.;
const AVATAR_GAP: f32 = 10.;
/// The user's bubbles and command cards take at most this share of the column.
const BUBBLE_SHARE: f32 = 0.78;
/// The size of the conversation's text.
const CHAT_TEXT: f32 = 13.5;
/// A row of Claude's actions.
const ROW_HEIGHT: f32 = 26.;
/// Where a tool's details start: under the row's label, past the icon.
const DETAIL_INSET: f32 = 22.;
/// Output lines shown before "Show more".
const CLIP_LINES: usize = 12;
/// The longest answer to one question its row shows alone; a longer one also opens in the details.
const ANSWER_IN_ROW: usize = 40;
/// Diff lines an edit card shows before "Show more".
const DIFF_PREVIEW_LINES: usize = 8;
/// A running subagent's latest steps on its card.
const AGENT_STEPS: usize = 3;
/// The cards' corners.
const CARD_RADIUS: f32 = 10.;
/// How long an entry a click brought into sight stays highlighted, then how long the highlight
/// takes to fade.
pub(crate) const REVEALED_HOLD: Duration = Duration::from_millis(1600);
pub(crate) const REVEALED_FADE: Duration = Duration::from_millis(500);
/// Space kept above (or below) an entry a click brings into sight.
const REVEAL_MARGIN: f32 = 48.;
/// More entries than this arriving in one sync are a conversation loading, not Claude talking:
/// they simply appear.
const MAX_ARRIVALS: usize = 6;

pub fn init(cx: &mut App) {
    crate::claude_agent_view::init(cx);
    // After the subagent view's keys: Esc clears a selection before it goes back.
    claude_select::init(cx);
}

/// The transcript's state in its chat: the list, what the user expanded, parsed Markdown.
pub struct TranscriptState {
    list: ListState,
    /// The list's items as of the last sync.
    items: Vec<Item>,
    /// What each item looked like when it was last measured.
    fingerprints: Vec<u64>,
    /// How many entries there were at the last sync: the user's new message brings the view down.
    entry_count: usize,
    /// The item that shows each entry; a subagent's entries — by the item of their Agent call.
    item_of: HashMap<EntryId, usize>,
    /// Entries whose expansion the user flipped (tool details, thinking, a card's output).
    pub(crate) toggled: HashSet<EntryId>,
    /// Groups of steps the user opened or closed, by their first entry (otherwise a group is open
    /// while its turn runs and closed after).
    opened: HashMap<EntryId, bool>,
    /// Long outputs and diffs the user asked to see whole.
    unclipped: HashSet<EntryId>,
    /// Claude's texts and thinking, parsed (and highlighted once complete), by entry.
    texts: HashMap<EntryId, Parsed>,
    /// Thumbnails of the user's pictures, by entry and index.
    images: HashMap<(EntryId, usize), Arc<Image>>,
    // Text selection with the mouse (`claude_select`).
    /// The text selected in one message.
    pub(crate) selection: Option<claude_select::Selection>,
    /// The messages' texts as last drawn: where the mouse is in them.
    pub(crate) drawn: claude_select::Drawn,
    /// The transcript's draws, counted: a message's texts are numbered anew in each.
    frame: Cell<u64>,
    /// The copy button that shows a check mark, and since when.
    copied: Option<(EntryId, ui::Copied)>,
    /// An entry to bring into view at the next sync ([`TranscriptState::reveal`]).
    reveal: Option<EntryId>,
    /// The view goes to it at once rather than gliding ([`TranscriptState::reveal_at_once`]).
    reveal_at_once: bool,
    /// The session changed since the last sync.
    dirty: bool,
    /// What it shows: `None` — the session's conversation; an Agent call — its subagent's (the
    /// subagent's view, part 10.2).
    pub(crate) scope: Option<EntryId>,
    /// The chat's search (⌘F): the matches to mark, the current one opened (part 10.2).
    pub(crate) search: Option<SearchMarks>,

    // What a click on something that runs (a background command, the session's status) brought
    // into sight.
    /// The entry and since when: it shows highlighted for a moment ([`REVEALED_HOLD`]).
    revealed: Option<(EntryId, Instant)>,
    /// Entries such a click opened: they stay open until the user closes them.
    shown: HashSet<EntryId>,

    // Motion (2026-10-28). The list draws only what is in view and builds each row anew, so what
    // moves is driven by the times kept here, never by element ids: an appearance doesn't replay
    // when its row scrolls back into view or a streamed delta changes it.
    /// The list has been synced once: entries known then were there before the chat looked.
    synced: bool,
    /// The newest entry known at the last sync: newer ones arrived since.
    newest: Option<EntryId>,
    /// The session's saved conversation was still being read at the last sync: what comes with it
    /// isn't an arrival.
    loading: bool,
    /// Entries that arrived while the chat was open, and when: the user's message rises from the
    /// field, Claude's blocks fade in, the "Done" card pops.
    arrived: HashMap<EntryId, Instant>,
    /// Bodies unfolding or folding away (a click on a row, a card, a group; a group closing by
    /// itself when its turn moves on), by entry.
    folds: HashMap<EntryId, Fold>,
    /// The natural height of each folding body as last drawn: its clip glides to it.
    fold_heights: Rc<RefCell<HashMap<EntryId, Pixels>>>,
    /// The group open while its turn runs, as of the last sync: when it closes by itself, it folds.
    live_group: Option<EntryId>,
    /// A redraw at the next animation tick is on its way ([`request_tick`]).
    tick_pending: Rc<Cell<bool>>,
    /// A jump gliding to its place ([`Glide`]); the wheel drops it (the list's scroll handler).
    glide: Rc<Cell<Option<Glide>>>,
    /// The "↓" button was drawn last frame, and since when it leaves: it fades out, taking no
    /// input, once the view is back at the end.
    jump_shown: Cell<bool>,
    jump_left: Cell<Option<Instant>>,
}

/// Where a [`Glide`] of the conversation goes.
#[derive(Clone, Copy, Debug, PartialEq)]
enum GlideTo {
    /// The end: there the list follows new content again.
    End,
    /// A scroll position, an item and the offset into it: an entry revealed, a match.
    Place(usize, Pixels),
}

/// A jump of the conversation gliding to its place ([`motion::SCROLL`]): "Jump to Latest", the
/// user's message sent while scrolled up, an entry revealed, the search's next match. The list
/// knows the heights of its items only once it has measured them, so the glide keeps no pixel
/// positions: each frame it puts the list at the target and back by what is left of the way —
/// it lands exactly on the target however the heights on the way turn out, and a glide to the
/// end goes along with content growing there. A way longer than the view starts a view short of
/// the target: the list jumps most of a long way and glides the last screen. Scrolling by hand
/// takes over (the list's scroll handler drops the glide).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Glide {
    to: GlideTo,
    /// How far short of the target the glide starts, in pixels: above it when positive.
    short: f32,
    since: Instant,
}

impl Glide {
    /// A glide over the way from the view's top at `from` to `target` (pixels down the list), at
    /// most `limit` of it; `None` if there is nowhere to go.
    fn new(to: GlideTo, from: f32, target: f32, limit: f32, now: Instant) -> Option<Self> {
        let limit = limit.max(0.);
        let short = (target - from).clamp(-limit, limit);
        (short.abs() >= 0.5).then_some(Self {
            to,
            short,
            since: now,
        })
    }

    /// How far short of the target the view is at `now`, and whether the glide has ended (at once
    /// with animations off).
    fn left(&self, now: Instant) -> (f32, bool) {
        let (t, done) = motion::SCROLL.at(now.saturating_duration_since(self.since));
        if done {
            (0., true)
        } else {
            (self.short * (1. - t), false)
        }
    }
}

/// How often something moving in the conversation asks to be drawn again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Redraw {
    None,
    /// At the next tick of the 30 fps grid streamed text redraws on: what moves while Claude
    /// streams adds no frames of its own.
    Tick,
    /// Every frame: what the user did (a click, a message sent) — rare and watched.
    Frame,
}

/// A body unfolding (`open`) or folding away, from where it was (`from`, its shown share: a
/// fold reversed halfway goes on from there).
#[derive(Clone, Copy, Debug)]
struct Fold {
    since: Instant,
    open: bool,
    from: f32,
    redraw: Redraw,
}

impl Fold {
    /// The share of the body shown now, 0 → 1.
    fn shown(&self) -> f32 {
        let target = if self.open { 1. } else { 0. };
        motion::lerp(
            self.from,
            target,
            motion::progress(self.since, motion::SMOOTH),
        )
    }

    fn running(&self) -> bool {
        motion::running(self.since, motion::SMOOTH)
    }
}

/// Claude's text as Markdown blocks.
struct Parsed {
    /// The length of the text they were parsed from.
    len: usize,
    blocks: Rc<Vec<Block>>,
    /// Code blocks have their colors (done off the UI thread once the text is complete).
    highlighted: bool,
    highlighting: bool,
}

/// One item of the conversation's list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Item {
    /// The entry at this index.
    Entry(usize),
    /// Claude's minor steps in a row (reads, searches, thinking, task updates…): the entries at
    /// `start..end`; hidden ones among them aren't drawn.
    Group { start: usize, end: usize },
}

impl TranscriptState {
    pub fn new(chat: WeakEntity<ClaudeChat>) -> Self {
        let list = ListState::new(0, ListAlignment::Bottom, px(600.));
        let glide: Rc<Cell<Option<Glide>>> = Rc::default();
        // The "↓" button follows the scroll position; scrolling by hand stops a glide.
        list.set_scroll_handler({
            let glide = glide.clone();
            move |_, _, cx| {
                glide.set(None);
                chat.update(cx, |_, cx| cx.notify()).ok();
            }
        });
        Self {
            list,
            items: Vec::new(),
            fingerprints: Vec::new(),
            entry_count: 0,
            item_of: HashMap::new(),
            toggled: HashSet::new(),
            opened: HashMap::new(),
            unclipped: HashSet::new(),
            texts: HashMap::new(),
            images: HashMap::new(),
            selection: None,
            drawn: Default::default(),
            frame: Cell::new(0),
            copied: None,
            reveal: None,
            reveal_at_once: false,
            dirty: true,
            scope: None,
            search: None,
            revealed: None,
            shown: HashSet::new(),
            synced: false,
            newest: None,
            loading: false,
            arrived: HashMap::new(),
            folds: HashMap::new(),
            fold_heights: Rc::default(),
            live_group: None,
            tick_pending: Rc::default(),
            glide,
            jump_shown: Cell::new(false),
            jump_left: Cell::new(None),
        }
    }

    /// The conversation of the subagent of the Agent call `tool` (part 10.2).
    pub fn scoped(chat: WeakEntity<ClaudeChat>, tool: EntryId) -> Self {
        Self {
            scope: Some(tool),
            ..Self::new(chat)
        }
    }

    /// The entry of the search's current match: it shows opened and whole.
    fn search_current(&self) -> Option<EntryId> {
        self.search.as_ref()?.current.map(|(entry, _)| entry)
    }

    /// The session changed: the next frame compares the entries again.
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// The end of the conversation is in view: new content scrolls in.
    pub fn at_bottom(&self) -> bool {
        self.list.logical_scroll_top().item_ix >= self.list.item_count()
    }

    /// Scrolls the conversation so the entry `id` is in sight — a subagent's entry by its Agent
    /// call, a step by its group (opened) — when the chat is drawn next: the caller redraws it
    /// (`cx.notify()` on the chat). A click on a session tab's status, a match of the search
    /// (part 10.2).
    /// The view glides there ([`motion::SCROLL`]).
    pub fn reveal(&mut self, id: EntryId) {
        self.reveal = Some(id);
        self.reveal_at_once = false;
        self.dirty = true;
    }

    /// [`TranscriptState::reveal`] at once: the first match of a query being typed follows the
    /// keys.
    pub fn reveal_at_once(&mut self, id: EntryId) {
        self.reveal(id);
        self.reveal_at_once = true;
    }

    /// Like [`TranscriptState::reveal`], and the entry opens and shows highlighted for a moment:
    /// what a click on a background command or the session's status asked to see. The caller
    /// redraws the chat when the highlight should fade and end ([`REVEALED_HOLD`]).
    pub fn reveal_highlighted(&mut self, id: EntryId) -> Instant {
        let at = Instant::now();
        self.reveal(id);
        self.shown.insert(id);
        self.revealed = Some((id, at));
        at
    }

    /// The highlight of [`TranscriptState::reveal_highlighted`] at `at` is over.
    pub fn end_highlight(&mut self, at: Instant) {
        if self.revealed.is_some_and(|(_, since)| since == at) {
            self.revealed = None;
        }
    }

    /// How long the entry `id` has been highlighted, while it is.
    fn highlighted(&self, id: EntryId) -> Option<Duration> {
        self.revealed
            .filter(|(revealed, _)| *revealed == id)
            .map(|(_, at)| at.elapsed())
            .filter(|elapsed| *elapsed < REVEALED_HOLD + REVEALED_FADE)
    }

    /// Takes the view to the end ("Jump to Latest", the user's message sent while scrolled up),
    /// gliding there.
    pub fn scroll_to_bottom(&self) {
        if self.at_bottom() {
            self.glide.set(None);
            self.place(GlideTo::End, 0.);
            return;
        }
        let from = self.view_top();
        self.place(GlideTo::End, 0.);
        let target = self.view_top();
        self.glide_from(GlideTo::End, from, target);
    }

    /// The view's top, in pixels down the list as far as its items are measured.
    fn view_top(&self) -> f32 {
        // At the end the list keeps no position of its own: its top is a view above the end.
        let end = f32::from(self.list.max_offset_for_scrollbar().height);
        self.list_top().min(end).max(0.)
    }

    /// The list's scroll position in pixels down the list, as it was set: past the end when it
    /// was set there (the list draws it at the end).
    fn list_top(&self) -> f32 {
        -f32::from(self.list.scroll_px_offset_for_scrollbar().y)
    }

    /// Puts the list `short` pixels short of `to` (above it when positive).
    fn place(&self, to: GlideTo, short: f32) {
        match to {
            GlideTo::End => {
                self.list.scroll_to(ListOffset {
                    item_ix: self.list.item_count(),
                    offset_in_item: px(0.),
                });
                if short > 0. {
                    let view = f32::from(self.list.viewport_bounds().size.height);
                    self.list.scroll_by(px(-(view + short)));
                }
            }
            GlideTo::Place(item_ix, offset_in_item) => {
                self.list.scroll_to(ListOffset {
                    item_ix,
                    offset_in_item,
                });
                self.list.scroll_by(px(-short));
            }
        }
    }

    /// The list is at `to` (whose top is at `target`): it glides there from the view's top at
    /// `from` when animations are on.
    fn glide_from(&self, to: GlideTo, from: f32, target: f32) {
        let limit = f32::from(self.list.viewport_bounds().size.height);
        let glide = (motion::enabled() && limit > 0.)
            .then(|| Glide::new(to, from, target, limit, Instant::now()))
            .flatten();
        if let Some(glide) = glide {
            self.place(to, glide.short);
        }
        self.glide.set(glide);
    }

    /// This frame's step of a glide: the list goes where the glide has it now. Taken before the
    /// list learns what changed, while the heights it knows are those it has just drawn.
    fn step_glide(&self) {
        let Some(glide) = self.glide.get() else {
            return;
        };
        let (short, done) = glide.left(Instant::now());
        self.place(glide.to, short);
        if done {
            self.glide.set(None);
        }
    }

    /// The view glides to the end: the "↓" button has done its job.
    fn gliding_to_end(&self) -> bool {
        self.glide
            .get()
            .is_some_and(|glide| glide.to == GlideTo::End)
    }

    fn expanded(&self, id: EntryId, default: bool) -> bool {
        if self.search_current() == Some(id) {
            return true;
        }
        let default = default || self.shown.contains(&id);
        (default || expand_all()) != self.toggled.contains(&id)
    }

    /// Flips the expansion of `id`, which was `expanded`: its body unfolds or folds away.
    fn toggle(&mut self, id: EntryId, expanded: bool) {
        if !self.toggled.remove(&id) {
            self.toggled.insert(id);
        }
        self.fold(id, !expanded, Redraw::Frame);
        self.dirty = true;
    }

    /// The body of `id` starts to unfold (`open`) or fold away — from where a fold still running
    /// has it.
    fn fold(&mut self, id: EntryId, open: bool, redraw: Redraw) {
        self.folds.retain(|_, fold| fold.running());
        if !motion::enabled() {
            return;
        }
        let from = match self.folds.get(&id) {
            Some(fold) => fold.shown(),
            None => {
                // Measured anew: the body may have grown since it was last folded.
                self.fold_heights.borrow_mut().remove(&id);
                if open { 0. } else { 1. }
            }
        };
        self.folds.insert(
            id,
            Fold {
                since: Instant::now(),
                open,
                from,
                redraw,
            },
        );
    }

    /// The fold of the body of `id`, while it runs toward `open`.
    fn folding(&self, id: EntryId, open: bool) -> Option<Fold> {
        self.folds
            .get(&id)
            .copied()
            .filter(|fold| fold.open == open && fold.running())
    }

    /// Since when the entry `id` appears, while it does.
    fn arriving(&self, id: EntryId, curve: Curve) -> Option<Instant> {
        self.arrived
            .get(&id)
            .copied()
            .filter(|since| motion::running(*since, curve))
    }

    /// A group of steps (by its first entry) is open: as the user left it, else by `default`.
    fn group_open(&self, id: EntryId, default: bool) -> bool {
        expand_all() || self.opened.get(&id).copied().unwrap_or(default)
    }

    fn just_copied(&self, id: EntryId) -> bool {
        self.copied
            .is_some_and(|(copied, at)| copied == id && at.showing())
    }
}

/// Brings the list up to date with the session: new and changed items are measured again,
/// changed texts parsed, complete ones highlighted in the background, new pictures decoded.
pub fn sync(
    state: &mut TranscriptState,
    session: &Entity<ClaudeSession>,
    cx: &mut Context<ClaudeChat>,
) {
    state.step_glide();
    if !state.dirty {
        return;
    }
    state.dirty = false;
    let mut texts = Vec::new();
    let mut pictures = Vec::new();
    let (fingerprints, user_sent, reveal) = {
        let model = session.read(cx).model();
        let entries = scope_entries(model, state.scope);
        let items = items(entries);
        state.item_of = item_index(entries, &items);
        // The entry to reveal opens the group that holds it before the items are measured.
        let reveal = state.reveal.take().and_then(|id| {
            let index = *state.item_of.get(&id)?;
            if let Item::Group { start, .. } = items[index] {
                state.opened.insert(entries[start].id, true);
            }
            Some(index)
        });
        let working = match state.scope {
            None => model.status.is_working(),
            Some(tool) => find_tool(&model.entries, tool).is_some_and(agent_running),
        };
        note_arrivals(state, entries, session.read(cx).is_loading());
        note_live_group(state, entries, &items, working);
        let fingerprints: Vec<u64> = (0..items.len())
            .map(|index| item_fingerprint(entries, &items, index, working, state))
            .collect();
        collect_texts(entries, &state.texts, &mut texts);
        for entry in entries {
            if let EntryKind::User(user) = &entry.kind {
                for (index, image) in user.images.iter().enumerate() {
                    if !state.images.contains_key(&(entry.id, index)) {
                        pictures.push((
                            (entry.id, index),
                            image.media_type.clone(),
                            image.data.clone(),
                        ));
                    }
                }
            }
        }
        // The user's own new message (or `!` command) brings the view to the end.
        let user_sent = entries[state.entry_count.min(entries.len())..]
            .iter()
            .any(|entry| matches!(entry.kind, EntryKind::User(_) | EntryKind::Shell(_)));
        state.entry_count = entries.len();
        state.items = items;
        (fingerprints, user_sent, reveal)
    };

    // Measure changed items again; new ones are added at the end.
    let old = std::mem::take(&mut state.fingerprints);
    let common = old.len().min(fingerprints.len());
    let mut index = 0;
    while index < common {
        if old[index] == fingerprints[index] {
            index += 1;
            continue;
        }
        let start = index;
        while index < common && old[index] != fingerprints[index] {
            index += 1;
        }
        state.list.splice(start..index, index - start);
    }
    if fingerprints.len() > old.len() {
        state
            .list
            .splice(old.len()..old.len(), fingerprints.len() - old.len());
    } else if fingerprints.len() < old.len() {
        state.list.splice(fingerprints.len()..old.len(), 0);
    }
    state.fingerprints = fingerprints;
    if user_sent {
        state.scroll_to_bottom();
    }
    let at_once = std::mem::take(&mut state.reveal_at_once);
    if let Some(index) = reveal {
        if at_once {
            state.glide.set(None);
        }
        // Not flush with the edge: a little of what's around shows too.
        let from = state.view_top();
        let top = state.list.logical_scroll_top();
        state.list.scroll_to_reveal_item(index);
        let moved = state.list.logical_scroll_top();
        if (moved.item_ix, moved.offset_in_item) != (top.item_ix, top.offset_in_item) {
            state.list.scroll_by(px(if index <= top.item_ix {
                -REVEAL_MARGIN
            } else {
                REVEAL_MARGIN
            }));
            // A place past the end (the margin under the last entry) is drawn at the end: the
            // glide toward it stops there a little early.
            if !at_once {
                let to = state.list.logical_scroll_top();
                let target = state.list_top();
                state.glide_from(GlideTo::Place(to.item_ix, to.offset_in_item), from, target);
            }
        }
    }

    for (key, media_type, data) in pictures {
        state.images.insert(
            key,
            Arc::new(Image::from_bytes(image_format(&media_type), data)),
        );
    }

    let scopes = if texts.iter().any(|text| !text.streaming) {
        markdown::scopes(cx)
    } else {
        Vec::new()
    };
    for text in texts {
        let blocks = markdown::parse(&text.text);
        let has_code = blocks.iter().any(|block| matches!(block, Block::Code(_)));
        let highlight = !text.streaming && has_code;
        state.texts.insert(
            text.id,
            Parsed {
                len: text.text.len(),
                blocks: Rc::new(blocks.clone()),
                highlighted: !has_code,
                highlighting: highlight,
            },
        );
        if highlight {
            let (id, len, scopes) = (text.id, text.text.len(), scopes.clone());
            cx.spawn(async move |chat, cx| {
                let highlighted = cx
                    .background_executor()
                    .spawn(async move {
                        let mut blocks = blocks;
                        markdown::highlight(&mut blocks, None, &scopes);
                        blocks
                    })
                    .await;
                chat.update(cx, |chat, cx| {
                    // The session's view and a subagent's may both show the text.
                    let highlighted = Rc::new(highlighted);
                    let mut done = false;
                    for transcript in chat.transcripts_mut() {
                        if let Some(parsed) = transcript.texts.get_mut(&id)
                            && parsed.len == len
                        {
                            parsed.blocks = highlighted.clone();
                            parsed.highlighted = true;
                            parsed.highlighting = false;
                            done = true;
                        }
                    }
                    if done {
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
    }
}

/// Notes the entries that arrived since the last sync, so they appear. Not arrivals: what the
/// first sync finds, a saved conversation being read or joining this one (it comes first, under
/// new ids — before what was known), a batch (a conversation loading).
fn note_arrivals(state: &mut TranscriptState, entries: &[Entry], loading: bool) {
    let newest = state.newest;
    let is_new = |entry: &Entry| newest.is_none_or(|newest| entry.id > newest);
    state
        .arrived
        .retain(|_, since| motion::running(*since, motion::BOUNCE));
    if state.synced && !state.loading && !loading && motion::enabled() {
        let known = entries.iter().position(|entry| !is_new(entry));
        let fresh: Vec<EntryId> = entries
            .iter()
            .enumerate()
            .filter(|(index, entry)| is_new(entry) && known.is_none_or(|known| *index > known))
            .map(|(_, entry)| entry.id)
            .collect();
        if fresh.len() <= MAX_ARRIVALS {
            let now = Instant::now();
            state.arrived.extend(fresh.into_iter().map(|id| (id, now)));
        }
    }
    state.synced = true;
    state.loading = loading;
    state.newest = entries.iter().map(|entry| entry.id).max().max(newest);
}

/// The group open while its turn runs closes by itself once something follows it or the turn
/// ends: it folds away — on the tick, as that happens while Claude streams.
fn note_live_group(state: &mut TranscriptState, entries: &[Entry], items: &[Item], working: bool) {
    let live = match items.last() {
        Some(Item::Group { start, .. }) if working => Some(entries[*start].id),
        _ => None,
    };
    if let Some(previous) = state.live_group.filter(|previous| live != Some(*previous))
        && !state.opened.contains_key(&previous)
    {
        state.fold(previous, false, Redraw::Tick);
    }
    state.live_group = live;
}

/// Draws the conversation again at the next tick of the 30 fps grid the streamed text redraws on
/// (`ClaudeSession::redraw_on_tick`) — once, however many of its rows ask.
fn request_tick(chat: &WeakEntity<ClaudeChat>, pending: &Rc<Cell<bool>>, cx: &mut App) {
    if pending.replace(true) {
        return;
    }
    let (chat, pending) = (chat.clone(), pending.clone());
    let wait = gpui::until_next_animation_tick();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(wait).await;
        pending.set(false);
        chat.update(cx, |_, cx| cx.notify()).ok();
    })
    .detach();
}

/// The list's items over `entries`: one per shown entry, Claude's minor steps in a row as one
/// group. Hidden entries (thinking without text, an empty text) belong to no item and don't break
/// a group. A lone minor step is its own item: a group of one thought would be «Thought» opening
/// to another «Thought» (the author, after 10.2).
pub(crate) fn items(entries: &[Entry]) -> Vec<Item> {
    /// The group being gathered: its first and past-the-last entries, how many it shows.
    struct Open {
        start: usize,
        end: usize,
        shown: usize,
    }
    fn close(open: Open, items: &mut Vec<Item>) {
        items.push(match open.shown {
            1 => Item::Entry(open.start),
            _ => Item::Group {
                start: open.start,
                end: open.end,
            },
        });
    }
    let mut items = Vec::new();
    let mut group: Option<Open> = None;
    for (index, entry) in entries.iter().enumerate() {
        if is_hidden(&entry.kind) {
            continue;
        }
        if is_minor(&entry.kind) {
            group = Some(match group {
                Some(open) => Open {
                    end: index + 1,
                    shown: open.shown + 1,
                    ..open
                },
                None => Open {
                    start: index,
                    end: index + 1,
                    shown: 1,
                },
            });
            continue;
        }
        if let Some(open) = group.take() {
            close(open, &mut items);
        }
        items.push(Item::Entry(index));
    }
    if let Some(open) = group {
        close(open, &mut items);
    }
    items
}

/// Which item shows each entry; a subagent's entries (at any depth) go by their Agent call's.
fn item_index(entries: &[Entry], items: &[Item]) -> HashMap<EntryId, usize> {
    fn nested(children: &[Entry], item: usize, index: &mut HashMap<EntryId, usize>) {
        for child in children {
            index.insert(child.id, item);
            if let EntryKind::Tool(tool) = &child.kind {
                nested(&tool.children, item, index);
            }
        }
    }
    let mut index = HashMap::new();
    for (item, range) in items.iter().map(|item| item.range()).enumerate() {
        for entry in entries.get(range).into_iter().flatten() {
            index.insert(entry.id, item);
            if let EntryKind::Tool(tool) = &entry.kind {
                nested(&tool.children, item, &mut index);
            }
        }
    }
    index
}

/// The entries a transcript shows: the session's conversation, or the subagent's of the Agent call
/// `scope` (nothing, if the call is gone).
pub(crate) fn scope_entries(model: &Session, scope: Option<EntryId>) -> &[Entry] {
    match scope {
        None => &model.entries,
        Some(id) => find_tool(&model.entries, id).map_or(&[], |tool| tool.children.as_slice()),
    }
}

/// The tool call with the entry id `id`, at any depth (subagents' calls too).
pub(crate) fn find_tool(entries: &[Entry], id: EntryId) -> Option<&ToolEntry> {
    entries.iter().find_map(|entry| match &entry.kind {
        EntryKind::Tool(tool) if entry.id == id => Some(tool.as_ref()),
        EntryKind::Tool(tool) => find_tool(&tool.children, id),
        _ => None,
    })
}

impl Item {
    /// The entries it shows.
    fn range(self) -> std::ops::Range<usize> {
        match self {
            Item::Entry(index) => index..index + 1,
            Item::Group { start, end } => start..end,
        }
    }

    /// The entry, when the item is one.
    fn kind(self, entries: &[Entry]) -> Option<&EntryKind> {
        match self {
            Item::Entry(index) => entries.get(index).map(|entry| &entry.kind),
            Item::Group { .. } => None,
        }
    }
}

/// A step that folds into a group: thinking, and the tools that only look around (read, search,
/// fetch) or keep notes (tasks), the skills and other tools' calls.
fn is_minor(kind: &EntryKind) -> bool {
    match kind {
        EntryKind::Thinking { .. } => true,
        EntryKind::Tool(tool) => matches!(
            tool.call,
            ToolCall::Read { .. }
                | ToolCall::Grep { .. }
                | ToolCall::Glob { .. }
                | ToolCall::WebFetch { .. }
                | ToolCall::WebSearch { .. }
                | ToolCall::TaskCreate { .. }
                | ToolCall::TaskUpdate { .. }
                | ToolCall::TodoWrite { .. }
                | ToolCall::Skill { .. }
                | ToolCall::Mcp { .. }
                | ToolCall::EnterPlanMode
                | ToolCall::Other
        ),
        _ => false,
    }
}

/// The user's side of the conversation: their messages and `!` commands.
fn is_user_side(kind: &EntryKind) -> bool {
    matches!(kind, EntryKind::User(_) | EntryKind::Shell(_))
}

/// The item starts a turn of Claude's — after the user's message, a turn's end or a compaction —
/// and gets the avatar. The end of a turn and a separator never carry it.
fn leads_turn(entries: &[Entry], items: &[Item], index: usize) -> bool {
    let carries = match items[index].kind(entries) {
        None => true,
        Some(kind) => !is_user_side(kind) && !is_turn_break(kind),
    };
    carries
        && match index.checked_sub(1) {
            None => true,
            Some(previous) => items[previous]
                .kind(entries)
                .is_some_and(|kind| is_user_side(kind) || is_turn_break(kind)),
        }
}

/// The end of a turn, or a line across the conversation (a compaction).
fn is_turn_break(kind: &EntryKind) -> bool {
    matches!(
        kind,
        EntryKind::TurnEnd(_) | EntryKind::Notice(Notice::Compacted { .. })
    )
}

/// The space above an item: wide between the user's messages and Claude's turns, tight within a
/// turn.
fn top_gap(entries: &[Entry], items: &[Item], index: usize) -> f32 {
    let Some(previous) = index.checked_sub(1).map(|previous| items[previous]) else {
        return 14.;
    };
    let user = |item: Item| item.kind(entries).is_some_and(is_user_side);
    let current = items[index];
    match (user(previous), user(current)) {
        (false, true) => 20.,
        (true, true) => 10.,
        (true, false) => 12.,
        (false, false) if previous.kind(entries).is_some_and(is_turn_break) => 18.,
        (false, false) => 8.,
    }
}

/// What an item looks like, cheaply: when it changes, the list measures the item again.
fn item_fingerprint(
    entries: &[Entry],
    items: &[Item],
    index: usize,
    working: bool,
    state: &TranscriptState,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    leads_turn(entries, items, index).hash(&mut hasher);
    top_gap(entries, items, index).to_bits().hash(&mut hasher);
    if let Some(search) = &state.search {
        (&search.needle, search.current).hash(&mut hasher);
    }
    match items[index] {
        Item::Entry(entry) => hash_entry(&entries[entry], state, &mut hasher),
        Item::Group { start, end } => {
            7u8.hash(&mut hasher);
            (working && index + 1 == items.len()).hash(&mut hasher);
            state.opened.get(&entries[start].id).hash(&mut hasher);
            for entry in &entries[start..end] {
                hash_entry(entry, state, &mut hasher);
            }
        }
    }
    hasher.finish()
}

/// A text of Claude's to parse.
struct TextJob {
    id: EntryId,
    text: String,
    streaming: bool,
}

/// Claude's texts and thinking (subagents' too) that changed since they were parsed, and complete
/// ones not highlighted yet.
fn collect_texts(entries: &[Entry], parsed: &HashMap<EntryId, Parsed>, jobs: &mut Vec<TextJob>) {
    for entry in entries {
        let (text, streaming) = match &entry.kind {
            EntryKind::Text { text, streaming }
            | EntryKind::Thinking {
                text, streaming, ..
            } => (text, *streaming),
            EntryKind::Tool(tool) => {
                collect_texts(&tool.children, parsed, jobs);
                continue;
            }
            _ => continue,
        };
        let stale = match parsed.get(&entry.id) {
            None => true,
            Some(parsed) => {
                parsed.len != text.len()
                    || (!streaming && !parsed.highlighted && !parsed.highlighting)
            }
        };
        if stale && !text.is_empty() {
            jobs.push(TextJob {
                id: entry.id,
                text: text.clone(),
                streaming,
            });
        }
    }
}

fn hash_entry(entry: &Entry, state: &TranscriptState, hasher: &mut DefaultHasher) {
    entry.id.hash(hasher);
    state.toggled.contains(&entry.id).hash(hasher);
    state.shown.contains(&entry.id).hash(hasher);
    state.unclipped.contains(&entry.id).hash(hasher);
    match &entry.kind {
        EntryKind::User(user) => (
            user.text.len(),
            user.images.len(),
            user.queued,
            user.cancelled,
            user.at,
        )
            .hash(hasher),
        EntryKind::Text { text, streaming } => (1u8, text.len(), streaming).hash(hasher),
        EntryKind::Thinking {
            text,
            tokens,
            streaming,
            duration,
            ..
        } => (2u8, text.len(), tokens, streaming, duration.is_some()).hash(hasher),
        EntryKind::Tool(tool) => {
            (
                3u8,
                tool.state as u8,
                tool.input.is_null(),
                tool.result.is_some(),
            )
                .hash(hasher);
            if let Some(task) = &tool.task {
                (
                    task.tokens,
                    task.tool_uses,
                    &task.last_tool,
                    &task.status,
                    task.summary.is_some(),
                )
                    .hash(hasher);
            }
            tool.children.len().hash(hasher);
            for child in &tool.children {
                hash_entry(child, state, hasher);
            }
        }
        EntryKind::Notice(notice) => (4u8, std::mem::discriminant(notice)).hash(hasher),
        EntryKind::TurnEnd(summary) => (5u8, summary.is_error).hash(hasher),
        EntryKind::Shell(shell) => (
            6u8,
            shell.output.len(),
            shell.state as u8,
            shell.exit_code,
            shell.delivered,
        )
            .hash(hasher),
    }
}

/// UI scenarios can't click: `FLUX_SCENARIO_CLAUDE_EXPANDED=1` shows every row expanded.
#[cfg(feature = "scenario")]
fn expand_all() -> bool {
    std::env::var_os("FLUX_SCENARIO_CLAUDE_EXPANDED").is_some_and(|value| !value.is_empty())
}

#[cfg(not(feature = "scenario"))]
fn expand_all() -> bool {
    false
}

pub(crate) fn image_format(media_type: &str) -> ImageFormat {
    match media_type {
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::Webp,
        _ => ImageFormat::Png,
    }
}

// --- The view ---

/// The conversation's column around `child`: centered, at most [`COLUMN_MAX_WIDTH`] wide, inset
/// from the island's edges. The chat puts its cards and field in the same column.
pub fn column(child: impl IntoElement) -> Div {
    div().w_full().flex().justify_center().child(
        div()
            .w_full()
            .max_w(px(COLUMN_MAX_WIDTH))
            .px(px(COLUMN_INSET))
            .child(child),
    )
}

/// The conversation filling the chat (the "↓" button over it while scrolled up), and the line of
/// what Claude does now under it; tips in an empty session.
pub fn render(chat: &ClaudeChat, window: &mut Window, cx: &mut Context<ClaudeChat>) -> AnyElement {
    let ui = Theme::ui(cx);
    let weak = cx.weak_entity();
    let state = chat.shown_transcript();
    // A glide steps each frame (in `sync`) while it runs.
    if state.glide.get().is_some() {
        window.request_animation_frame();
    }
    state.frame.set(state.frame.get() + 1);
    claude_select::prune(&state.drawn, state.frame.get(), state.selection.as_ref());
    let model = chat.session().read(cx).model();
    let agent = state.scope.and_then(|tool| find_tool(&model.entries, tool));
    let activity = match agent {
        None => activity_line(model, ui),
        Some(tool) => agent_activity_line(tool, ui),
    };
    if let Some(tool) = agent
        && scope_entries(model, state.scope).is_empty()
    {
        // The subagent hasn't said anything yet: its task, and that it starts.
        return div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .justify_end()
            .child(column(agent_task(tool, ui)).pb(px(8.)))
            .children(activity)
            .into_any_element();
    }
    if model.entries.is_empty() {
        let starting = matches!(model.status, Status::Starting);
        return div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(empty_state(starting, ui))
            .children(activity)
            .into_any_element();
    }
    let jump_button = || {
        let weak = weak.clone();
        div().absolute().bottom(px(10.)).right(px(12.)).child(
            div()
                .id("claude-jump-to-latest")
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(14.))
                .bg(ui.elevated)
                .border_1()
                .border_color(ui.elevated_border)
                .shadow(ui::popover_shadow(ui))
                .cursor_pointer()
                .hover(move |style| style.bg(ui.hover))
                .active(motion::press)
                .tooltip(ui::tooltip(tr("Jump to Latest"), None))
                .child(icon(IconName::ArrowDown, ui.text_muted).size(px(14.)))
                .on_click(move |_, _, cx| {
                    weak.update(cx, |chat, cx| {
                        chat.shown_transcript().scroll_to_bottom();
                        cx.notify();
                    })
                    .ok();
                }),
        )
    };
    // It pops up where it sits as the user leaves the end; as the view comes back (or sets off
    // there after a click), it fades out into its corner, taking no input.
    let away = !state.at_bottom() && !state.gliding_to_end();
    if away {
        state.jump_left.set(None);
    } else if state.jump_shown.get() && motion::enabled() {
        state.jump_left.set(Some(Instant::now()));
    }
    state.jump_shown.set(away);
    let jump = if away {
        Some(Appear::pop((1., 1.)).apply("claude-jump-appear", jump_button()))
    } else {
        state
            .jump_left
            .get()
            .filter(|since| motion::running(*since, motion::EXIT))
            .map(|since| {
                let progress = motion::animate(since, motion::EXIT, window);
                motion::inert(motion::leaving_style(jump_button(), progress, (1., 1.)))
                    .into_any_element()
            })
    };
    let items = {
        let weak = weak.clone();
        list(state.list.clone(), move |index, window, cx| {
            render_item(&weak, index, window, cx)
        })
        .size_full()
    };
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .text_size(px(CHAT_TEXT))
        .child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(items)
                .children(jump)
                .child(claude_select::drag_listener(weak.clone())),
        )
        .children(activity)
        .into_any_element()
}

/// One item of the main conversation, in the column, with the space above it.
fn render_item(
    chat: &WeakEntity<ClaudeChat>,
    index: usize,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(entity) = chat.upgrade() else {
        return div().into_any_element();
    };
    let (element, redraw, pending) = item_element(&entity, chat, index, cx);
    match redraw {
        Redraw::Frame => window.request_animation_frame(),
        Redraw::Tick => request_tick(chat, &pending, cx),
        Redraw::None => {}
    }
    element
}

/// [`render_item`]'s element, and how soon what moves in it wants the next frame.
fn item_element(
    entity: &Entity<ClaudeChat>,
    chat: &WeakEntity<ClaudeChat>,
    index: usize,
    cx: &App,
) -> (AnyElement, Redraw, Rc<Cell<bool>>) {
    let chat_view = entity.read(cx);
    let session = chat_view.session().clone();
    let model = session.read(cx).model();
    let state = chat_view.shown_transcript();
    let agent = state.scope.and_then(|tool| find_tool(&model.entries, tool));
    let entries = scope_entries(model, state.scope);
    let pending = state.tick_pending.clone();
    let Some(item) = state
        .items
        .get(index)
        .copied()
        .filter(|item| item.range().end <= entries.len())
    else {
        return (div().into_any_element(), Redraw::None, pending);
    };
    let theme = Theme::get(cx);
    let renderer = Renderer {
        ui: theme.ui,
        theme,
        state,
        chat: chat.clone(),
        session: &session,
        model,
        root: &model.info.cwd,
        tasks: &model.tasks,
        agent: agent.is_some(),
        block: chat_view.region_refusal().map(|refusal| LiveBlock {
            refusal: refusal.clone(),
            checking: region_guard::verdict(cx) == Verdict::Checking,
        }),
        marks: RefCell::new(HashMap::new()),
        redraw: Cell::new(Redraw::None),
    };
    let lead = leads_turn(entries, &state.items, index);
    let last = index + 1 == state.items.len();
    let working = match agent {
        None => model.status.is_working(),
        Some(tool) => agent_running(tool),
    };
    let body = match item {
        Item::Entry(entry) => renderer.top_entry(&entries[entry], lead),
        Item::Group { start, end } => {
            let live = last && working;
            renderer.claude(lead, renderer.group(&entries[start..end], live))
        }
    };
    // A subagent's conversation starts with its task (part 10.2).
    let body = match agent.filter(|_| index == 0) {
        Some(tool) => div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(agent_task(tool, theme.ui))
            .child(body)
            .into_any_element(),
        None => body,
    };
    let body = renderer.arrival(&entries[item.range().start], body);
    let element = column(body)
        .pt(px(top_gap(entries, &state.items, index)))
        .when(last, |item| item.pb(px(12.)))
        .into_any_element();
    (element, renderer.redraw.get(), pending)
}

/// Thinking that is over with nothing to show — no text (the CLI didn't summarize it) and no
/// known time — and an empty text: no row, the activity line said it while it lasted.
fn is_hidden(kind: &EntryKind) -> bool {
    match kind {
        EntryKind::Thinking {
            text,
            streaming: false,
            duration,
            ..
        } => text.trim().is_empty() && duration.is_none_or(|duration| duration.as_secs() == 0),
        EntryKind::Text {
            text,
            streaming: false,
        } => text.trim().is_empty(),
        _ => false,
    }
}

/// The block of the country check in force (part «Флаг страны»): why, and whether the country is
/// being checked again.
struct LiveBlock {
    refusal: region_guard::Refusal,
    checking: bool,
}

/// Draws entries; borrows what it needs for one frame.
struct Renderer<'a> {
    ui: UiColors,
    theme: &'a Theme,
    state: &'a TranscriptState,
    chat: WeakEntity<ClaudeChat>,
    session: &'a Entity<ClaudeSession>,
    model: &'a Session,
    root: &'a Path,
    /// Claude's task list: a task update names its task.
    tasks: &'a [flux_claude::TaskItem],
    /// A subagent's conversation (its avatar instead of Claude's).
    agent: bool,
    /// The block of the country check in force: the last note of a block is live while it lasts.
    block: Option<LiveBlock>,
    /// The search's marks by entry, made on first use: an entry's texts share its count.
    marks: RefCell<HashMap<EntryId, Rc<Marks>>>,
    /// How soon what moves in the item wants the next frame.
    redraw: Cell<Redraw>,
}

impl Renderer<'_> {
    /// Something in the item moves: it is drawn again as `redraw` says.
    fn animating(&self, redraw: Redraw) {
        self.redraw.set(self.redraw.get().max(redraw));
    }

    /// The item of `entry`, appearing while the entry has just arrived: the user's message rises
    /// from the field below, growing out of its corner by the field (`BOUNCE`); the "Done" card
    /// pops (`POP`); Claude's other blocks fade in rising a little (`SMOOTH`, on the tick).
    fn arrival(&self, entry: &Entry, body: AnyElement) -> AnyElement {
        let (appear, curve, redraw) = match &entry.kind {
            EntryKind::User(_) | EntryKind::Shell(_) => (
                Appear::slide(0., 16.).scale(0.96).origin(1., 1.),
                motion::BOUNCE,
                Redraw::Frame,
            ),
            EntryKind::TurnEnd(_) if !self.model.turn_changes(entry.id).is_empty() => (
                Appear::pop((0.5, 1.)).scale(0.95),
                motion::POP,
                Redraw::Frame,
            ),
            _ => (
                Appear::slide(0., 8.).curve(motion::SMOOTH),
                motion::SMOOTH,
                Redraw::Tick,
            ),
        };
        match self.state.arriving(entry.id, curve) {
            Some(since) => {
                self.animating(redraw);
                appear
                    .style(div().child(body), motion::progress(since, curve))
                    .into_any_element()
            }
            None => body,
        }
    }

    /// A step that joined an open group fades in, rising a little — not the group's first, which
    /// came with the group.
    fn arriving_step(&self, entry: &Entry, row: AnyElement) -> AnyElement {
        match self.state.arriving(entry.id, motion::SMOOTH) {
            Some(since) => {
                self.animating(Redraw::Tick);
                Appear::slide(0., 6.)
                    .style(div().child(row), motion::progress(since, motion::SMOOTH))
                    .into_any_element()
            }
            None => row,
        }
    }

    /// The body of a row, a card or a group that opens and closes (`open` now): shown while open;
    /// for a moment after it flips, it unfolds — its clip glides from nothing to the body's height
    /// (`SMOOTH`) as it fades in — or folds away, taking no input.
    fn fold(&self, id: EntryId, open: bool, body: Option<AnyElement>) -> Option<AnyElement> {
        let Some(fold) = self.state.folding(id, open) else {
            return body.filter(|_| open);
        };
        let body = body?;
        self.animating(fold.redraw);
        let shown = fold.shown();
        let heights = self.state.fold_heights.clone();
        let natural = heights.borrow().get(&id).copied();
        let opacity = if open { (shown / 0.6).min(1.) } else { shown };
        let clip = div()
            .overflow_hidden()
            .opacity(opacity.clamp(0., 1.))
            .on_children_prepainted(move |bounds, _, _| {
                if let Some(bounds) = bounds.first() {
                    heights.borrow_mut().insert(id, bounds.size.height);
                }
            });
        // The first frame measures the body: hidden when it opens, whole when it folds.
        let clip = match natural {
            Some(height) => clip.h(height * shown.max(0.)),
            None if open => clip.h(px(0.)),
            None => clip,
        };
        let clip = clip.child(body);
        Some(if open {
            clip.into_any_element()
        } else {
            motion::inert(clip).into_any_element()
        })
    }

    /// A top-level entry on its side of the conversation.
    fn top_entry(&self, entry: &Entry, lead: bool) -> AnyElement {
        match &entry.kind {
            EntryKind::User(user) => self.user(entry.id, user),
            EntryKind::Shell(shell) => self.shell(entry.id, shell),
            EntryKind::Notice(notice @ Notice::Compacted { .. }) => self.notice(entry.id, notice),
            _ => self.claude(lead, self.entry(entry, 0)),
        }
    }

    /// Claude's side: the avatar's column (the avatar where a turn starts), then the content.
    fn claude(&self, lead: bool, content: AnyElement) -> AnyElement {
        div()
            .flex()
            .gap(px(AVATAR_GAP))
            .child(div().flex_none().w(px(AVATAR)).children(lead.then(|| {
                if self.agent {
                    agent_avatar(self.ui)
                } else {
                    avatar(self.ui)
                }
            })))
            .child(div().flex_1().min_w_0().child(content))
            .into_any_element()
    }

    /// The search's marks for the texts of the entry `id` (none while nothing is searched).
    fn marks(&self, id: EntryId) -> Option<Rc<Marks>> {
        let search = self
            .state
            .search
            .as_ref()
            .filter(|search| !search.needle.is_empty())?;
        let mut marks = self.marks.borrow_mut();
        let marks = marks.entry(id).or_insert_with(|| {
            Rc::new(Marks {
                needle: search.needle.clone(),
                active: search
                    .current
                    .filter(|(entry, _)| *entry == id)
                    .map(|(_, index)| index),
                count: Cell::new(0),
                color: self.ui.search_match,
                active_color: self.ui.search_match_active,
            })
        });
        Some(marks.clone())
    }

    /// What makes the texts of the entry `id` selectable with the mouse (`claude_select`).
    fn select(&self, id: EntryId) -> Collect {
        Collect::new(
            id,
            self.state.frame.get(),
            self.state.selection.as_ref(),
            self.ui.selection,
            self.state.drawn.clone(),
        )
    }

    /// A line of text with the search's matches in it marked.
    fn marked(&self, id: EntryId, text: String) -> AnyElement {
        match self.marks(id) {
            Some(marks) => {
                let highlights = marks.highlights(&text);
                if highlights.is_empty() {
                    text.into_any_element()
                } else {
                    StyledText::new(text)
                        .with_highlights(highlights)
                        .into_any_element()
                }
            }
            None => text.into_any_element(),
        }
    }

    fn entry(&self, entry: &Entry, depth: usize) -> AnyElement {
        match &entry.kind {
            EntryKind::User(user) => self.user(entry.id, user),
            EntryKind::Text { text, streaming } => self.text(entry.id, text, *streaming),
            EntryKind::Thinking {
                text,
                tokens,
                streaming,
                duration,
                ..
            } => self.thinking(entry.id, text, *tokens, *streaming, *duration),
            EntryKind::Tool(tool) => self.tool_entry(entry.id, tool, depth),
            EntryKind::Notice(notice) => self.notice(entry.id, notice),
            EntryKind::TurnEnd(summary) => self.turn_end(entry.id, summary),
            EntryKind::Shell(shell) => self.shell(entry.id, shell),
        }
    }

    /// A tool call that isn't a minor step: a card for a command, an edit, a subagent; a row with
    /// its details for the rest (a question, the plan).
    fn tool_entry(&self, id: EntryId, tool: &ToolEntry, depth: usize) -> AnyElement {
        let element = match &tool.call {
            ToolCall::Bash { .. } => self.bash_card(id, tool),
            ToolCall::Edit { .. } | ToolCall::Write { .. } | ToolCall::NotebookEdit { .. } => {
                self.edit_card(id, tool)
            }
            ToolCall::Agent { .. } if depth == 0 => self.agent_card(id, tool),
            _ => self.tool(id, tool, depth),
        };
        match self.state.highlighted(id) {
            Some(elapsed) => self.highlight(id, element, elapsed),
            None => element,
        }
    }

    /// An entry a click brought into sight: an accent frame over it, which fades after a moment.
    fn highlight(&self, id: EntryId, element: AnyElement, elapsed: Duration) -> AnyElement {
        let ui = self.ui;
        let frame = div()
            .absolute()
            .top(px(-3.))
            .bottom(px(-3.))
            .left(px(-3.))
            .right(px(-3.))
            .rounded(px(CARD_RADIUS + 3.))
            .border_2()
            .border_color(UiColors::tint(ui.accent, 0.85))
            .bg(UiColors::tint(ui.accent, 0.07));
        let frame = if elapsed < REVEALED_HOLD {
            frame.into_any_element()
        } else {
            frame
                .with_animation(
                    ("claude-revealed", id),
                    Animation::new(REVEALED_FADE),
                    |frame, delta| frame.opacity(1. - delta),
                )
                .into_any_element()
        };
        div()
            .relative()
            .flex()
            .flex_col()
            .child(element)
            .child(frame)
            .into_any_element()
    }

    // --- The user's side ---

    /// The user's message: a plugin's attachments, mentions at its ends and pictures as chips
    /// above a bubble on the right, the time (or Queued, Cancelled) under it, Copy / Edit and
    /// resend / Rewind the code on hover.
    fn user(&self, id: EntryId, user: &UserEntry) -> AnyElement {
        let ui = self.ui;
        let (attachments, text) = leading_attachments(&user.text);
        let (chips, text) = edge_mentions(text);
        let thumbnails: Vec<AnyElement> = (0..user.images.len())
            .filter_map(|index| {
                let image = self.state.images.get(&(id, index))?.clone();
                let attachment = user.images.get(index)?;
                let source = ImageSource::Bytes {
                    name: format!(
                        "image-{}.{}",
                        index + 1,
                        image_extension(&attachment.media_type)
                    ),
                    media_type: attachment.media_type.clone(),
                    data: Arc::from(attachment.data.as_slice()),
                };
                let chat = self.chat.clone();
                Some(
                    div()
                        .id(("claude-user-image", (id as usize) * 16 + index))
                        .w(px(64.))
                        .h(px(48.))
                        .rounded(px(RADIUS_SM))
                        .overflow_hidden()
                        .border_1()
                        .border_color(UiColors::tint(ui.foreground, 0.14))
                        .cursor_pointer()
                        .hover(move |style| style.border_color(ui.accent))
                        .tooltip(ui::tooltip(tr("Preview"), None))
                        .child(img(image).size_full().object_fit(gpui::ObjectFit::Cover))
                        .on_click(move |_, _, cx| {
                            let source = source.clone();
                            chat.update(cx, |chat, cx| chat.preview_image(source, cx))
                                .ok();
                        })
                        .into_any_element(),
                )
            })
            .collect();
        let chips: Vec<AnyElement> = attachments
            .into_iter()
            .enumerate()
            .map(|(index, block)| self.attachment_chip(id, index, block))
            .chain(
                chips
                    .into_iter()
                    .enumerate()
                    .map(|(index, chip)| self.mention_chip(id, index, chip)),
            )
            .collect();
        let above = (!chips.is_empty() || !thumbnails.is_empty()).then(|| {
            div()
                .max_w(gpui::relative(BUBBLE_SHARE))
                .flex()
                .flex_wrap()
                .justify_end()
                .items_end()
                .gap_1p5()
                .children(chips)
                .children(thumbnails)
        });
        let bubble = (!text.trim().is_empty()).then(|| {
            div()
                .max_w(gpui::relative(BUBBLE_SHARE))
                .px(px(14.))
                .pt(px(8.))
                .pb(px(9.))
                .rounded(px(16.))
                .rounded_br(px(5.))
                .bg(UiColors::tint(ui.accent, 0.17))
                .border_1()
                .border_color(UiColors::tint(ui.accent, 0.26))
                .cursor_text()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    claude_select::mouse_down(self.chat.clone(), id),
                )
                .child(self.user_text(id, text, user.cancelled))
        });
        let status = if user.cancelled {
            Some(tr("Cancelled").to_string())
        } else if user.queued {
            Some(tr("Queued").to_string())
        } else {
            user.at.map(time_label)
        };
        let cancel = user
            .uuid
            .clone()
            .filter(|_| user.queued)
            .map(|uuid: String| {
                let session = self.session.clone();
                small_button(("claude-cancel-queued", id), IconName::Close, ui)
                    .tooltip(ui::tooltip(tr("Don't Send"), None))
                    .on_click(move |_, _, cx| {
                        session.update(cx, |session, cx| session.cancel_queued(&uuid, cx))
                    })
            });
        let group: SharedString = format!("claude-user-{id}").into();
        let actions = (!user.queued && !user.cancelled).then(|| {
            let prompt = !user.text.trim_start().starts_with('/');
            let copied = self.state.just_copied(id);
            let chat = self.chat.clone();
            let copy = small_button_frame(("claude-user-copy", id), ui)
                .child(copy_icon(copied, 13., ui.text_muted, ui.text_muted))
                .tooltip(ui::tooltip(tr("Copy"), None))
                .on_click(move |_, _, cx| {
                    chat.update(cx, |chat, cx| {
                        chat.copy_message(id, cx);
                        note_copied(chat, id, cx);
                    })
                    .ok();
                });
            let edit = prompt.then(|| {
                let chat = self.chat.clone();
                small_button(("claude-user-edit", id), IconName::Pencil, ui)
                    .tooltip(ui::tooltip(tr("Edit and Resend"), None))
                    .on_click(move |_, window, cx| {
                        chat.update(cx, |chat, cx| chat.edit_message(id, window, cx))
                            .ok();
                    })
            });
            let rewind = prompt.then(|| {
                let chat = self.chat.clone();
                small_button(("claude-user-rewind", id), IconName::Rollback, ui)
                    .tooltip(ui::tooltip(tr("Rewind Code to Before This Message"), None))
                    .on_click(move |_, window, cx| {
                        chat.update(cx, |chat, cx| chat.rewind_code(id, window, cx))
                            .ok();
                    })
            });
            // They fade in under the pointer (an id keeps the fade).
            div()
                .id(("claude-user-actions", id))
                .flex()
                .items_center()
                .gap(px(2.))
                .when(!copied, |row| {
                    row.opacity(0.)
                        .group_hover(group.clone(), |style| style.opacity(1.))
                })
                .child(copy)
                .children(edit)
                .children(rewind)
        });
        let footer = div()
            .h(px(20.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(theme::TEXT_XS))
            .text_color(ui.dim)
            .children(actions)
            .when(user.queued, |row| {
                row.child(icon(IconName::Clock, ui.dim).size(px(12.)))
            })
            .children(status)
            .children(cancel);
        div()
            .group(group.clone())
            .flex()
            .flex_col()
            .items_end()
            .gap(px(5.))
            .when(user.queued || user.cancelled, |column| column.opacity(0.65))
            .children(above)
            .children(bubble)
            .child(footer)
            .into_any_element()
    }

    /// A mention at an end of the message, as a chip: the file's icon, its name and lines; a
    /// click opens it there.
    fn mention_chip(&self, id: EntryId, index: usize, chip: Mention<'_>) -> AnyElement {
        let ui = self.ui;
        let name = Path::new(chip.path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(chip.path)
            .to_string();
        let label = match chip.lines {
            Some((first, last)) if first == last => format!("{name}:{first}"),
            Some((first, last)) => format!("{name}:{first}–{last}"),
            None => name.clone(),
        };
        let location = Location {
            path: self.root.join(chip.path),
            line: chip.lines.map_or(0, |(first, _)| first.saturating_sub(1)),
            start: 0,
            end: 0,
        };
        let chat = self.chat.clone();
        let file = file_icon(&name, &ui);
        div()
            .id(("claude-mention-chip", (id as usize) * 16 + index))
            .h(px(22.))
            .px(px(7.))
            .flex()
            .items_center()
            .gap(px(5.))
            .rounded(px(RADIUS_SM))
            .bg(UiColors::tint(ui.foreground, 0.06))
            .border_1()
            .border_color(UiColors::tint(ui.foreground, 0.08))
            .text_size(px(theme::TEXT_SM))
            .text_color(ui.foreground)
            .cursor_pointer()
            .hover(move |style| style.border_color(ui.accent))
            .tooltip(ui::tooltip(
                chip.raw.trim_start_matches('@').to_string(),
                None,
            ))
            .child(file.render().size(px(13.)))
            .child(label)
            .on_click(move |_, _, cx| {
                let location = location.clone();
                chat.update(cx, |_, cx| cx.emit(ClaudeChatEvent::OpenLocation(location)))
                    .ok();
            })
            .into_any_element()
    }

    /// What a plugin's session sent with the message (part 10.3: a task — its key, text, comments):
    /// a chip with its label, the text on hover.
    fn attachment_chip(&self, id: EntryId, index: usize, block: AttachmentBlock<'_>) -> AnyElement {
        let ui = self.ui;
        let preview = attachment_preview(block.text);
        div()
            .id(("claude-attachment-chip", (id as usize) * 16 + index))
            .h(px(22.))
            .px(px(7.))
            .flex()
            .items_center()
            .gap(px(5.))
            .rounded(px(RADIUS_SM))
            .bg(UiColors::tint(ui.foreground, 0.06))
            .border_1()
            .border_color(UiColors::tint(ui.foreground, 0.08))
            .text_size(px(theme::TEXT_SM))
            .text_color(ui.foreground)
            .when(!preview.is_empty(), |chip| {
                chip.tooltip(ui::tooltip(preview, None))
            })
            .child(icon(IconName::Paperclip, ui.text_muted).size(px(13.)))
            .child(block.label.to_string())
            .into_any_element()
    }

    /// The message's text; file mentions (`@src/a.rs#L3-9`) look like links and open the file.
    fn user_text(&self, id: EntryId, text: &str, cancelled: bool) -> AnyElement {
        let ui = self.ui;
        let segments = split_mentions(text);
        let mut display = String::new();
        let mut runs = Vec::new();
        let mut ranges = Vec::new();
        let mut targets = Vec::new();
        let base = TextRun {
            len: 0,
            font: font(theme::UI_FONT),
            color: ui.foreground,
            background_color: None,
            underline: None,
            strikethrough: cancelled.then(|| gpui::StrikethroughStyle {
                thickness: px(1.),
                color: Some(ui.dim),
            }),
        };
        for segment in &segments {
            match segment {
                Segment::Text(text) => {
                    runs.push(TextRun {
                        len: text.len(),
                        ..base.clone()
                    });
                    display.push_str(text);
                }
                Segment::Mention { raw, path, lines } => {
                    let start = display.len();
                    display.push_str(raw);
                    runs.push(TextRun {
                        len: raw.len(),
                        font: font(theme::code_font()),
                        color: ui.accent_text,
                        background_color: Some(UiColors::tint(ui.accent, 0.14)),
                        ..base.clone()
                    });
                    ranges.push(start..display.len());
                    targets.push(Location {
                        path: self.root.join(path),
                        line: lines.map_or(0, |(first, _)| first.saturating_sub(1)),
                        start: 0,
                        end: 0,
                    });
                }
            }
        }
        let runs = match self.marks(id) {
            Some(marks) => marks.mark(&display, runs),
            None => runs,
        };
        // Web addresses and plugins' links (part 10.3): `TRACK-142` goes to its task, with its
        // tooltip.
        let mut links: Vec<_> = crate::plugin_links::web_links(&display)
            .into_iter()
            .filter(|url| {
                !ranges
                    .iter()
                    .any(|r| r.start < url.end && url.start < r.end)
            })
            .map(|url| {
                let target = crate::plugin_links::Target::Url(display[url.clone()].to_string());
                (url, target)
            })
            .collect();
        let taken: Vec<_> = ranges
            .iter()
            .cloned()
            .chain(links.iter().map(|(range, _)| range.clone()))
            .collect();
        links.extend(crate::plugin_links::plugin_targets(&display, &taken));
        links.sort_by_key(|(range, _)| range.start);
        let link_ranges: Vec<_> = links.iter().map(|(range, _)| range.clone()).collect();
        let runs = crate::plugin_links::link_runs(runs, &link_ranges, ui.accent_text);
        let select = self.select(id);
        let (index, runs) = select.text(&display, runs, claude_select::Joint::Block, None);
        let styled = StyledText::new(display).with_runs(runs);
        let layout = styled.layout().clone();
        // The bubble takes the mouse (`user`).
        let selectable = |text: AnyElement| select.wrap(index, &layout, text);
        if ranges.is_empty() && links.is_empty() {
            return selectable(styled.into_any_element());
        }
        let chat = self.chat.clone();
        let mentions = ranges.len();
        let links = Rc::new(links);
        let clicked = links.clone();
        let text = InteractiveText::new(("claude-user-text", id), styled)
            .on_click(
                ranges.into_iter().chain(link_ranges).collect(),
                move |index, window, cx| {
                    if let Some(location) = targets.get(index).cloned() {
                        chat.update(cx, |_, cx| cx.emit(ClaudeChatEvent::OpenLocation(location)))
                            .ok();
                    } else {
                        match clicked.get(index - mentions) {
                            Some((_, crate::plugin_links::Target::Plugin(link))) => {
                                let store = crate::plugin_links::store_of(window, cx);
                                crate::plugin_links::activate(link, store, cx);
                            }
                            Some((_, crate::plugin_links::Target::Url(url))) => {
                                crate::plugin_links::open_url(url, cx)
                            }
                            None => {}
                        }
                    }
                },
            )
            .tooltip(move |index, window, cx| {
                let (_, target) = links.iter().find(|(range, _)| range.contains(&index))?;
                match target {
                    crate::plugin_links::Target::Plugin(link) => {
                        let store = crate::plugin_links::store_of(window, cx);
                        crate::plugin_links::tooltip_view(link, store, cx)
                    }
                    crate::plugin_links::Target::Url(url) => {
                        Some(crate::plugin_links::url_tooltip(url, cx))
                    }
                }
            })
            .into_any_element();
        selectable(text)
    }

    /// A `!` command: a terminal card on the user's side — the command, how it ended (or a clock
    /// and Stop while it runs), its output; under it, whether Claude got the output.
    fn shell(&self, id: EntryId, shell: &ShellEntry) -> AnyElement {
        let ui = self.ui;
        let running = shell.state == ShellState::Running;
        let output = shell.output.trim_end();
        let expandable = !output.is_empty();
        let expanded = expandable && self.state.expanded(id, true);
        let status: Option<AnyElement> = match shell.state {
            ShellState::Running => {
                let chat = self.chat.clone();
                Some(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .children(shell.started.map(|started| {
                            div()
                                .text_size(px(theme::TEXT_XS))
                                .text_color(ui.dim)
                                .child(format_elapsed(started.elapsed()))
                        }))
                        .child(
                            ui::text_button(("claude-shell-stop", id), tr("Stop"), true, ui)
                                .h(px(22.))
                                .on_click(move |_, _, cx| {
                                    chat.update(cx, |chat, cx| chat.stop_shell(id, cx)).ok();
                                }),
                        )
                        .into_any_element(),
                )
            }
            ShellState::Done => {
                let mut parts = Vec::new();
                if let Some(code) = shell.exit_code {
                    parts.push(trf("exit {0}", &[&code]));
                }
                if let Some(duration) = shell.duration {
                    parts.push(format_duration(duration));
                }
                let failed = shell.exit_code.is_some_and(|code| code != 0);
                (!parts.is_empty()).then(|| {
                    div()
                        .flex_none()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(if failed { ui.error } else { ui.dim })
                        .child(parts.join(" · "))
                        .into_any_element()
                })
            }
            ShellState::Stopped => state_mark(ToolState::Interrupted, ui),
            ShellState::Failed => Some(ui::badge(tr("failed"), ui.error).into_any_element()),
        };
        let header = div()
            .id(("claude-shell", id))
            .h(px(32.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(CARD_RADIUS - 1.))
            .child(bang_badge(ui))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(theme::code_font())
                    .text_size(px(12.))
                    .text_color(ui.foreground)
                    .child(self.marked(id, first_line(&shell.command))),
            )
            .when(running, |row| {
                row.child(pulse(("claude-shell-pulse", id), ui))
            })
            .children(status)
            .when(expandable, |row| {
                row.cursor_pointer()
                    .hover(move |style| style.bg(ui.hover))
                    .child(chevron(expanded, ui))
                    .on_click(self.toggle(id, expanded))
            });
        let folding = expandable && self.state.folding(id, false).is_some();
        let body = (expanded || folding).then(|| {
            let color = if shell.state == ShellState::Failed {
                ui.error
            } else {
                ui.text_muted
            };
            div()
                .border_t_1()
                .border_color(ui.divider)
                .px(px(10.))
                .py(px(8.))
                .child(self.marked_output_lines(id, output, color))
                .into_any_element()
        });
        let body = self.fold(id, expanded, body);
        let note = if shell.delivered {
            tr("Claude got the output")
        } else {
            tr("Claude will get the output")
        };
        div()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(5.))
            .child(
                div()
                    .w(gpui::relative(BUBBLE_SHARE))
                    .flex()
                    .flex_col()
                    .rounded(px(CARD_RADIUS))
                    .border_1()
                    .border_color(UiColors::tint(ui.success, 0.28))
                    .bg(UiColors::tint(ui.success, 0.04))
                    .child(header)
                    .children(body),
            )
            .child(
                div()
                    .h(px(16.))
                    .text_size(px(theme::TEXT_XS))
                    .text_color(ui.dim)
                    .child(note),
            )
            .into_any_element()
    }

    // --- Claude's text and thinking ---

    fn text(&self, id: EntryId, text: &str, streaming: bool) -> AnyElement {
        let ui = self.ui;
        let Some(parsed) = self.state.texts.get(&id) else {
            return div().child(text.to_string()).into_any_element();
        };
        let group: SharedString = format!("claude-text-{id}").into();
        let copied = self.state.just_copied(id);
        let copy = (!streaming).then(|| self.copy_button(id, text.to_string()));
        // The copy button sits over the message's bottom right corner: no room is taken for it.
        let select = self.select(id);
        div()
            .group(group.clone())
            .relative()
            .cursor_text()
            .on_mouse_down(
                gpui::MouseButton::Left,
                claude_select::mouse_down(self.chat.clone(), id),
            )
            .child(markdown::render_selectable(
                &parsed.blocks,
                ui.foreground,
                self.theme,
                format!("claude-code-{id}"),
                self.marks(id).as_deref(),
                &select,
            ))
            .children(copy.map(|copy| {
                div()
                    .id(("claude-text-copy", id))
                    .absolute()
                    .right(px(-4.))
                    .bottom(px(-4.))
                    .rounded(px(ui::RADIUS_XS))
                    .bg(ui.island)
                    .when(!copied, |corner| {
                        corner
                            .opacity(0.)
                            .group_hover(group.clone(), |style| style.opacity(1.))
                    })
                    .child(copy)
            }))
            .into_any_element()
    }

    /// A small copy button (its entry shows it while the pointer is over it; the check mark stays
    /// a moment after a click).
    fn copy_button(&self, id: EntryId, text: String) -> AnyElement {
        let ui = self.ui;
        let copied = self.state.just_copied(id);
        let chat = self.chat.clone();
        div()
            .id(("claude-copy", id))
            .size(px(20.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(ui::RADIUS_XS))
            .cursor_pointer()
            .hover(move |style| style.bg(ui.hover))
            .tooltip(ui::tooltip(tr("Copy"), None))
            .child(copy_icon(copied, 13., ui.dim, ui.success))
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                chat.update(cx, |chat, cx| note_copied(chat, id, cx)).ok();
            })
            .into_any_element()
    }

    fn thinking(
        &self,
        id: EntryId,
        text: &str,
        tokens: u64,
        streaming: bool,
        duration: Option<Duration>,
    ) -> AnyElement {
        let ui = self.ui;
        let label = thinking_label(streaming, tokens, duration);
        let expandable = !text.trim().is_empty();
        let expanded = expandable && self.state.expanded(id, false);
        let row = div()
            .id(("claude-thinking", id))
            .h(px(ROW_HEIGHT))
            .px(px(4.))
            .mx(px(-4.))
            .rounded(px(RADIUS_SM))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(theme::TEXT_SM))
            .text_color(ui.dim)
            .child(
                div()
                    .flex_none()
                    .w(px(16.))
                    .flex()
                    .justify_center()
                    .child(if streaming {
                        pulse(("claude-thinking-pulse", id), ui).into_any_element()
                    } else {
                        icon(IconName::Sparkle, ui.dim)
                            .size(px(13.))
                            .into_any_element()
                    }),
            )
            .child(label)
            .when(expandable, |row| {
                row.cursor_pointer()
                    .hover(move |style| style.bg(ui.hover))
                    .child(chevron(expanded, ui))
                    .on_click(self.toggle(id, expanded))
            });
        let folding = expandable && self.state.folding(id, false).is_some();
        let body = (expanded || folding).then(|| {
            let blocks = self
                .state
                .texts
                .get(&id)
                .map(|parsed| parsed.blocks.clone());
            div()
                .ml(px(7.))
                .pl(px(DETAIL_INSET - 7.))
                .pb(px(4.))
                .border_l_1()
                .border_color(ui.divider)
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.text_muted)
                .child(match blocks {
                    Some(blocks) => markdown::render(&blocks, ui.text_muted, self.theme),
                    None => div().child(text.to_string()).into_any_element(),
                })
                .into_any_element()
        });
        div()
            .flex()
            .flex_col()
            .child(row)
            .children(self.fold(id, expanded, body))
            .into_any_element()
    }

    // --- Claude's steps ---

    /// Minor steps in a row: one line that sums them up ("Thought for 6 s · read 2 files ·
    /// searched 3 times"), and opened — a row per step. Open while its turn runs (`live`: the
    /// last item of a working session) or while a step waits for the user; closed after.
    fn group(&self, entries: &[Entry], live: bool) -> AnyElement {
        let ui = self.ui;
        let Some(first) = entries.first() else {
            return div().into_any_element();
        };
        let id = first.id;
        let shown: Vec<&Entry> = entries
            .iter()
            .filter(|entry| !is_hidden(&entry.kind))
            .collect();
        let tool_state = |entry: &&Entry| match &entry.kind {
            EntryKind::Tool(tool) => Some(tool.state),
            _ => None,
        };
        let waiting = shown
            .iter()
            .any(|entry| tool_state(entry) == Some(ToolState::Waiting));
        let failed = shown
            .iter()
            .filter(|entry| tool_state(entry) == Some(ToolState::Failed))
            .count();
        let open = self.state.group_open(id, live || waiting);
        let leading = if live {
            pulse(("claude-group-pulse", id), ui).into_any_element()
        } else {
            icon(IconName::Sparkle, ui.dim)
                .size(px(13.))
                .into_any_element()
        };
        let chat = self.chat.clone();
        let header = div()
            .id(("claude-group", id))
            .h(px(28.))
            .m(px(1.))
            .px(px(9.))
            .rounded(px(CARD_RADIUS - 2.))
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(theme::TEXT_SM))
            .text_color(ui.text_muted)
            .cursor_pointer()
            .hover(move |style| style.bg(ui.hover))
            .child(
                div()
                    .flex_none()
                    .w(px(14.))
                    .flex()
                    .justify_center()
                    .child(leading),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(group_summary(&shown)),
            )
            .when(failed > 0, |row| {
                row.child(
                    div()
                        .flex_none()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(ui.error)
                        .child(trn(failed, "{n} error", "{n} errors")),
                )
            })
            .when(waiting, |row| {
                row.children(state_mark(ToolState::Waiting, ui))
            })
            .child(chevron(open, ui))
            .on_click(move |_, _, cx| {
                chat.update(cx, |chat, cx| {
                    let transcript = chat.shown_transcript_mut();
                    transcript.opened.insert(id, !open);
                    transcript.fold(id, !open, Redraw::Frame);
                    transcript.invalidate();
                    cx.notify();
                })
                .ok();
            });
        let folding = self.state.folding(id, false).is_some();
        let rows = (open || folding).then(|| {
            div()
                .flex()
                .flex_col()
                .px(px(10.))
                .pb(px(5.))
                .children(shown.iter().map(|entry| {
                    let row = match &entry.kind {
                        EntryKind::Tool(tool) => self.tool(entry.id, tool, 1),
                        _ => self.entry(entry, 1),
                    };
                    if entry.id == id {
                        row
                    } else {
                        self.arriving_step(entry, row)
                    }
                }))
                .into_any_element()
        });
        let rows = self.fold(id, open, rows);
        div()
            .flex()
            .flex_col()
            .rounded(px(CARD_RADIUS))
            .bg(UiColors::tint(ui.foreground, 0.03))
            .border_1()
            .border_color(UiColors::tint(ui.foreground, 0.07))
            .child(header)
            .children(rows)
            .into_any_element()
    }

    /// A card: a frame with a header (a tile with the step's icon, its title, what's at the end)
    /// and, opened, a body under a line.
    fn card(
        &self,
        id: impl Into<gpui::ElementId>,
        header: Vec<AnyElement>,
        body: Option<AnyElement>,
        toggle: Option<bool>,
        accent: Option<Hsla>,
    ) -> AnyElement {
        let ui = self.ui;
        let border = accent.map_or(UiColors::tint(ui.foreground, 0.09), |color| {
            UiColors::tint(color, 0.3)
        });
        let background = accent.map_or(UiColors::tint(ui.foreground, 0.025), |color| {
            UiColors::tint(color, 0.06)
        });
        let header = div()
            .id(id)
            .h(px(34.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(CARD_RADIUS - 1.))
            .text_size(px(theme::TEXT_SM))
            .children(header);
        let header = match toggle {
            Some(expanded) => header
                .cursor_pointer()
                .hover(move |style| style.bg(ui.hover))
                .child(chevron(expanded, ui))
                .into_any_element(),
            None => header.into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .rounded(px(CARD_RADIUS))
            .border_1()
            .border_color(border)
            .bg(background)
            .child(header)
            .children(body.map(|body| div().border_t_1().border_color(ui.divider).child(body)))
            .into_any_element()
    }

    /// A command Claude ran: a terminal card — the command, how it ended, its output (open when it
    /// failed or was refused).
    fn bash_card(&self, id: EntryId, tool: &ToolEntry) -> AnyElement {
        let ui = self.ui;
        let ToolCall::Bash {
            command,
            background,
            ..
        } = &tool.call
        else {
            return self.tool(id, tool, 0);
        };
        // A command in the background: its call ends at once, the command goes on as a task —
        // the card shows the task's state.
        let task = if *background && tool.state == ToolState::Done {
            background_run(self.model, tool)
        } else {
            None
        };
        let state = match task {
            Some(BackgroundRun::Running) => ToolState::Running,
            Some(BackgroundRun::Failed) => ToolState::Failed,
            Some(BackgroundRun::Stopped) => ToolState::Interrupted,
            Some(BackgroundRun::Exited(_)) | None => tool.state,
        };
        let running = matches!(state, ToolState::Streaming | ToolState::Running);
        let exit = match task {
            Some(BackgroundRun::Exited(code)) => Some(code),
            _ => exit_code(tool),
        };
        let failed = state == ToolState::Failed
            || exit.is_some_and(|code| code != 0)
            || tool.result.as_ref().is_some_and(|result| result.is_error);
        let color = match state {
            ToolState::Streaming | ToolState::Running => ui.accent_text,
            ToolState::Denied | ToolState::Waiting => ui.warning,
            ToolState::Interrupted => ui.dim,
            _ if failed => ui.error,
            _ => ui.success,
        };
        let body = self
            .details_body(id, tool, 0)
            .map(|body| div().px(px(10.)).py(px(8.)).child(body).into_any_element());
        let expanded = body.is_some()
            && self
                .state
                .expanded(id, failed || tool.state == ToolState::Denied);
        let end: Option<AnyElement> = match (state, exit) {
            (ToolState::Waiting | ToolState::Denied | ToolState::Interrupted, _) => {
                state_mark(tool.state, ui)
            }
            (_, Some(code)) if code != 0 => {
                Some(ui::badge(trf("exit {0}", &[&code]), ui.error).into_any_element())
            }
            (ToolState::Failed, _) => Some(ui::badge(tr("failed"), ui.error).into_any_element()),
            _ => None,
        };
        let mut header = vec![
            tile(
                IconName::Terminal,
                color,
                running,
                ("claude-bash-pulse", id),
                ui,
            ),
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(theme::code_font())
                .text_size(px(12.))
                .text_color(ui.foreground)
                .child(self.marked(id, first_line(command)))
                .into_any_element(),
        ];
        if *background {
            // In the accent while the command runs (the tile pulses too), dim once it ended.
            let color = if running { ui.accent_text } else { ui.dim };
            header.push(ui::badge(tr("in the background"), color).into_any_element());
        }
        header.extend(end);
        let toggle = body.is_some().then_some(expanded);
        let body = self.fold(id, expanded, body);
        let card = self.card(("claude-bash", id), header, body, toggle, None);
        self.clickable_card(id, card, toggle)
    }

    /// A card whose header flips its body (`toggle`: whether it is open; `None` — it has none).
    fn clickable_card(&self, id: EntryId, card: AnyElement, toggle: Option<bool>) -> AnyElement {
        let Some(expanded) = toggle else {
            return card;
        };
        div()
            .id(("claude-card-toggle", id))
            .on_click(self.toggle(id, expanded))
            .child(card)
            .into_any_element()
    }

    /// An edit of Claude's: a file card — the path (a link to the change), +N −M, Diff and
    /// Rollback, and the change itself (its first lines; all of them on "Show more").
    fn edit_card(&self, id: EntryId, tool: &ToolEntry) -> AnyElement {
        let ui = self.ui;
        let structured = tool
            .result
            .as_ref()
            .and_then(|result| result.structured.as_ref());
        let created = matches!(tool.call, ToolCall::Write { .. })
            && structured.is_some_and(|result| result["type"] == "create");
        let summary = tool_summary(tool, self.root, self.tasks);
        let running = matches!(tool.state, ToolState::Streaming | ToolState::Running);
        let color = match tool.state {
            ToolState::Streaming | ToolState::Running => ui.accent_text,
            ToolState::Denied | ToolState::Waiting => ui.warning,
            ToolState::Failed => ui.error,
            ToolState::Interrupted => ui.dim,
            ToolState::Done if created => ui.vcs_added,
            ToolState::Done => ui.orange,
        };
        let path = tool.call_path().map(|path| Location {
            path: path.to_path_buf(),
            line: summary.line.unwrap_or(0),
            start: 0,
            end: 0,
        });
        let label = {
            let text = div()
                .min_w_0()
                .truncate()
                .font_family(theme::code_font())
                .text_size(px(12.))
                .text_color(ui.accent_text)
                .child(summary.detail.clone());
            match path {
                Some(location) => {
                    let chat = self.chat.clone();
                    text.id(("claude-edit-path", id))
                        .cursor_pointer()
                        .hover(|style| style.underline())
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            let location = location.clone();
                            chat.update(cx, |_, cx| {
                                cx.emit(ClaudeChatEvent::OpenLocation(location))
                            })
                            .ok();
                        })
                        .into_any_element()
                }
                None => text.into_any_element(),
            }
        };
        let counts = match (structured.and_then(patch_counts), &tool.call) {
            (Some((added, removed)), _) if added > 0 || removed > 0 => Some((added, removed)),
            (_, ToolCall::Write { content, .. }) if created => Some((content.lines().count(), 0)),
            _ => None,
        };
        let mut header = vec![
            tile(
                if created {
                    IconName::FilePlus
                } else {
                    IconName::Pencil
                },
                color,
                running,
                ("claude-edit-pulse", id),
                ui,
            ),
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(label)
                .children(counts.map(|(added, removed)| counts_label(added, removed, ui)))
                .into_any_element(),
        ];
        header.extend(state_mark(tool.state, ui));
        // Icons, not labels: every edit has them, and the island is narrow.
        if tool.state == ToolState::Done && tool.call_path().is_some() {
            let chat = self.chat.clone();
            header.push(
                small_button(("claude-edit-diff", id), IconName::Diff, ui)
                    .tooltip(ui::tooltip(tr("Show Diff"), None))
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        chat.update(cx, |chat, cx| chat.diff_edit(id, window, cx))
                            .ok();
                    })
                    .into_any_element(),
            );
            let chat = self.chat.clone();
            header.push(
                small_button(("claude-edit-rollback", id), IconName::Rollback, ui)
                    .tooltip(ui::tooltip(tr("Rollback"), None))
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        chat.update(cx, |chat, cx| chat.rollback_edit(id, window, cx))
                            .ok();
                    })
                    .into_any_element(),
            );
        }
        let body = self
            .edit_body(id, tool, structured)
            .map(|body| div().px(px(4.)).py(px(4.)).child(body).into_any_element());
        let expanded = body.is_some() && self.state.expanded(id, true);
        let toggle = body.is_some().then_some(expanded);
        let body = self.fold(id, expanded, body);
        let card = self.card(("claude-edit", id), header, body, toggle, None);
        self.clickable_card(id, card, toggle)
    }

    /// What an edit card opens to: the change, or why it didn't happen.
    fn edit_body(
        &self,
        id: EntryId,
        tool: &ToolEntry,
        structured: Option<&Value>,
    ) -> Option<AnyElement> {
        // A refused or failed edit changed nothing: its reason, not a diff.
        if matches!(tool.state, ToolState::Denied | ToolState::Failed) {
            return self.result_body(id, tool);
        }
        let lines = structured.map(patch_lines).unwrap_or_default();
        let lines = if lines.is_empty() {
            // A new file: its text as added lines.
            match &tool.call {
                ToolCall::Write { content, .. } if tool.state == ToolState::Done => content
                    .lines()
                    .enumerate()
                    .map(|(index, text)| PatchLine {
                        kind: LineKind::Added,
                        old: None,
                        new: Some(index + 1),
                        text: text.to_string(),
                    })
                    .collect(),
                _ => Vec::new(),
            }
        } else {
            lines
        };
        if lines.is_empty() {
            return self.result_body(id, tool);
        }
        Some(self.diff_lines(id, &lines, DIFF_PREVIEW_LINES))
    }

    /// A subagent: a card with what it does and how far it got — its latest steps while it runs,
    /// its answer when done — and Watch / Open for its whole conversation.
    fn agent_card(&self, id: EntryId, tool: &ToolEntry) -> AnyElement {
        let ui = self.ui;
        let ToolCall::Agent {
            description,
            subagent_type,
            prompt,
            background,
        } = &tool.call
        else {
            return self.tool(id, tool, 0);
        };
        let running = agent_running(tool);
        let structured = tool
            .result
            .as_ref()
            .and_then(|result| result.structured.as_ref())
            .filter(|result| result["status"] == "completed");
        // The title keeps the first line; how far it got goes under it, next to Watch / Open.
        let mark: Option<AnyElement> = if running {
            Some(pulse(("claude-agent-pulse", id), ui).into_any_element())
        } else {
            match tool.state {
                ToolState::Done => Some(
                    icon(IconName::CheckCircle, ui.success)
                        .size(px(14.))
                        .into_any_element(),
                ),
                state => state_mark(state, ui),
            }
        };
        let meta = if running {
            agent_progress(tool)
        } else {
            structured.and_then(agent_totals)
        };
        let kind = subagent_type
            .clone()
            .filter(|kind| kind != "general-purpose");
        let title = div()
            .id(("claude-agent-title", id))
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .tooltip(ui::tooltip(clip_chars(prompt, 400), None))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(ui.foreground)
                    .child(description.clone()),
            )
            .children(kind.map(|kind| {
                div()
                    .flex_none()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(ui.dim)
                    .child(kind)
            }))
            .when(*background, |row| {
                row.child(ui::badge(tr("in the background"), ui.dim))
            });
        let first = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(theme::TEXT_SM))
            .child(
                div()
                    .flex_none()
                    .size(px(22.))
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(UiColors::tint(ui.accent, 0.2))
                    .child(icon(IconName::Agent, ui.accent_text).size(px(13.))),
            )
            .child(title)
            .children(mark);
        // While it runs: its latest steps; done: its answer, in short.
        let steps: Vec<AnyElement> = if running {
            let shown: Vec<&Entry> = tool
                .children
                .iter()
                .filter(|child| {
                    matches!(child.kind, EntryKind::Tool(_) | EntryKind::Text { .. })
                        && !is_hidden(&child.kind)
                })
                .collect();
            shown[shown.len().saturating_sub(AGENT_STEPS)..]
                .iter()
                .map(|child| self.agent_step(child))
                .collect()
        } else {
            Vec::new()
        };
        let answer = agent_card_answer(tool).map(|answer| {
            div()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.text_muted)
                .child(self.marked(id, answer))
                .into_any_element()
        });
        let open = (running || !tool.children.is_empty() || answer.is_some()).then(|| {
            let chat = self.chat.clone();
            div()
                .id(("claude-agent-open", id))
                .flex_none()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.accent_text)
                .cursor_pointer()
                .hover(|style| style.underline())
                .child(if running { tr("Watch") } else { tr("Open") })
                .on_click(move |_, window, cx| {
                    chat.update(cx, |chat, cx| chat.open_agent(id, window, cx))
                        .ok();
                })
        });
        let footer = (meta.is_some() || open.is_some()).then(|| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(ui.dim)
                        .children(meta),
                )
                .children(open)
        });
        let second = (!steps.is_empty() || answer.is_some() || footer.is_some()).then(|| {
            div()
                .ml(px(30.))
                .flex()
                .flex_col()
                .gap(px(3.))
                .children(steps)
                .children(answer)
                .children(footer)
        });
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .px(px(10.))
            .py(px(9.))
            .rounded(px(CARD_RADIUS))
            .border_1()
            .border_color(UiColors::tint(ui.accent, 0.28))
            .bg(UiColors::tint(ui.accent, 0.07))
            .child(first)
            .children(second)
            .into_any_element()
    }

    /// A step of a running subagent on its card: quiet, one line.
    fn agent_step(&self, entry: &Entry) -> AnyElement {
        let ui = self.ui;
        let (icon_name, title, detail, running) = match &entry.kind {
            EntryKind::Tool(tool) => {
                let summary = tool_summary(tool, self.root, self.tasks);
                (
                    summary.icon,
                    summary.title.to_string(),
                    summary.detail,
                    matches!(tool.state, ToolState::Streaming | ToolState::Running),
                )
            }
            EntryKind::Text { text, .. } => {
                (IconName::Claude, String::new(), first_line(text), false)
            }
            _ => (IconName::Claude, String::new(), String::new(), false),
        };
        div()
            .h(px(20.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(theme::TEXT_XS))
            .text_color(ui.text_muted)
            .child(icon(icon_name, if running { ui.accent_text } else { ui.dim }).size(px(12.)))
            .when(!title.is_empty(), |row| {
                row.child(div().flex_none().text_color(ui.foreground).child(title))
            })
            .child(div().min_w_0().truncate().child(detail))
            .into_any_element()
    }

    /// A row of a tool call (in a group, or for the calls without a card): its icon, name and
    /// detail, a note at the end; a click opens its details.
    fn tool(&self, id: EntryId, tool: &ToolEntry, depth: usize) -> AnyElement {
        let ui = self.ui;
        let summary = tool_summary(tool, self.root, self.tasks);
        let running = matches!(tool.state, ToolState::Streaming | ToolState::Running);
        let details = self.details(id, tool, depth);
        let expanded = details.is_some() && self.state.expanded(id, false);
        let mark = state_mark(tool.state, ui);
        // A file's row has the file type's icon (in the row's quiet color).
        let icon_source = match &tool.call {
            ToolCall::Read { path, .. } => path
                .file_name()
                .and_then(|name| name.to_str())
                .map_or(IconSource::Builtin(summary.icon), |name| {
                    file_icon(name, &ui).source
                }),
            _ => IconSource::Builtin(summary.icon),
        };
        let leading = if running {
            pulse(("claude-tool-pulse", id), ui).into_any_element()
        } else {
            source_icon(&icon_source, tool_color(tool.state, ui))
                .size(px(14.))
                .into_any_element()
        };
        let path = tool.call_path().map(|path| Location {
            path: path.to_path_buf(),
            line: summary.line.unwrap_or(0),
            start: 0,
            end: 0,
        });
        let detail_label = {
            let text = div()
                .min_w_0()
                .truncate()
                .text_color(ui.text_muted)
                .when(summary.code, |label| {
                    label.font_family(theme::code_font()).text_size(px(12.))
                })
                .child(self.marked(id, summary.detail.clone()));
            match path {
                Some(location) => {
                    let chat = self.chat.clone();
                    text.id(("claude-tool-path", id))
                        .cursor_pointer()
                        .hover(move |style| style.text_color(ui.accent_text))
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            let location = location.clone();
                            chat.update(cx, |_, cx| {
                                cx.emit(ClaudeChatEvent::OpenLocation(location))
                            })
                            .ok();
                        })
                        .into_any_element()
                }
                None => text.into_any_element(),
            }
        };
        let row = div()
            .id(("claude-tool", id))
            .h(px(ROW_HEIGHT))
            .px(px(4.))
            .mx(px(-4.))
            .rounded(px(RADIUS_SM))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(theme::TEXT_SM))
            .child(
                div()
                    .flex_none()
                    .w(px(16.))
                    .flex()
                    .justify_center()
                    .child(leading),
            )
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(ui.foreground)
                    .child(summary.title.clone()),
            )
            .child(div().flex_1().min_w_0().flex().child(detail_label))
            // A meta keeps a part of the row: a long one (an "Other" answer, a deep path) ends in
            // an ellipsis instead of pushing the row past the chat's edge.
            .children(summary.meta.clone().map(|meta| {
                div()
                    .flex_none()
                    .max_w(gpui::relative(0.45))
                    .truncate()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(ui.dim)
                    .child(meta.replace('\n', " "))
            }))
            .children(mark)
            .when(details.is_some(), |row| {
                row.cursor_pointer()
                    .hover(move |style| style.bg(ui.hover))
                    .child(chevron(expanded, ui))
                    .on_click(self.toggle(id, expanded))
            });
        let error = (tool.state == ToolState::Failed && !expanded)
            .then_some(tool.result.as_ref())
            .flatten()
            .map(|result| first_line(&result.text))
            .filter(|line| !line.is_empty())
            .map(|line| {
                div()
                    .pl(px(DETAIL_INSET))
                    .text_size(px(theme::TEXT_XS))
                    .text_color(ui.error)
                    .truncate()
                    .child(line)
            });
        div()
            .flex()
            .flex_col()
            .child(row)
            .children(error)
            .children(self.fold(id, expanded, details))
            .into_any_element()
    }

    /// The flip of an entry's expansion (`expanded` now), for a click handler.
    fn toggle(
        &self,
        id: EntryId,
        expanded: bool,
    ) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
        let chat = self.chat.clone();
        move |_, _, cx| {
            chat.update(cx, |chat, cx| {
                chat.shown_transcript_mut().toggle(id, expanded);
                cx.notify();
            })
            .ok();
        }
    }

    /// What a row shows when expanded, under it with a guide line; `None` — nothing to show.
    fn details(&self, id: EntryId, tool: &ToolEntry, depth: usize) -> Option<AnyElement> {
        self.details_body(id, tool, depth)
            .map(|body| self.detail_box(body))
    }

    /// What a tool call has to show besides its row (a card's body, a row's details).
    fn details_body(&self, id: EntryId, tool: &ToolEntry, depth: usize) -> Option<AnyElement> {
        let ui = self.ui;
        let structured = tool
            .result
            .as_ref()
            .and_then(|result| result.structured.as_ref());
        let body: AnyElement = match &tool.call {
            ToolCall::Agent { prompt, .. } => {
                if tool.children.is_empty() && tool.result.is_none() {
                    return None;
                }
                let progress = agent_progress(tool).map(|progress| {
                    div()
                        .mt_1()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(ui.dim)
                        .child(progress)
                });
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(theme::TEXT_XS))
                            .text_color(ui.dim)
                            .child(clip_chars(prompt, 240)),
                    )
                    .children(
                        tool.children
                            .iter()
                            .filter(|child| !is_hidden(&child.kind))
                            .map(|child| self.entry(child, depth + 1)),
                    )
                    .children(progress)
                    .children(agent_answer(tool).filter(|_| tool.children.is_empty()).map(
                        |answer| {
                            div()
                                .mt_1()
                                .text_size(px(theme::TEXT_SM))
                                .text_color(ui.text_muted)
                                .child(clip_chars(&answer, 600))
                        },
                    ))
                    .into_any_element()
            }
            ToolCall::Edit { .. } | ToolCall::Write { .. } | ToolCall::NotebookEdit { .. } => {
                return self.edit_body(id, tool, structured);
            }
            ToolCall::Bash { command, .. } => {
                let (stdout, stderr) = match structured {
                    Some(result) if result.is_object() => (
                        result["stdout"].as_str().unwrap_or("").to_string(),
                        result["stderr"].as_str().unwrap_or("").to_string(),
                    ),
                    _ => (
                        tool.result
                            .as_ref()
                            .map(|result| result.text.clone())
                            .unwrap_or_default(),
                        String::new(),
                    ),
                };
                let interrupted = structured
                    .and_then(|result| result["interrupted"].as_bool())
                    .unwrap_or(false);
                let long_command = command.contains('\n') || command.len() > 60;
                if stdout.trim().is_empty()
                    && stderr.trim().is_empty()
                    && !interrupted
                    && !long_command
                {
                    // Nothing came out: the reason it didn't run, if any.
                    return self.result_body(id, tool);
                }
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when(long_command, |column| {
                        column.child(
                            code_box("", ui.text_muted, ui)
                                .child(self.marked(id, command.to_string())),
                        )
                    })
                    .when(!stdout.trim().is_empty(), |column| {
                        column.child(self.marked_output_lines(id, stdout.trim_end(), ui.foreground))
                    })
                    .when(!stderr.trim().is_empty(), |column| {
                        column.child(self.marked_output_lines(id, stderr.trim_end(), ui.warning))
                    })
                    .when(interrupted, |column| {
                        column.child(note(tr("The command was interrupted"), ui.warning))
                    })
                    .into_any_element()
            }
            ToolCall::ExitPlanMode { plan } => {
                let plan = if plan.is_empty() {
                    structured
                        .and_then(|result| result["plan"].as_str())
                        .unwrap_or("")
                        .to_string()
                } else {
                    plan.clone()
                };
                if plan.trim().is_empty() {
                    return None;
                }
                markdown::render(&markdown::parse(&plan), ui.foreground, self.theme)
            }
            ToolCall::WebSearch { .. } => {
                let links: Vec<(String, String)> = structured
                    .and_then(|result| result["results"].as_array())
                    .into_iter()
                    .flatten()
                    .flat_map(|result| result["content"].as_array().into_iter().flatten())
                    .filter_map(|link| {
                        Some((
                            link["title"].as_str()?.to_string(),
                            link["url"].as_str()?.to_string(),
                        ))
                    })
                    .collect();
                if links.is_empty() {
                    return self.result_body(id, tool);
                }
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(links.into_iter().take(10).enumerate().map(
                        |(index, (title, url))| {
                            let target = url.clone();
                            div()
                                .id(("claude-link", (id as usize) * 16 + index))
                                .flex()
                                .gap_1p5()
                                .text_size(px(theme::TEXT_SM))
                                .cursor_pointer()
                                .on_click(move |_, _, cx| cx.open_url(&target))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(ui.accent_text)
                                        .child(title),
                                )
                                .child(div().flex_none().text_color(ui.dim).child(host(&url)))
                        },
                    ))
                    .into_any_element()
            }
            ToolCall::WebFetch { .. } => {
                let answer = structured
                    .and_then(|result| result["result"].as_str())
                    .map(str::to_string);
                match answer {
                    Some(answer) if !answer.trim().is_empty() => {
                        markdown::render(&markdown::parse(&answer), ui.text_muted, self.theme)
                    }
                    _ => return self.result_body(id, tool),
                }
            }
            ToolCall::AskUserQuestion { .. } => {
                let answers: Vec<(String, String)> = structured
                    .and_then(|result| result["answers"].as_object())
                    .map(|answers| {
                        answers
                            .iter()
                            .map(|(question, answer)| {
                                (question.clone(), answer.as_str().unwrap_or("").to_string())
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                // One question: the row already says it and the answer, unless the answer is too long
                // for the row (an "Other" text) — then the details show both in full.
                if answers.len() <= 1
                    && answers
                        .first()
                        .is_none_or(|(_, answer)| fits_in_row(answer))
                {
                    return None;
                }
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(answers.into_iter().map(|(question, answer)| {
                        div()
                            .flex()
                            .flex_col()
                            .text_size(px(theme::TEXT_SM))
                            .child(div().text_color(ui.text_muted).child(question))
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(ui.foreground)
                                    .child(answer),
                            )
                    }))
                    .into_any_element()
            }
            ToolCall::Read { .. }
            | ToolCall::TaskCreate { .. }
            | ToolCall::TaskUpdate { .. }
            | ToolCall::EnterPlanMode
            | ToolCall::Skill { .. } => {
                // Their results say nothing the row doesn't, unless they failed.
                if tool.state != ToolState::Failed {
                    return None;
                }
                return self.result_body(id, tool);
            }
            ToolCall::TodoWrite { todos } => div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .children(todos.iter().map(|todo| {
                    let (mark, color) = match todo.status {
                        flux_claude::TaskStatus::Completed => ("☑", ui.success),
                        flux_claude::TaskStatus::InProgress => ("◐", ui.accent_text),
                        flux_claude::TaskStatus::Pending => ("☐", ui.dim),
                    };
                    div()
                        .flex()
                        .gap_1p5()
                        .text_size(px(theme::TEXT_SM))
                        .child(div().text_color(color).child(mark))
                        .child(div().text_color(ui.text_muted).child(todo.subject.clone()))
                }))
                .into_any_element(),
            ToolCall::Grep { .. }
            | ToolCall::Glob { .. }
            | ToolCall::Mcp { .. }
            | ToolCall::Other => return self.result_body(id, tool),
        };
        Some(body)
    }

    /// The details' frame: under the row, past its icon, with a guide line.
    fn detail_box(&self, body: AnyElement) -> AnyElement {
        div()
            .ml(px(7.))
            .pl(px(DETAIL_INSET - 7.))
            .pt(px(2.))
            .pb(px(6.))
            .border_l_1()
            .border_color(self.ui.divider)
            .child(body)
            .into_any_element()
    }

    /// What the tool returned, as text (clipped); `None` — nothing.
    fn result_body(&self, id: EntryId, tool: &ToolEntry) -> Option<AnyElement> {
        let result = tool.result.as_ref()?;
        let text = result.text.trim_end();
        if text.trim().is_empty() {
            return None;
        }
        // A refusal is the user's (or a rule's) words: prose, not output.
        if tool.state == ToolState::Denied {
            return Some(
                div()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(self.ui.text_muted)
                    .child(clip_chars(text.trim_start_matches("Error: "), 600))
                    .into_any_element(),
            );
        }
        let color = if result.is_error {
            self.ui.error
        } else {
            self.ui.text_muted
        };
        Some(self.output_lines(id, text, color))
    }

    /// Output in the code font, clipped to a few lines with "Show more".
    fn output_lines(&self, id: EntryId, text: &str, color: Hsla) -> AnyElement {
        self.output_lines_with(id, text, color, false)
    }

    /// Output a search goes through (a command's): its matches marked.
    fn marked_output_lines(&self, id: EntryId, text: &str, color: Hsla) -> AnyElement {
        self.output_lines_with(id, text, color, true)
    }

    fn output_lines_with(&self, id: EntryId, text: &str, color: Hsla, marked: bool) -> AnyElement {
        let ui = self.ui;
        let unclipped =
            self.state.unclipped.contains(&id) || self.state.search_current() == Some(id);
        let (shown, hidden) = if unclipped {
            (text, 0)
        } else {
            clip_lines(text, CLIP_LINES)
        };
        let text_box = if marked && self.state.search.is_some() {
            code_box("", color, ui).child(self.marked(id, shown.to_string()))
        } else {
            code_box(shown, color, ui)
        };
        div()
            .flex()
            .flex_col()
            .child(text_box)
            .children((hidden > 0).then(|| self.show_more(id, hidden)))
            .into_any_element()
    }

    fn show_more(&self, id: EntryId, hidden: usize) -> AnyElement {
        let ui = self.ui;
        let chat = self.chat.clone();
        div()
            .id(("claude-show-more", id))
            .mt_1()
            .text_size(px(theme::TEXT_XS))
            .text_color(ui.accent_text)
            .cursor_pointer()
            .hover(|style| style.underline())
            .child(trn(hidden, "Show {n} more line", "Show {n} more lines"))
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                chat.update(cx, |chat, cx| {
                    chat.shown_transcript_mut().unclipped.insert(id);
                    chat.shown_transcript_mut().invalidate();
                    cx.notify();
                })
                .ok();
            })
            .into_any_element()
    }

    /// An edit's change: removed and added lines with their numbers, hunks apart; the first `max`
    /// lines until the user asks for all.
    fn diff_lines(&self, id: EntryId, lines: &[PatchLine], max: usize) -> AnyElement {
        let ui = self.ui;
        let unclipped = self.state.unclipped.contains(&id);
        let shown = if unclipped {
            lines.len()
        } else {
            lines.len().min(max)
        };
        let number = |value: Option<usize>| value.map(|n| n.to_string()).unwrap_or_default();
        let rows = lines[..shown].iter().map(|line| {
            // Removed lines take the stronger gray of the diff viewer's changed words: its
            // block gray is too faint on the code box.
            let (background, sign, color) = match line.kind {
                LineKind::Added => (Some(ui.diff_added_bg), "+", ui.foreground),
                LineKind::Removed => (Some(ui.diff_deleted_word), "−", ui.text_muted),
                LineKind::Context => (None, " ", ui.foreground),
                LineKind::Gap => (None, "⋯", ui.dim),
            };
            div()
                .flex()
                .when_some(background, |row, background| row.bg(background))
                .child(
                    div()
                        .flex_none()
                        .w(px(30.))
                        .pr_1()
                        .flex()
                        .justify_end()
                        .text_color(ui.dim)
                        .child(number(line.old)),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(30.))
                        .pr_1()
                        .flex()
                        .justify_end()
                        .text_color(ui.dim)
                        .child(number(line.new)),
                )
                .child(div().flex_none().w(px(12.)).text_color(ui.dim).child(sign))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_color(color)
                        .child(line.text.replace('\t', "    ")),
                )
        });
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .py_1()
                    .font_family(theme::code_font())
                    .text_size(px(12.))
                    .line_height(px(18.))
                    .children(rows),
            )
            .children((shown < lines.len()).then(|| {
                div()
                    .px(px(8.))
                    .pb(px(4.))
                    .child(self.show_more(id, lines.len() - shown))
            }))
            .into_any_element()
    }

    // --- Notices and the ends of turns ---

    fn notice(&self, id: EntryId, notice: &Notice) -> AnyElement {
        let ui = self.ui;
        match notice {
            Notice::Interrupted => notice_row(IconName::Stop, tr("Interrupted"), ui.dim, ui),
            Notice::Compacted {
                pre_tokens,
                post_tokens,
                ..
            } => {
                let label = match (pre_tokens, post_tokens) {
                    (Some(pre), Some(post)) => trf(
                        "Conversation compacted · {0} → {1} tokens",
                        &[&format_tokens(*pre), &format_tokens(*post)],
                    ),
                    _ => tr("Conversation compacted").to_string(),
                };
                separator(label, ui)
            }
            Notice::Retrying {
                attempt,
                max,
                error,
            } => {
                let label = trf("Retrying ({0} of {1})…", &[attempt, max]);
                let label = match error {
                    Some(error) => format!("{label} {error}"),
                    None => label,
                };
                notice_row(IconName::Refresh, label, ui.warning, ui)
            }
            Notice::Error { kind, text } => self.error(kind, text),
            Notice::Denied { tool, message } => {
                let label = trf("{0} was refused", &[tool]);
                div()
                    .flex()
                    .flex_col()
                    .child(notice_row(IconName::Warning, label, ui.warning, ui))
                    .when(!message.is_empty(), |column| {
                        column.child(
                            div()
                                .pl(px(DETAIL_INSET))
                                .text_size(px(theme::TEXT_XS))
                                .text_color(ui.dim)
                                .child(clip_chars(message, 300)),
                        )
                    })
                    .into_any_element()
            }
            Notice::Info { level, text } => {
                let (name, color) = match level.as_str() {
                    "warning" | "high" => (IconName::Warning, ui.warning),
                    _ => (IconName::Info, ui.info),
                };
                notice_row(name, text.clone(), color, ui)
            }
            Notice::LocalCommand { text, raw } => self.local_command(text, raw),
            Notice::Exited { code, stderr } => {
                let title = match code {
                    Some(code) => trf("Claude stopped (exit code {0})", &[code]),
                    None => tr("Claude couldn't start").to_string(),
                };
                callout(
                    IconName::Error,
                    title,
                    Some(tr("Send a message to start it again.").into()),
                    (!stderr.is_empty()).then(|| stderr.join("\n")),
                    ui.error,
                    None,
                    ui,
                )
            }
            Notice::Blocked { country } => self.blocked(id, country.as_ref()),
        }
    }

    /// The country check kept Claude from going or stopped it (part «Флаг страны»). The last such
    /// note, while the block lasts, is a callout of the block in force — the flag (a warning sign
    /// when the country is unknown), why, "Check Again" — as the field's banner says it. Once
    /// Claude is allowed again, or a later note took over, it is a quiet line of what was.
    fn blocked(&self, id: EntryId, country: Option<&String>) -> AnyElement {
        let ui = self.ui;
        let lead = |country: Option<&String>, size: f32| match country {
            Some(code) => div()
                .text_size(px(size))
                .child(region_guard::flag(code))
                .into_any_element(),
            None => icon(IconName::Warning, ui.warning)
                .size(px(size))
                .into_any_element(),
        };
        let latest = self
            .model
            .entries
            .iter()
            .rev()
            .find(|entry| matches!(entry.kind, EntryKind::Notice(Notice::Blocked { .. })))
            .map(|entry| entry.id);
        let Some(live) = self.block.as_ref().filter(|_| latest == Some(id)) else {
            let label = match country {
                Some(code) => trf(
                    "Claude was blocked: the IP address was in {0}.",
                    &[&region_guard::country_label(code)],
                ),
                None => {
                    tr("Claude was blocked: Flux couldn't find out the country of the IP address.")
                        .to_string()
                }
            };
            return div()
                .min_h(px(ROW_HEIGHT))
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.text_muted)
                .child(
                    div()
                        .flex_none()
                        .w(px(16.))
                        .flex()
                        .justify_center()
                        .child(lead(country, 13.)),
                )
                .child(div().min_w_0().child(label))
                .into_any_element();
        };
        let action = if live.checking {
            ui::primary_button(
                "claude-region-check",
                tr("Checking the country…"),
                false,
                ui,
            )
        } else {
            ui::primary_button("claude-region-check", tr("Check Again"), true, ui)
                .on_click(|_, _, cx| region_guard::check_again(cx))
        };
        callout_box(
            lead(live.refusal.country.as_ref(), 14.),
            tr("Claude is blocked").to_string(),
            Some(live.refusal.reason().into()),
            None,
            ui.warning,
            Some(action.into_any_element()),
            ui,
        )
    }

    fn error(&self, kind: &str, text: &str) -> AnyElement {
        let ui = self.ui;
        let (title, action): (SharedString, NoticeAction) = match kind {
            "authentication_failed" | "oauth_org_not_allowed" => (
                tr("Claude needs you to sign in again").into(),
                Some((tr("Sign In").into(), Box::new(crate::claude::SignIn))),
            ),
            "rate_limit" => (tr("Usage limit reached").into(), None),
            "billing_error" => (tr("A billing problem stopped the request").into(), None),
            "overloaded" => (
                tr("Claude is overloaded — try again in a moment").into(),
                None,
            ),
            "max_output_tokens" => (tr("The answer hit the output limit").into(), None),
            _ => (tr("The request failed").into(), None),
        };
        callout(
            IconName::Error,
            title.to_string(),
            (!text.is_empty()).then(|| SharedString::from(text.to_string())),
            None,
            ui.error,
            action,
            ui,
        )
    }

    /// The reply of a local slash command: `/context` as a breakdown, the rest as text.
    fn local_command(&self, text: &str, raw: &Value) -> AnyElement {
        let ui = self.ui;
        let context = &raw["context_usage"];
        if let (Some(total), Some(max)) = (
            context["total_tokens"].as_u64(),
            context["raw_max_tokens"].as_u64(),
        ) {
            let fraction = if max == 0 {
                0.
            } else {
                (total as f32 / max as f32).clamp(0., 1.)
            };
            let categories: Vec<(String, u64)> = context["categories"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|category| category["kind"] != "free")
                .filter_map(|category| {
                    Some((
                        category["name"].as_str()?.to_string(),
                        category["tokens"].as_u64()?,
                    ))
                })
                .filter(|(_, tokens)| *tokens > 0)
                .collect();
            return div()
                .flex()
                .flex_col()
                .gap_1p5()
                .p(px(10.))
                .rounded(px(CARD_RADIUS))
                .border_1()
                .border_color(UiColors::tint(ui.foreground, 0.09))
                .bg(UiColors::tint(ui.foreground, 0.025))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .text_size(px(theme::TEXT_SM))
                        .child(div().font_weight(FontWeight::MEDIUM).child(tr("Context")))
                        .child(div().text_color(ui.text_muted).child(trf(
                            "{0} of {1} tokens",
                            &[&format_tokens(total), &format_tokens(max)],
                        ))),
                )
                .child(
                    div()
                        .h(px(6.))
                        .rounded(px(3.))
                        .bg(UiColors::tint(ui.foreground, 0.08))
                        .child(
                            div()
                                .h_full()
                                .rounded(px(3.))
                                .w(gpui::relative(fraction))
                                .bg(ui.accent),
                        ),
                )
                .children(categories.into_iter().map(|(name, tokens)| {
                    div()
                        .flex()
                        .justify_between()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(ui.text_muted)
                        .child(name)
                        .child(format_tokens(tokens))
                }))
                .into_any_element();
        }
        if text.trim().is_empty() {
            return div().into_any_element();
        }
        code_box(text.trim_end(), ui.text_muted, ui).into_any_element()
    }

    /// The end of a turn: the "Done" card when it changed files, else a quiet line — how long it
    /// took (and why it ended, when it didn't end well), Copy for its answer.
    fn turn_end(&self, id: EntryId, summary: &TurnSummary) -> AnyElement {
        let files = self.model.turn_changes(id);
        if files.is_empty() {
            return self.turn_footer(id, summary);
        }
        self.done_card(id, summary, &files)
    }

    fn turn_footer(&self, id: EntryId, summary: &TurnSummary) -> AnyElement {
        let ui = self.ui;
        let (name, color) = turn_mark(summary, ui);
        let cost = turn_cost(summary);
        let answer = self.turn_answer(id);
        let group: SharedString = format!("claude-turn-{id}").into();
        let copied = self.state.just_copied(id);
        div()
            .id(("claude-turn-end", id))
            .group(group.clone())
            .h(px(22.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(theme::TEXT_XS))
            .text_color(if summary.is_error && !stopped(summary) {
                ui.warning
            } else {
                ui.dim
            })
            .child(icon(name, color).size(px(12.)))
            .child(turn_label(summary))
            .when_some(cost, |line, cost| line.tooltip(ui::tooltip(cost, None)))
            .child(div().flex_1())
            .children(answer.map(|answer| {
                div()
                    .id(("claude-turn-copy", id))
                    .when(!copied, |corner| {
                        corner
                            .opacity(0.)
                            .group_hover(group.clone(), |style| style.opacity(1.))
                    })
                    .child(self.copy_button(id, answer))
            }))
            .into_any_element()
    }

    /// A turn that changed files ends with a card, as an agent's task in JetBrains: how it went,
    /// the files it changed and created with their counts; Review and Rollback for all of them,
    /// and for each (on hover).
    fn done_card(&self, id: EntryId, summary: &TurnSummary, files: &[TurnFile]) -> AnyElement {
        let ui = self.ui;
        let (name, color) = turn_mark(summary, ui);
        let title = if summary.is_error {
            turn_label(summary)
        } else {
            trf("Done in {0}", &[&format_duration(summary.duration)])
        };
        // Links, as the "Done" card of JetBrains has them: room for the title in a narrow island.
        let chat = self.chat.clone();
        let review =
            link(("claude-done-review", id), tr("Review"), ui).on_click(move |_, window, cx| {
                chat.update(cx, |chat, cx| chat.review_turn(id, None, window, cx))
                    .ok();
            });
        let chat = self.chat.clone();
        let rollback = link(("claude-done-rollback", id), tr("Rollback"), ui).on_click(
            move |_, window, cx| {
                chat.update(cx, |chat, cx| chat.rollback_turn(id, None, window, cx))
                    .ok();
            },
        );
        let header = div()
            .id(("claude-done", id))
            .h(px(34.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(theme::TEXT_SM))
            .child(icon(name, color).size(px(15.)))
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(ui.foreground)
                    .child(title),
            )
            .when_some(turn_cost(summary), |row, cost| {
                row.tooltip(ui::tooltip(cost, None))
            })
            .child(div().flex_1())
            .child(review)
            .child(rollback);
        let section = |label: SharedString, files: Vec<(usize, &TurnFile)>| {
            (!files.is_empty()).then(|| {
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .px(px(12.))
                            .pt(px(4.))
                            .pb(px(2.))
                            .child(ui::section_label(label, ui)),
                    )
                    .children(
                        files
                            .into_iter()
                            .map(|(index, file)| self.done_file(id, index, file)),
                    )
            })
        };
        let changed: Vec<(usize, &TurnFile)> = files
            .iter()
            .enumerate()
            .filter(|(_, file)| !file.created)
            .collect();
        let created: Vec<(usize, &TurnFile)> = files
            .iter()
            .enumerate()
            .filter(|(_, file)| file.created)
            .collect();
        div()
            .flex()
            .flex_col()
            .pb(px(5.))
            .rounded(px(CARD_RADIUS))
            .border_1()
            .border_color(UiColors::tint(ui.foreground, 0.09))
            .bg(UiColors::tint(ui.foreground, 0.03))
            .child(header)
            .children(section(tr("Changed Files").into(), changed))
            .children(section(tr("New Files").into(), created))
            .into_any_element()
    }

    /// A file of the "Done" card: its icon and path (a link), the counts; diff and rollback on
    /// hover.
    fn done_file(&self, turn_end: EntryId, index: usize, file: &TurnFile) -> AnyElement {
        let ui = self.ui;
        let key = (turn_end as usize) * 64 + index;
        let group: SharedString = format!("claude-done-file-{key}").into();
        let label = relative(&file.path, self.root);
        let name = file
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_string();
        let location = Location {
            path: file.path.clone(),
            line: 0,
            start: 0,
            end: 0,
        };
        let chat = self.chat.clone();
        let review = {
            let chat = self.chat.clone();
            let path = file.path.clone();
            small_button(("claude-done-file-diff", key), IconName::Diff, ui)
                .tooltip(ui::tooltip(tr("Show Diff"), None))
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    let path = path.clone();
                    chat.update(cx, |chat, cx| {
                        chat.review_turn(turn_end, Some(path), window, cx)
                    })
                    .ok();
                })
        };
        let rollback = {
            let chat = self.chat.clone();
            let path = file.path.clone();
            small_button(("claude-done-file-rollback", key), IconName::Rollback, ui)
                .tooltip(ui::tooltip(tr("Rollback"), None))
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    let path = path.clone();
                    chat.update(cx, |chat, cx| {
                        chat.rollback_turn(turn_end, Some(path), window, cx)
                    })
                    .ok();
                })
        };
        div()
            .id(("claude-done-file", key))
            .group(group.clone())
            .h(px(26.))
            .mx(px(4.))
            .px(px(8.))
            .rounded(px(RADIUS_SM))
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(theme::TEXT_SM))
            .cursor_pointer()
            .hover(move |style| style.bg(ui.hover))
            .child(file_icon(&name, &ui).render().size(px(14.)))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(theme::code_font())
                    .text_size(px(12.))
                    .text_color(if file.created {
                        ui.vcs_added
                    } else {
                        ui.foreground
                    })
                    .child(label),
            )
            .child(counts_label(file.added, file.removed, ui))
            .child(div().flex_1())
            // They fade in under the pointer (an id keeps the fade).
            .child(
                div()
                    .id(("claude-done-file-actions", key))
                    .flex()
                    .gap(px(2.))
                    .opacity(0.)
                    .group_hover(group, |style| style.opacity(1.))
                    .child(review)
                    .child(rollback),
            )
            .on_click(move |_, _, cx| {
                let location = location.clone();
                chat.update(cx, |_, cx| cx.emit(ClaudeChatEvent::OpenLocation(location)))
                    .ok();
            })
            .into_any_element()
    }

    /// The answer of the turn ending at `turn_end`: its last text, to copy.
    fn turn_answer(&self, turn_end: EntryId) -> Option<String> {
        let entries = &self.model.entries;
        let end = entries.iter().position(|entry| entry.id == turn_end)?;
        entries[..end]
            .iter()
            .rev()
            .take_while(|entry| !matches!(entry.kind, EntryKind::TurnEnd(_) | EntryKind::User(_)))
            .find_map(|entry| match &entry.kind {
                EntryKind::Text { text, .. } if !text.trim().is_empty() => Some(text.clone()),
                _ => None,
            })
    }
}

/// The user stopped the turn (Esc): not an error to point at.
fn stopped(summary: &TurnSummary) -> bool {
    matches!(
        summary.reason.as_deref(),
        Some("aborted_streaming" | "aborted_tools")
    )
}

/// How long a turn took, and why it ended when it didn't end well.
fn turn_label(summary: &TurnSummary) -> String {
    let mut label = format_duration(summary.duration);
    if summary.is_error {
        let reason = match summary.reason.as_deref() {
            Some("aborted_streaming" | "aborted_tools") => tr("stopped"),
            Some("max_turns") => tr("the turn limit was reached"),
            Some("prompt_too_long") => tr("the conversation is too long, /compact it"),
            Some("budget_exhausted") => tr("the budget is spent"),
            _ => tr("ended with an error"),
        };
        label = format!("{label} · {reason}");
    }
    label
}

/// The icon of a turn's end: done, stopped or failed.
fn turn_mark(summary: &TurnSummary, ui: UiColors) -> (IconName, Hsla) {
    if !summary.is_error {
        (IconName::CheckCircle, ui.success)
    } else if stopped(summary) {
        (IconName::Stop, ui.dim)
    } else {
        (IconName::Warning, ui.warning)
    }
}

/// What a turn would have cost at the API's prices (a tooltip).
fn turn_cost(summary: &TurnSummary) -> Option<String> {
    (summary.cost_usd > 0.).then(|| {
        trf(
            "≈ ${0} at API prices (with a subscription, it counts towards its limits)",
            &[&format!("{:.2}", summary.cost_usd)],
        )
    })
}

/// A subagent still works: its call runs, or it went to the background and its task isn't over.
fn agent_running(tool: &ToolEntry) -> bool {
    matches!(tool.state, ToolState::Streaming | ToolState::Running)
        || tool.task.as_ref().is_some_and(|task| {
            task.background
                && !matches!(
                    task.status.as_deref(),
                    Some("completed" | "failed" | "killed" | "stopped")
                )
        })
}

/// A running subagent's progress: its last tool, tool calls, tokens.
fn agent_progress(tool: &ToolEntry) -> Option<String> {
    let task: &TaskProgress = tool.task.as_ref()?;
    let mut parts = Vec::new();
    if let Some(last) = &task.last_tool {
        parts.push(tool_display_name(last));
    }
    if let Some(uses) = task.tool_uses {
        parts.push(trn(uses as usize, "{n} tool use", "{n} tool uses"));
    }
    if let Some(tokens) = task.tokens {
        parts.push(trf("{0} tokens", &[&format_tokens(tokens)]));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// A finished subagent's totals: tool calls and how long it took.
fn agent_totals(result: &Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(uses) = result["totalToolUseCount"].as_u64() {
        parts.push(trn(uses as usize, "{n} tool use", "{n} tool uses"));
    }
    if let Some(ms) = result["totalDurationMs"].as_u64() {
        parts.push(format_duration(Duration::from_millis(ms)));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// A subagent's answer: its last text, else the summary of its task, else the call's result.
fn agent_answer(tool: &ToolEntry) -> Option<String> {
    let last_text = tool
        .children
        .iter()
        .rev()
        .find_map(|child| match &child.kind {
            EntryKind::Text { text, .. } if !text.trim().is_empty() => Some(text.clone()),
            _ => None,
        });
    last_text
        .or_else(|| tool.task.as_ref().and_then(|task| task.summary.clone()))
        .or_else(|| {
            tool.result
                .as_ref()
                .and_then(|result| result.structured.as_ref())
                .and_then(|result| result["content"].as_array())
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|part| part["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
        })
        .filter(|text| !text.trim().is_empty())
}

/// A subagent still works (the subagent's view, part 10.2).
pub(crate) fn agent_is_running(tool: &ToolEntry) -> bool {
    agent_running(tool)
}

/// A subagent's totals for its view's bar: how far it got while it works, what it took when done.
pub(crate) fn agent_totals_label(tool: &ToolEntry) -> Option<String> {
    if agent_running(tool) {
        return agent_progress(tool);
    }
    tool.result
        .as_ref()
        .and_then(|result| result.structured.as_ref())
        .filter(|result| result["status"] == "completed")
        .and_then(agent_totals)
}

/// The answer a finished subagent's card shows: its first lines, plain, clipped.
pub(crate) fn agent_card_answer(tool: &ToolEntry) -> Option<String> {
    (!agent_running(tool))
        .then(|| agent_answer(tool))
        .flatten()
        .map(|answer| clip_chars(&plain_text(&first_lines(&answer, 2)), 280))
}

/// The texts a command's card shows opened, as the search marks them: a long command whole, then
/// its output and its errors (part 10.2).
pub(crate) fn bash_body_texts(tool: &ToolEntry, command: &str) -> Vec<String> {
    let structured = tool
        .result
        .as_ref()
        .and_then(|result| result.structured.as_ref());
    let (stdout, stderr) = match structured {
        Some(result) if result.is_object() => (
            result["stdout"].as_str().unwrap_or("").to_string(),
            result["stderr"].as_str().unwrap_or("").to_string(),
        ),
        _ => (
            tool.result
                .as_ref()
                .map(|result| result.text.clone())
                .unwrap_or_default(),
            String::new(),
        ),
    };
    let interrupted = structured
        .and_then(|result| result["interrupted"].as_bool())
        .unwrap_or(false);
    let long_command = command.contains('\n') || command.len() > 60;
    if stdout.trim().is_empty() && stderr.trim().is_empty() && !interrupted && !long_command {
        return Vec::new();
    }
    let mut texts = Vec::new();
    if long_command {
        texts.push(command.to_string());
    }
    for output in [stdout, stderr] {
        if !output.trim().is_empty() {
            texts.push(output.trim_end().to_string());
        }
    }
    texts
}

/// The exit code a failed command reported ("Exit code 101" at the start of its result).
/// How a command Claude ran in the background is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackgroundRun {
    Running,
    /// It ended with this exit code.
    Exited(i32),
    Failed,
    /// Stopped (by the user, or with the session).
    Stopped,
}

/// The state of the background command of `tool` (a Bash call with `run_in_background`, whose
/// result came): running while the session lists its task in the background; afterwards, as the
/// task's last update says. `None` — nothing known (an old session's call).
fn background_run(model: &Session, tool: &ToolEntry) -> Option<BackgroundRun> {
    let task_id = model.task_id(&tool.tool_use_id);
    if task_id.is_some_and(|id| model.background.iter().any(|task| task.task_id == id)) {
        return Some(BackgroundRun::Running);
    }
    let task = tool.task.as_ref()?;
    match task.status.as_deref()? {
        "completed" => Some(
            task.summary
                .as_deref()
                .and_then(summary_exit_code)
                .map_or(BackgroundRun::Exited(0), BackgroundRun::Exited),
        ),
        "failed" => Some(BackgroundRun::Failed),
        "killed" => Some(BackgroundRun::Stopped),
        _ => None,
    }
}

/// The exit code in a background command's summary: `Background command "…" completed (exit code
/// 1)`.
fn summary_exit_code(summary: &str) -> Option<i32> {
    let rest = &summary[summary.rfind("exit code ")? + "exit code ".len()..];
    rest.split(|c: char| !c.is_ascii_digit() && c != '-')
        .next()?
        .parse()
        .ok()
}

fn exit_code(tool: &ToolEntry) -> Option<i32> {
    let result = tool.result.as_ref()?;
    let text = result.text.trim_start().trim_start_matches("Error: ");
    let rest = text.strip_prefix("Exit code ")?;
    rest.split(|c: char| !c.is_ascii_digit() && c != '-')
        .next()?
        .parse()
        .ok()
}

/// What a group of steps did, in short: "Thought for 6 s · read 2 files · searched 3 times".
fn group_summary(entries: &[&Entry]) -> String {
    let mut thought: Option<Duration> = None;
    let mut thinking = false;
    let mut streaming = false;
    let mut files: HashSet<&Path> = HashSet::new();
    let (mut searches, mut web, mut pages, mut tasks, mut tools) = (0, 0, 0, 0, 0);
    for entry in entries {
        match &entry.kind {
            EntryKind::Thinking {
                streaming: live,
                duration,
                ..
            } => {
                thinking = true;
                streaming |= *live;
                if let Some(duration) = duration {
                    thought = Some(thought.unwrap_or_default() + *duration);
                }
            }
            EntryKind::Tool(tool) => match &tool.call {
                ToolCall::Read { path, .. } => {
                    files.insert(path.as_path());
                }
                ToolCall::Grep { .. } | ToolCall::Glob { .. } => searches += 1,
                ToolCall::WebSearch { .. } => web += 1,
                ToolCall::WebFetch { .. } => pages += 1,
                ToolCall::TaskCreate { .. }
                | ToolCall::TaskUpdate { .. }
                | ToolCall::TodoWrite { .. } => tasks += 1,
                _ => tools += 1,
            },
            _ => {}
        }
    }
    let mut parts: Vec<String> = Vec::new();
    if thinking {
        parts.push(if streaming {
            tr("Thinking…").to_string()
        } else {
            thinking_label(false, 0, thought)
        });
    }
    if !files.is_empty() {
        parts.push(trn(files.len(), "read {n} file", "read {n} files"));
    }
    if searches > 0 {
        parts.push(trn(searches, "searched {n} time", "searched {n} times"));
    }
    if web > 0 {
        parts.push(trn(
            web,
            "searched the web {n} time",
            "searched the web {n} times",
        ));
    }
    if pages > 0 {
        parts.push(trn(pages, "fetched {n} page", "fetched {n} pages"));
    }
    if tasks > 0 {
        parts.push(tr("updated the tasks").to_string());
    }
    if tools > 0 {
        parts.push(trn(tools, "used {n} tool", "used {n} tools"));
    }
    capitalized(&parts.join(" · "))
}

/// The text with its first letter capitalized.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// When the user sent a message: "12:41" today, "Yesterday 12:41", else the date and time.
fn time_label(at: SystemTime) -> String {
    let seconds = |time: SystemTime| {
        time.duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs() as i64)
    };
    let local = |seconds: i64| seconds + crate::git_log::local_offset(seconds);
    clock_label(local(seconds(at)), local(seconds(SystemTime::now())))
}

/// [`time_label`] on local seconds since the epoch.
fn clock_label(at: i64, now: i64) -> String {
    let minutes = at.rem_euclid(86_400) / 60;
    let clock = format!("{:02}:{:02}", minutes / 60, minutes % 60);
    let day = at.div_euclid(86_400);
    let today = now.div_euclid(86_400);
    if day == today {
        clock
    } else if day + 1 == today {
        trf("Yesterday {0}", &[&clock])
    } else {
        format!("{} {clock}", crate::blame::date(at))
    }
}

/// A file name's extension for a picture's media type.
fn image_extension(media_type: &str) -> &'static str {
    match media_type {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "png",
    }
}

/// What a plugin's Claude session sends before the message (part 10.3, `claude.new-session`):
/// `<attachment name="TRACK-142">…</attachment>` — a chip above the bubble, not text in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AttachmentBlock<'a> {
    pub label: &'a str,
    pub text: &'a str,
}

const ATTACHMENT_OPEN: &str = "<attachment name=\"";
const ATTACHMENT_CLOSE: &str = "</attachment>";

/// The attachment blocks at the start of a message, and the text after them.
pub(crate) fn leading_attachments(text: &str) -> (Vec<AttachmentBlock<'_>>, &str) {
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(after) = rest.trim_start().strip_prefix(ATTACHMENT_OPEN) {
        let Some(quote) = after.find('"') else {
            break;
        };
        let Some(body) = after[quote + 1..].strip_prefix('>') else {
            break;
        };
        let Some(end) = body.find(ATTACHMENT_CLOSE) else {
            break;
        };
        blocks.push(AttachmentBlock {
            label: &after[..quote],
            text: body[..end].trim(),
        });
        rest = &body[end + ATTACHMENT_CLOSE.len()..];
    }
    if blocks.is_empty() {
        return (blocks, text);
    }
    (blocks, rest.trim_start())
}

/// The bubble's text of a user's message: without a plugin's attachments and the mentions at its
/// ends (they are chips) — what the bubble shows and ⌘F searches.
pub(crate) fn bubble_text(text: &str) -> &str {
    edge_mentions(leading_attachments(text).1).1
}

/// The start of an attachment's text for its chip's tooltip.
fn attachment_preview(text: &str) -> String {
    const MAX_LINES: usize = 12;
    const MAX_CHARS: usize = 600;
    let mut preview = String::new();
    for (index, line) in text.lines().enumerate() {
        if index == MAX_LINES || preview.chars().count() + line.chars().count() > MAX_CHARS {
            preview.push('…');
            break;
        }
        if index > 0 {
            preview.push('\n');
        }
        preview.push_str(line);
    }
    preview
}

/// A mention at an end of a message (a chip above the bubble).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Mention<'a> {
    pub raw: &'a str,
    pub path: &'a str,
    pub lines: Option<(usize, usize)>,
}

/// The mentions at the start and the end of a message, and the text between them: ⌥⌘K and "Fix
/// with Claude" put the file and the lines first; mentions inside a sentence stay in it.
pub(crate) fn edge_mentions(text: &str) -> (Vec<Mention<'_>>, &str) {
    let segments = split_mentions(text);
    let is_space =
        |segment: &Segment| matches!(segment, Segment::Text(text) if text.trim().is_empty());
    let mut start = 0;
    let mut chips = Vec::new();
    while start < segments.len() {
        if let Some(chip) = mention(&segments[start]) {
            chips.push(chip);
        } else if !is_space(&segments[start]) {
            break;
        }
        start += 1;
    }
    let mut end = segments.len();
    let mut tail = Vec::new();
    while end > start {
        if let Some(chip) = mention(&segments[end - 1]) {
            tail.push(chip);
        } else if !is_space(&segments[end - 1]) {
            break;
        }
        end -= 1;
    }
    tail.reverse();
    chips.extend(tail);
    // The text between the first and the last segment kept, as it is in the message.
    let rest = match (
        segments.get(start),
        end.checked_sub(1).and_then(|last| segments.get(last)),
    ) {
        (Some(first), Some(last)) if start < end => {
            let from = segment_offset(text, first);
            let to = segment_offset(text, last) + segment_text(last).len();
            &text[from..to]
        }
        _ => "",
    };
    (chips, rest.trim())
}

/// A segment as a chip, when it is a mention.
fn mention<'a>(segment: &Segment<'a>) -> Option<Mention<'a>> {
    match *segment {
        Segment::Mention { raw, path, lines } => Some(Mention { raw, path, lines }),
        Segment::Text(_) => None,
    }
}

fn segment_text<'a>(segment: &Segment<'a>) -> &'a str {
    match *segment {
        Segment::Text(text) => text,
        Segment::Mention { raw, .. } => raw,
    }
}

/// Where a segment (a slice of `text`) starts in it.
fn segment_offset(text: &str, segment: &Segment) -> usize {
    segment_text(segment).as_ptr() as usize - text.as_ptr() as usize
}

/// The file a tool call is about.
trait CallPath {
    fn call_path(&self) -> Option<&Path>;
}

impl CallPath for ToolEntry {
    fn call_path(&self) -> Option<&Path> {
        match &self.call {
            ToolCall::Read { path, .. }
            | ToolCall::Edit { path, .. }
            | ToolCall::Write { path, .. }
            | ToolCall::NotebookEdit { path } => {
                Some(path.as_path()).filter(|path| !path.as_os_str().is_empty())
            }
            _ => None,
        }
    }
}

// --- Small elements ---

/// Claude's avatar: its spark on a circle of its own orange (the author: unmistakably Claude),
/// a shade deeper towards the corner.
fn avatar(ui: UiColors) -> AnyElement {
    let deeper = gpui::Hsla {
        l: (ui.claude.l - 0.08).max(0.),
        ..ui.claude
    };
    div()
        .size(px(AVATAR))
        .rounded(px(AVATAR / 2.))
        .flex()
        .items_center()
        .justify_center()
        .bg(linear_gradient(
            135.,
            linear_color_stop(ui.claude, 0.),
            linear_color_stop(deeper, 1.),
        ))
        .shadow(vec![BoxShadow {
            color: UiColors::tint(ui.claude, 0.35),
            offset: point(px(0.), px(2.)),
            blur_radius: px(8.),
            spread_radius: px(0.),
        }])
        .child(icon(IconName::Claude, ui.on_accent).size(px(14.)))
        .into_any_element()
}

/// A subagent's avatar in its conversation: the agent icon on a blue-to-indigo gradient.
fn agent_avatar(ui: UiColors) -> AnyElement {
    div()
        .size(px(AVATAR))
        .rounded(px(AVATAR / 2.))
        .flex()
        .items_center()
        .justify_center()
        .bg(linear_gradient(
            135.,
            linear_color_stop(ui.blue, 0.),
            linear_color_stop(ui.accent, 1.),
        ))
        .child(icon(IconName::Agent, ui.on_accent).size(px(14.)))
        .into_any_element()
}

/// A card's tile: the step's icon on its tint (a pulse while it runs).
fn tile(
    name: IconName,
    color: Hsla,
    running: bool,
    pulse_id: impl Into<gpui::ElementId>,
    ui: UiColors,
) -> AnyElement {
    div()
        .flex_none()
        .size(px(22.))
        .rounded(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .bg(UiColors::tint(color, 0.14))
        .child(if running {
            pulse(pulse_id, ui).into_any_element()
        } else {
            icon(name, color).size(px(13.)).into_any_element()
        })
        .into_any_element()
}

/// The `!` of a shell command.
fn bang_badge(ui: UiColors) -> AnyElement {
    div()
        .flex_none()
        .h(px(20.))
        .px(px(6.))
        .flex()
        .items_center()
        .rounded(px(5.))
        .bg(UiColors::tint(ui.success, 0.16))
        .font_family(theme::code_font())
        .font_weight(FontWeight::BOLD)
        .text_size(px(12.))
        .text_color(ui.success)
        .child("!")
        .into_any_element()
}

/// "+4 −1": added lines in green, removed in red.
fn counts_label(added: usize, removed: usize, ui: UiColors) -> AnyElement {
    div()
        .flex_none()
        .flex()
        .gap(px(4.))
        .text_size(px(theme::TEXT_SM))
        .font_family(theme::code_font())
        .when(added > 0, |row| {
            row.child(div().text_color(ui.diff_added).child(format!("+{added}")))
        })
        .when(removed > 0, |row| {
            row.child(div().text_color(ui.error).child(format!("−{removed}")))
        })
        .into_any_element()
}

/// A small square button with an icon (22 px), for rows and cards.
fn small_button(id: impl Into<gpui::ElementId>, name: IconName, ui: UiColors) -> Stateful<Div> {
    small_button_frame(id, ui).child(icon(name, ui.text_muted).size(px(13.)))
}

/// [`small_button`] without its icon.
fn small_button_frame(id: impl Into<gpui::ElementId>, ui: UiColors) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(ui::RADIUS_XS + 1.))
        .cursor_pointer()
        .hover(move |style| style.bg(ui.hover))
}

/// The copy button of entry `id` was pressed: it shows its check mark for a moment
/// ([`ui::Copied`]).
fn note_copied(chat: &mut ClaudeChat, id: EntryId, cx: &mut Context<ClaudeChat>) {
    let mut copied = ui::Copied::default();
    copied.now(cx);
    chat.shown_transcript_mut().copied = Some((id, copied));
}

/// The icon of a copy button, `size` pixels: the copy icon (`idle`) and, a moment after a click,
/// the check mark (`done`) crossfade ([`motion::FADE`]). The button's id keeps the fade.
fn copy_icon(copied: bool, size: f32, idle: Hsla, done: Hsla) -> AnyElement {
    let shown = if copied { 1. } else { 0. };
    let fade = |icon: Svg, value: f32| icon.opacity(value);
    div()
        .relative()
        .flex_none()
        .size(px(size))
        .child(icon(IconName::Copy, idle).size(px(size)).with_transition(
            "copy-idle",
            1. - shown,
            motion::FADE,
            fade,
        ))
        .child(div().absolute().top_0().left_0().size(px(size)).child(
            icon(IconName::Check, done).size(px(size)).with_transition(
                "copy-done",
                shown,
                motion::FADE,
                fade,
            ),
        ))
        .into_any_element()
}

/// A link in the accent color (a card's action).
fn link(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    ui: UiColors,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .text_size(px(theme::TEXT_SM))
        .text_color(ui.accent_text)
        .cursor_pointer()
        .hover(|style| style.underline())
        .child(label.into())
}

/// A pulsing dot: something runs. With animations off it shows steady.
fn pulse(id: impl Into<gpui::ElementId>, ui: UiColors) -> AnyElement {
    let dot = div().flex_none().size(px(7.)).rounded(px(4.)).bg(ui.accent);
    if !motion::enabled() {
        return dot.into_any_element();
    }
    dot.with_animation(
        id,
        Animation::new(Duration::from_millis(900)).repeat(),
        |dot, delta| dot.opacity(0.3 + 0.7 * (1. - (2. * delta - 1.).abs())),
    )
    .into_any_element()
}

/// A row's chevron: points right, and turns down as the row opens (`POP`). The row's id scopes
/// the turn.
fn chevron(expanded: bool, ui: UiColors) -> impl IntoElement {
    icon(IconName::ChevronRight, ui.dim)
        .size(px(12.))
        .with_transition(
            "chevron",
            if expanded { 1. } else { 0. },
            motion::POP,
            |chevron, turn: f32| {
                chevron.with_transformation(Transformation::rotate(radians(
                    turn * std::f32::consts::FRAC_PI_2,
                )))
            },
        )
}

/// The state of a tool call at the row's end; nothing for a call that went well.
fn state_mark(state: ToolState, ui: UiColors) -> Option<AnyElement> {
    let (label, color) = match state {
        ToolState::Waiting => (tr("waiting for your answer"), ui.warning),
        ToolState::Failed => (tr("failed"), ui.error),
        ToolState::Denied => (tr("refused"), ui.warning),
        ToolState::Interrupted => (tr("stopped"), ui.dim),
        ToolState::Streaming | ToolState::Running | ToolState::Done => return None,
    };
    Some(
        div()
            .flex_none()
            .text_size(px(theme::TEXT_XS))
            .text_color(color)
            .child(label)
            .into_any_element(),
    )
}

fn tool_color(state: ToolState, ui: UiColors) -> Hsla {
    match state {
        ToolState::Done => ui.text_muted,
        ToolState::Failed => ui.error,
        ToolState::Denied | ToolState::Waiting => ui.warning,
        ToolState::Interrupted => ui.dim,
        ToolState::Streaming | ToolState::Running => ui.accent_text,
    }
}

/// Text in the code font on a quiet background.
fn code_box(text: &str, color: Hsla, ui: UiColors) -> gpui::Div {
    div()
        .px_2()
        .py_1p5()
        .rounded(px(RADIUS_SM))
        .bg(UiColors::tint(ui.foreground, 0.04))
        .font_family(theme::code_font())
        .text_size(px(12.))
        .line_height(px(18.))
        .text_color(color)
        .child(text.to_string())
}

fn note(text: &str, color: Hsla) -> gpui::Div {
    div()
        .text_size(px(theme::TEXT_XS))
        .text_color(color)
        .child(text.to_string())
}

fn notice_row(
    name: IconName,
    label: impl Into<SharedString>,
    color: Hsla,
    ui: UiColors,
) -> AnyElement {
    div()
        .min_h(px(ROW_HEIGHT))
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(px(theme::TEXT_SM))
        .text_color(if color == ui.dim {
            ui.dim
        } else {
            ui.text_muted
        })
        .child(
            div()
                .flex_none()
                .w(px(16.))
                .flex()
                .justify_center()
                .child(icon(name, color).size(px(13.))),
        )
        .child(div().min_w_0().child(label.into()))
        .into_any_element()
}

/// A line across the conversation with a label in the middle.
fn separator(label: String, ui: UiColors) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_size(px(theme::TEXT_XS))
        .text_color(ui.dim)
        .child(div().flex_1().h(px(1.)).bg(ui.divider))
        .child(label)
        .child(div().flex_1().h(px(1.)).bg(ui.divider))
        .into_any_element()
}

/// A button of a notice: its label and the action it dispatches.
type NoticeAction = Option<(SharedString, Box<dyn gpui::Action>)>;

/// A box for something that went wrong: a title, an explanation, details in the code font, an
/// action button.
fn callout(
    name: IconName,
    title: String,
    body: Option<SharedString>,
    details: Option<String>,
    color: Hsla,
    action: NoticeAction,
    ui: UiColors,
) -> AnyElement {
    let action = action.map(|(label, action)| {
        ui::primary_button("claude-notice-action", label, true, ui)
            .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
            .into_any_element()
    });
    callout_box(
        icon(name, color).size(px(14.)).into_any_element(),
        title,
        body,
        details,
        color,
        action,
        ui,
    )
}

/// [`callout`] with any mark before the title (a country's flag) and any button.
fn callout_box(
    lead: AnyElement,
    title: String,
    body: Option<SharedString>,
    details: Option<String>,
    color: Hsla,
    action: Option<AnyElement>,
    ui: UiColors,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .p(px(10.))
        .rounded(px(RADIUS_MD))
        .bg(UiColors::tint(color, 0.08))
        .border_1()
        .border_color(UiColors::tint(color, 0.3))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(theme::TEXT_SM))
                .font_weight(FontWeight::MEDIUM)
                .child(lead)
                .child(title),
        )
        .children(body.map(|body| {
            div()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.text_muted)
                .child(body)
        }))
        .children(details.map(|details| {
            let (shown, _) = clip_lines(&details, CLIP_LINES);
            code_box(shown, ui.text_muted, ui)
        }))
        .children(action.map(|action| div().flex().child(action)))
        .into_any_element()
}

/// The line under the conversation while Claude works: what it does, for how long, Esc to stop;
/// while a question waits — that it waits for the user. It keeps to Claude's side of the column.
fn activity_line(model: &Session, ui: UiColors) -> Option<AnyElement> {
    let row = || {
        div()
            .flex_1()
            .min_w_0()
            .h(px(30.))
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(theme::TEXT_SM))
            .text_color(ui.text_muted)
    };
    let line = match &model.status {
        Status::Working { since, activity } => {
            let (label, tokens) = activity_label(activity, model);
            let mut details = vec![format_elapsed(since.elapsed())];
            if let Some(tokens) = tokens.filter(|tokens| *tokens > 0) {
                details.push(trf("{0} tokens", &[&format_tokens(tokens)]));
            }
            row()
                .child(pulse("claude-activity", ui))
                .child(div().flex_none().text_color(ui.foreground).child(label))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(ui.dim)
                        .child(details.join(" · ")),
                )
                .child(div().flex_1())
                .child(ui::keycap("Esc", ui))
                .child(
                    div()
                        .flex_none()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(ui.dim)
                        .child(tr("to stop")),
                )
        }
        Status::WaitingForUser => row()
            .child(icon(IconName::Question, ui.warning).size(px(14.)))
            .child(
                div()
                    .text_color(ui.warning)
                    .child(tr("Waiting for your answer")),
            ),
        Status::Starting if !model.entries.is_empty() => row()
            .child(pulse("claude-starting", ui))
            .child(tr("Starting Claude…")),
        _ => return None,
    };
    Some(
        column(
            div()
                .flex()
                .gap(px(AVATAR_GAP))
                .child(div().flex_none().w(px(AVATAR)))
                .child(line),
        )
        .flex_none()
        .into_any_element(),
    )
}

/// Under a subagent's conversation while it works: the tool it runs (or that it thinks), its
/// count of tool uses and tokens so far (part 10.2).
fn agent_activity_line(tool: &ToolEntry, ui: UiColors) -> Option<AnyElement> {
    if !agent_running(tool) {
        return None;
    }
    let running = tool
        .children
        .iter()
        .rev()
        .find_map(|child| match &child.kind {
            EntryKind::Tool(call)
                if matches!(call.state, ToolState::Streaming | ToolState::Running) =>
            {
                Some(call.name.clone())
            }
            _ => None,
        });
    let label = match running.or_else(|| tool.task.as_ref()?.last_tool.clone()) {
        Some(name) => trf("Running {0}…", &[&tool_display_name(&name)]),
        None => tr("Thinking…").to_string(),
    };
    let details = agent_progress(tool);
    Some(
        column(
            div()
                .flex()
                .gap(px(AVATAR_GAP))
                .child(div().flex_none().w(px(AVATAR)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h(px(30.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.text_muted)
                        .child(pulse("claude-agent-activity", ui))
                        .child(div().flex_none().text_color(ui.foreground).child(label))
                        .children(details.map(|details| {
                            div().min_w_0().truncate().text_color(ui.dim).child(details)
                        })),
                ),
        )
        .flex_none()
        .into_any_element(),
    )
}

/// The task Claude gave a subagent, on top of the subagent's conversation (part 10.2).
fn agent_task(tool: &ToolEntry, ui: UiColors) -> AnyElement {
    let prompt = match &tool.call {
        ToolCall::Agent { prompt, .. } => prompt.as_str(),
        _ => "",
    };
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .px(px(12.))
        .py(px(9.))
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_dashed()
        .border_color(UiColors::tint(ui.accent, 0.35))
        .bg(UiColors::tint(ui.accent, 0.05))
        .child(ui::section_label(tr("Task from Claude"), ui))
        .child(
            div()
                .text_size(px(theme::TEXT_MD))
                .text_color(ui.foreground)
                .child(clip_chars(prompt.trim(), 2000)),
        )
        .into_any_element()
}

/// What Claude does, and the tokens to show next to it.
fn activity_label(activity: &Activity, model: &Session) -> (String, Option<u64>) {
    match activity {
        Activity::Thinking { tokens } => (tr("Thinking…").to_string(), Some(*tokens)),
        Activity::Responding => (tr("Writing…").to_string(), None),
        Activity::Tool(name) => (trf("Running {0}…", &[&tool_display_name(name)]), None),
        Activity::Compacting => (tr("Compacting the conversation…").to_string(), None),
        Activity::Retrying { attempt, max } => {
            (trf("Retrying ({0} of {1})…", &[attempt, max]), None)
        }
        Activity::Requesting => {
            let label = if model.pending.is_empty() {
                tr("Working…")
            } else {
                tr("Waiting for your answer")
            };
            (label.to_string(), None)
        }
    }
}

/// An empty session: what to ask, and how.
fn empty_state(starting: bool, ui: UiColors) -> AnyElement {
    let tip = |keys: AnyElement, text: &str| {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_none()
                    .w(px(64.))
                    .flex()
                    .justify_end()
                    .child(keys),
            )
            .child(div().text_color(ui.text_muted).child(text.to_string()))
    };
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(14.))
        .px(px(24.))
        .text_size(px(theme::TEXT_SM))
        .child(
            icon(
                IconName::Claude,
                if starting { ui.dim } else { ui.accent_text },
            )
            .size(px(28.)),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_LG))
                .text_color(ui.foreground)
                .child(if starting {
                    tr("Starting Claude…")
                } else {
                    tr("Ask Claude about this project")
                }),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(tip(
                    ui::keys("⌥⌘K", ui).into_any_element(),
                    tr("Add the editor's selection"),
                ))
                .child(tip(
                    ui::keycap("@", ui).into_any_element(),
                    tr("Mention a file"),
                ))
                .child(tip(
                    ui::keycap("/", ui).into_any_element(),
                    tr("Commands and skills"),
                ))
                .child(tip(
                    ui::keycap("Esc", ui).into_any_element(),
                    tr("Stop Claude"),
                )),
        )
        .into_any_element()
}

// --- Pure helpers ---

/// A piece of the user's message: text, or a file mention (`@src/a.rs#L3-9`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Segment<'a> {
    Text(&'a str),
    Mention {
        raw: &'a str,
        path: &'a str,
        /// 1-based, inclusive.
        lines: Option<(usize, usize)>,
    },
}

/// Splits the text at file mentions: `@` at the start or after whitespace, then the path up to
/// whitespace, with an optional `#L3` or `#L3-9`; punctuation that ends a sentence stays text.
pub(crate) fn split_mentions(text: &str) -> Vec<Segment<'_>> {
    let mut segments = Vec::new();
    let mut plain_start = 0;
    let mut index = 0;
    let bytes = text.as_bytes();
    while index < text.len() {
        let at_word_start = index == 0 || bytes[index - 1].is_ascii_whitespace();
        if bytes[index] == b'@' && at_word_start {
            let end = text[index..]
                .find(char::is_whitespace)
                .map_or(text.len(), |offset| index + offset);
            let raw =
                text[index..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '"', '\'']);
            let (path, lines) = match raw[1..].split_once("#L") {
                Some((path, range)) => {
                    let mut numbers = range.splitn(2, '-').map(|n| n.parse::<usize>().ok());
                    match (numbers.next().flatten(), numbers.next()) {
                        (Some(first), None) => (path, Some((first, first))),
                        (Some(first), Some(Some(last))) => (path, Some((first, last.max(first)))),
                        _ => (&raw[1..], None),
                    }
                }
                None => (&raw[1..], None),
            };
            if !path.is_empty() {
                if plain_start < index {
                    segments.push(Segment::Text(&text[plain_start..index]));
                }
                let raw = &text[index..index + raw.len()];
                segments.push(Segment::Mention { raw, path, lines });
                index += raw.len();
                plain_start = index;
                continue;
            }
        }
        index += text[index..].chars().next().map_or(1, char::len_utf8);
    }
    if plain_start < text.len() {
        segments.push(Segment::Text(&text[plain_start..]));
    }
    segments
}

/// What a tool row says: the icon, the title, the detail (a path, a command…), a note at the end
/// ("+4 −1"), and the line to open the file at.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolSummary {
    pub icon: IconName,
    pub title: SharedString,
    pub detail: String,
    /// The detail is code (a command, a pattern).
    pub code: bool,
    pub meta: Option<String>,
    /// Zero-based.
    pub line: Option<usize>,
}

pub(crate) fn tool_summary(
    tool: &ToolEntry,
    root: &Path,
    tasks: &[flux_claude::TaskItem],
) -> ToolSummary {
    let structured = tool
        .result
        .as_ref()
        .and_then(|result| result.structured.as_ref());
    let summary = |icon, title: &str, detail: String| ToolSummary {
        icon,
        title: title.to_string().into(),
        detail,
        code: false,
        meta: None,
        line: None,
    };
    match &tool.call {
        ToolCall::Read {
            path,
            offset,
            limit,
        } => {
            let file = structured.map(|result| &result["file"]);
            let start = file
                .and_then(|file| file["startLine"].as_u64())
                .or(*offset)
                .map(|line| line.max(1) as usize);
            let count = file
                .and_then(|file| file["numLines"].as_u64())
                .or(*limit)
                .map(|count| count as usize);
            let total = file.and_then(|file| file["totalLines"].as_u64());
            let meta = match (start, count, total) {
                (Some(start), Some(count), Some(total)) if count as u64 >= total && start == 1 => {
                    Some(trn(count, "{n} line", "{n} lines"))
                }
                (Some(start), Some(count), _) if count > 0 => {
                    Some(trf("lines {0}–{1}", &[&start, &(start + count - 1)]))
                }
                _ => None,
            };
            ToolSummary {
                meta,
                line: start.map(|start| start - 1),
                ..summary(IconName::File, "Read", relative(path, root))
            }
        }
        ToolCall::Edit { path, .. } => {
            let first = structured.and_then(first_changed_line);
            ToolSummary {
                meta: structured
                    .and_then(patch_counts)
                    .map(|(added, removed)| counts(added, removed)),
                line: first,
                ..summary(IconName::Pencil, "Edit", relative(path, root))
            }
        }
        ToolCall::Write { path, content } => {
            let created = structured.is_some_and(|result| result["type"] == "create");
            let meta = match structured.and_then(patch_counts) {
                Some((added, removed)) if !created && (added > 0 || removed > 0) => {
                    Some(counts(added, removed))
                }
                _ if tool.state == ToolState::Done => Some(trn(
                    content.lines().count(),
                    "new file · {n} line",
                    "new file · {n} lines",
                )),
                _ => None,
            };
            ToolSummary {
                meta,
                ..summary(IconName::FilePlus, "Write", relative(path, root))
            }
        }
        ToolCall::NotebookEdit { path } => {
            summary(IconName::Pencil, "NotebookEdit", relative(path, root))
        }
        ToolCall::Bash {
            command,
            description,
            background,
        } => ToolSummary {
            code: true,
            meta: background
                .then(|| tr("in the background").to_string())
                .or_else(|| {
                    description
                        .as_ref()
                        .filter(|_| command.contains('\n'))
                        .cloned()
                }),
            ..summary(IconName::Terminal, "Bash", first_line(command))
        },
        ToolCall::Grep { pattern, path } => ToolSummary {
            code: true,
            meta: path.as_ref().map(|path| relative(Path::new(path), root)),
            ..summary(IconName::Search, "Grep", pattern.clone())
        },
        ToolCall::Glob { pattern } => ToolSummary {
            code: true,
            ..summary(IconName::Search, "Glob", pattern.clone())
        },
        ToolCall::WebFetch { url } => summary(IconName::Globe, "Fetch", url.clone()),
        ToolCall::WebSearch { query } => summary(IconName::Globe, "Web Search", query.clone()),
        ToolCall::Agent {
            description,
            subagent_type,
            background,
            ..
        } => ToolSummary {
            meta: if *background {
                Some(tr("in the background").to_string())
            } else {
                subagent_type
                    .clone()
                    .filter(|kind| kind != "general-purpose")
            },
            ..summary(IconName::Agent, tr("Agent"), description.clone())
        },
        ToolCall::TaskCreate { subject } => {
            summary(IconName::Checklist, tr("Task"), subject.clone())
        }
        ToolCall::TaskUpdate { task_id, status } => {
            let status = match status.as_deref() {
                Some("in_progress") => tr("in progress").to_string(),
                Some("completed") => tr("done").to_string(),
                Some("deleted") => tr("deleted").to_string(),
                Some(other) => other.to_string(),
                None => tr("updated").to_string(),
            };
            let task = tasks
                .iter()
                .find(|task| &task.id == task_id)
                .map_or_else(|| format!("#{task_id}"), |task| task.subject.clone());
            summary(
                IconName::Checklist,
                tr("Task"),
                format!("{task} · {status}"),
            )
        }
        ToolCall::TodoWrite { todos } => summary(
            IconName::Checklist,
            tr("Tasks"),
            trn(todos.len(), "{n} task", "{n} tasks"),
        ),
        ToolCall::AskUserQuestion { questions } => {
            let answer = structured
                .and_then(|result| result["answers"].as_object())
                .and_then(|answers| answers.values().next())
                .and_then(Value::as_str)
                .map(str::to_string);
            ToolSummary {
                meta: answer,
                ..summary(
                    IconName::Question,
                    tr("Question"),
                    questions
                        .first()
                        .map(|question| question.question.clone())
                        .unwrap_or_default(),
                )
            }
        }
        ToolCall::ExitPlanMode { .. } => summary(IconName::Plan, tr("Plan"), String::new()),
        ToolCall::EnterPlanMode => summary(IconName::Plan, tr("Plan mode"), String::new()),
        ToolCall::Skill { name } => summary(IconName::Sparkle, "Skill", name.clone()),
        ToolCall::Mcp { server, tool: name } => ToolSummary {
            meta: Some(server.clone()),
            ..summary(IconName::Plug, name, compact_json(&tool.input, 120))
        },
        ToolCall::Other => ToolSummary {
            code: true,
            ..summary(
                IconName::Command,
                &tool.name,
                compact_json(&tool.input, 120),
            )
        },
    }
}

/// A tool's name as the activity line says it ("Running Bash…").
fn tool_display_name(name: &str) -> String {
    match name {
        "WebFetch" => "Fetch".into(),
        "WebSearch" => "Web Search".into(),
        "Task" => "Agent".into(),
        other => match other
            .strip_prefix("mcp__")
            .and_then(|rest| rest.split_once("__"))
        {
            Some((_, tool)) => tool.to_string(),
            None => other.to_string(),
        },
    }
}

fn counts(added: usize, removed: usize) -> String {
    format!("+{added} −{removed}")
}

/// A line of an edit's change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PatchLine {
    pub kind: LineKind,
    /// 1-based numbers in the old and the new text.
    pub old: Option<usize>,
    pub new: Option<usize>,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineKind {
    Context,
    Added,
    Removed,
    /// Lines skipped between two hunks.
    Gap,
}

/// The lines of `structuredPatch` (`[{oldStart, newStart, lines: [" ctx", "-old", "+new"]}]`),
/// numbered, with a gap between hunks.
pub(crate) fn patch_lines(structured: &Value) -> Vec<PatchLine> {
    let mut lines = Vec::new();
    for (index, hunk) in structured["structuredPatch"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        if index > 0 {
            lines.push(PatchLine {
                kind: LineKind::Gap,
                old: None,
                new: None,
                text: String::new(),
            });
        }
        let mut old = hunk["oldStart"].as_u64().unwrap_or(1) as usize;
        let mut new = hunk["newStart"].as_u64().unwrap_or(1) as usize;
        for line in hunk["lines"].as_array().into_iter().flatten() {
            let line = line.as_str().unwrap_or("");
            let (kind, text) = match line.chars().next() {
                Some('+') => (LineKind::Added, &line[1..]),
                Some('-') => (LineKind::Removed, &line[1..]),
                Some(' ') => (LineKind::Context, &line[1..]),
                // "\ No newline at end of file" and the like.
                Some('\\') => continue,
                _ => (LineKind::Context, line),
            };
            let (old_number, new_number) = match kind {
                LineKind::Added => {
                    new += 1;
                    (None, Some(new - 1))
                }
                LineKind::Removed => {
                    old += 1;
                    (Some(old - 1), None)
                }
                _ => {
                    old += 1;
                    new += 1;
                    (Some(old - 1), Some(new - 1))
                }
            };
            lines.push(PatchLine {
                kind,
                old: old_number,
                new: new_number,
                text: text.to_string(),
            });
        }
    }
    lines
}

/// How many lines an edit added and removed.
pub(crate) fn patch_counts(structured: &Value) -> Option<(usize, usize)> {
    let hunks = structured["structuredPatch"].as_array()?;
    let mut added = 0;
    let mut removed = 0;
    for line in hunks
        .iter()
        .flat_map(|hunk| hunk["lines"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
    {
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    Some((added, removed))
}

/// The first line an edit changed (zero-based), to open the file there.
fn first_changed_line(structured: &Value) -> Option<usize> {
    patch_lines(structured)
        .into_iter()
        .find(|line| matches!(line.kind, LineKind::Added | LineKind::Removed))
        .and_then(|line| line.new.or(line.old))
        .map(|line| line.saturating_sub(1))
}

/// The row of a thought: "Thinking…" while it streams; then how long it took — or just
/// "Thought" when that is unknown (it came whole: a saved conversation, a subagent) or under a
/// second (no "0 s" or milliseconds).
fn thinking_label(streaming: bool, tokens: u64, duration: Option<Duration>) -> String {
    if streaming {
        return if tokens > 0 {
            trf("Thinking… {0} tokens", &[&format_tokens(tokens)])
        } else {
            tr("Thinking…").to_string()
        };
    }
    match duration.filter(|duration| duration.as_secs() > 0) {
        Some(duration) => trf("Thought for {0}", &[&format_duration(duration)]),
        None => tr("Thought").to_string(),
    }
}

/// A live clock, as the CLI's: whole seconds from "0 s" ("0 s", "12 s", "1 min 5 s") — no
/// milliseconds flickering through the first second.
pub(crate) fn format_elapsed(elapsed: Duration) -> String {
    match elapsed.as_secs() {
        0 => trf("{0} s", &[&0]),
        _ => format_duration(elapsed),
    }
}

/// "350 ms", "12 s", "1 min 5 s", "1 h 2 min": how long something finished took.
pub(crate) fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds == 0 {
        return trf("{0} ms", &[&duration.as_millis()]);
    }
    if seconds < 60 {
        return trf("{0} s", &[&seconds]);
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        let rest = seconds % 60;
        if rest == 0 {
            return trf("{0} min", &[&minutes]);
        }
        return trf("{0} min {1} s", &[&minutes, &rest]);
    }
    trf("{0} h {1} min", &[&(minutes / 60), &(minutes % 60)])
}

/// "950", "1.2k", "35k", "1.2M".
pub(crate) fn format_tokens(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..10_000 => format!("{:.1}k", tokens as f64 / 1_000.),
        10_000..1_000_000 => format!("{}k", tokens / 1_000),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.),
    }
}

/// A path relative to the project root when it is inside it.
pub(crate) fn relative(path: &Path, root: &Path) -> String {
    let path: PathBuf = path.into();
    match path.strip_prefix(root) {
        Ok(relative) if !relative.as_os_str().is_empty() => relative.to_string_lossy().into_owned(),
        _ => path.to_string_lossy().into_owned(),
    }
}

/// At most `max` lines, and how many were left out.
pub(crate) fn clip_lines(text: &str, max: usize) -> (&str, usize) {
    let total = text.lines().count();
    if total <= max {
        return (text, 0);
    }
    let end = text
        .match_indices('\n')
        .nth(max - 1)
        .map_or(text.len(), |(index, _)| index);
    (&text[..end], total - max)
}

fn clip_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((index, _)) => format!("{}…", text[..index].trim_end()),
        None => text.to_string(),
    }
}

/// Markdown as plain text: the text of its spans, without the markers (`**kiwi**` → `kiwi`).
fn plain_text(markdown: &str) -> String {
    markdown::parse_inline(markdown)
        .into_iter()
        .map(|span| span.text)
        .collect()
}

/// Whether an answer to Claude's question reads whole in its row ([`ANSWER_IN_ROW`]).
fn fits_in_row(answer: &str) -> bool {
    answer.chars().count() <= ANSWER_IN_ROW && !answer.contains('\n')
}

/// The first `count` lines, joined (an answer in short).
fn first_lines(text: &str, count: usize) -> String {
    text.trim()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(count)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if text.trim().lines().nth(1).is_some() {
        format!("{line} …")
    } else {
        line.to_string()
    }
}

fn compact_json(value: &Value, max: usize) -> String {
    if value.is_null() {
        return String::new();
    }
    clip_chars(&value.to_string(), max)
}

/// "https://docs.rs/gpui/latest" → "docs.rs".
fn host(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_claude::session::ToolResult;
    use serde_json::json;

    fn tool(name: &str, input: Value, result: Option<Value>) -> ToolEntry {
        ToolEntry {
            tool_use_id: "t1".into(),
            name: name.into(),
            call: ToolCall::parse(name, &input),
            input,
            state: ToolState::Done,
            result: result.map(|structured| ToolResult {
                text: String::new(),
                is_error: false,
                images: Vec::new(),
                structured: Some(structured),
            }),
            children: Vec::new(),
            task: None,
        }
    }

    #[test]
    fn a_background_command_shows_its_task() {
        let model = Session::new(PathBuf::from("/project"));
        let mut bash = tool(
            "Bash",
            json!({"command": "sleep 3", "run_in_background": true}),
            None,
        );
        // An old session's call: nothing known about its task.
        assert_eq!(background_run(&model, &bash), None);
        let ended = |status: &str, summary: Option<&str>| flux_claude::session::TaskProgress {
            status: Some(status.into()),
            summary: summary.map(String::from),
            background: true,
            ..Default::default()
        };
        // Running, but the session doesn't list it (the process is gone): not shown as running.
        bash.task = Some(ended("running", None));
        assert_eq!(background_run(&model, &bash), None);
        bash.task = Some(ended(
            "completed",
            Some("Background command \"Sleep\" completed (exit code 2)"),
        ));
        assert_eq!(
            background_run(&model, &bash),
            Some(BackgroundRun::Exited(2))
        );
        bash.task = Some(ended("completed", None));
        assert_eq!(
            background_run(&model, &bash),
            Some(BackgroundRun::Exited(0))
        );
        bash.task = Some(ended("killed", None));
        assert_eq!(background_run(&model, &bash), Some(BackgroundRun::Stopped));
        bash.task = Some(ended("failed", None));
        assert_eq!(background_run(&model, &bash), Some(BackgroundRun::Failed));
    }

    #[test]
    fn mentions_split_out_of_the_text() {
        let segments = split_mentions("fix @src/a.rs#L3-9, and @b.rs please; mail me@x.com");
        assert_eq!(
            segments,
            [
                Segment::Text("fix "),
                Segment::Mention {
                    raw: "@src/a.rs#L3-9",
                    path: "src/a.rs",
                    lines: Some((3, 9))
                },
                Segment::Text(", and "),
                Segment::Mention {
                    raw: "@b.rs",
                    path: "b.rs",
                    lines: None
                },
                Segment::Text(" please; mail me@x.com"),
            ]
        );
        assert_eq!(
            split_mentions("@one#L7"),
            [Segment::Mention {
                raw: "@one#L7",
                path: "one",
                lines: Some((7, 7))
            }]
        );
        assert_eq!(split_mentions("just @"), [Segment::Text("just @")]);
    }

    #[test]
    fn patches_are_numbered_with_gaps_between_hunks() {
        let structured = json!({ "structuredPatch": [
            { "oldStart": 3, "newStart": 3, "lines": [" a", "-b", "+B", "+C", " d"] },
            { "oldStart": 20, "newStart": 21, "lines": ["-x", "\\ No newline at end of file"] },
        ]});
        let lines = patch_lines(&structured);
        let kinds: Vec<LineKind> = lines.iter().map(|line| line.kind).collect();
        assert_eq!(
            kinds,
            [
                LineKind::Context,
                LineKind::Removed,
                LineKind::Added,
                LineKind::Added,
                LineKind::Context,
                LineKind::Gap,
                LineKind::Removed
            ]
        );
        assert_eq!((lines[1].old, lines[1].new), (Some(4), None));
        assert_eq!((lines[3].old, lines[3].new), (None, Some(5)));
        assert_eq!((lines[4].old, lines[4].new), (Some(5), Some(6)));
        assert_eq!(lines[6].old, Some(20));
        assert_eq!(patch_counts(&structured), Some((2, 2)));
        assert_eq!(first_changed_line(&structured), Some(3));
    }

    #[test]
    fn rows_summarize_their_calls() {
        let root = Path::new("/p");
        let read = tool(
            "Read",
            json!({ "file_path": "/p/src/main.rs" }),
            Some(
                json!({ "type": "text", "file": { "startLine": 10, "numLines": 31, "totalLines": 200 } }),
            ),
        );
        let summary = tool_summary(&read, root, &[]);
        assert_eq!(summary.detail, "src/main.rs");
        assert_eq!(summary.meta.as_deref(), Some("lines 10–40"));
        assert_eq!(summary.line, Some(9));
        let edit = tool(
            "Edit",
            json!({ "file_path": "/p/a.rs", "old_string": "x", "new_string": "y" }),
            Some(
                json!({ "structuredPatch": [{ "oldStart": 1, "newStart": 1, "lines": ["-x", "+y"] }] }),
            ),
        );
        assert_eq!(
            tool_summary(&edit, root, &[]).meta.as_deref(),
            Some("+1 −1")
        );
        let bash = tool(
            "Bash",
            json!({ "command": "cargo test\ncargo clippy" }),
            None,
        );
        let summary = tool_summary(&bash, root, &[]);
        assert_eq!(summary.detail, "cargo test …");
        assert!(summary.code);
        let outside = tool("Read", json!({ "file_path": "/etc/hosts" }), None);
        assert_eq!(tool_summary(&outside, root, &[]).detail, "/etc/hosts");
    }

    #[test]
    fn durations_and_tokens_read_short() {
        assert_eq!(format_duration(Duration::from_millis(350)), "350 ms");
        assert_eq!(format_duration(Duration::from_secs(12)), "12 s");
        assert_eq!(format_duration(Duration::from_secs(65)), "1 min 5 s");
        assert_eq!(format_duration(Duration::from_secs(120)), "2 min");
        assert_eq!(format_duration(Duration::from_secs(3720)), "1 h 2 min");
        assert_eq!(format_tokens(950), "950");
        assert_eq!(format_tokens(1_234), "1.2k");
        assert_eq!(format_tokens(35_400), "35k");
        assert_eq!(format_tokens(1_250_000), "1.2M");
    }

    #[test]
    fn live_clocks_count_whole_seconds_from_zero() {
        assert_eq!(format_elapsed(Duration::ZERO), "0 s");
        assert_eq!(format_elapsed(Duration::from_millis(350)), "0 s");
        assert_eq!(format_elapsed(Duration::from_millis(1_999)), "1 s");
        assert_eq!(format_elapsed(Duration::from_secs(65)), "1 min 5 s");
    }

    #[test]
    fn a_thought_shows_its_time_only_when_it_is_known() {
        assert_eq!(thinking_label(true, 0, None), "Thinking…");
        assert_eq!(thinking_label(true, 1_234, None), "Thinking… 1.2k tokens");
        assert_eq!(
            thinking_label(false, 0, Some(Duration::from_millis(6_002))),
            "Thought for 6 s"
        );
        // Came whole (a saved conversation), or under a second: no "0 s", no milliseconds.
        assert_eq!(thinking_label(false, 0, None), "Thought");
        assert_eq!(
            thinking_label(false, 0, Some(Duration::from_millis(2))),
            "Thought"
        );
    }

    #[test]
    fn mentions_at_the_ends_become_chips() {
        let (chips, text) = edge_mentions("@src/a.rs#L3-9 why does @b.rs fail? @c.rs");
        let paths: Vec<&str> = chips.iter().map(|chip| chip.path).collect();
        assert_eq!(paths, ["src/a.rs", "c.rs"]);
        assert_eq!(chips[0].lines, Some((3, 9)));
        assert_eq!(text, "why does @b.rs fail?");
        let (chips, text) = edge_mentions("@only.rs");
        assert_eq!(chips.len(), 1);
        assert_eq!(text, "");
        let (chips, text) = edge_mentions("plain text");
        assert!(chips.is_empty());
        assert_eq!(text, "plain text");
    }

    #[test]
    fn a_plugins_attachments_become_chips() {
        // As `claude_session::with_attachments` sends them: blocks, a blank line, the message.
        let message = "<attachment name=\"TRACK-142\">\n# Fix the sign-in\n\nSteps…\n</attachment>\n\n\
                       <attachment name=\"TRACK-7\">\nParent\n</attachment>\n\n@src/a.rs what now?";
        let (blocks, rest) = leading_attachments(message);
        let labels: Vec<&str> = blocks.iter().map(|block| block.label).collect();
        assert_eq!(labels, ["TRACK-142", "TRACK-7"]);
        assert_eq!(blocks[0].text, "# Fix the sign-in\n\nSteps…");
        assert_eq!(rest, "@src/a.rs what now?");
        assert_eq!(bubble_text(message), "what now?");
        // Only attachments: no bubble.
        let (blocks, rest) = leading_attachments("<attachment name=\"A\">\nx\n</attachment>");
        assert_eq!(blocks.len(), 1);
        assert_eq!(rest, "");
        // Not an attachment block at the start, or an unclosed one: the text as it is.
        let text = "Look at <attachment name=\"A\">x</attachment>";
        assert_eq!(leading_attachments(text), (Vec::new(), text));
        let text = "<attachment name=\"A\">never closed";
        assert_eq!(leading_attachments(text), (Vec::new(), text));
    }

    #[test]
    fn an_attachments_tooltip_is_its_start() {
        assert_eq!(attachment_preview("a\nb"), "a\nb");
        let long: String = (0..20).map(|n| format!("line {n}\n")).collect();
        let preview = attachment_preview(&long);
        assert_eq!(preview.lines().count(), 12);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn minor_steps_fold_into_groups() {
        let read = |id: EntryId| Entry {
            id,
            kind: EntryKind::Tool(Box::new(tool(
                "Read",
                json!({ "file_path": "/p/a.rs" }),
                None,
            ))),
        };
        let text = |id: EntryId, text: &str| Entry {
            id,
            kind: EntryKind::Text {
                text: text.into(),
                streaming: false,
            },
        };
        let bash = Entry {
            id: 9,
            kind: EntryKind::Tool(Box::new(tool("Bash", json!({ "command": "ls" }), None))),
        };
        let entries = vec![
            read(1),
            text(2, ""),
            read(3),
            text(4, "Done."),
            bash,
            read(6),
        ];
        // A lone step (the last read) is its own item, not a group of one.
        assert_eq!(
            items(&entries),
            [
                Item::Group { start: 0, end: 3 },
                Item::Entry(3),
                Item::Entry(4),
                Item::Entry(5),
            ]
        );
        let shown: Vec<&Entry> = entries[..3].iter().collect();
        assert_eq!(group_summary(&shown), "Read 1 file");
    }

    #[test]
    fn long_output_is_clipped_by_lines() {
        let text = "1\n2\n3\n4\n5";
        assert_eq!(clip_lines(text, 3), ("1\n2\n3", 2));
        assert_eq!(clip_lines(text, 5), (text, 0));
        assert_eq!(clip_chars("abcdef", 3), "abc…");
        assert_eq!(host("https://docs.rs/gpui/latest"), "docs.rs");
    }

    #[test]
    fn a_long_answer_opens_in_the_details() {
        assert!(fits_in_row("Option B"));
        assert!(fits_in_row(&"a".repeat(ANSWER_IN_ROW)));
        assert!(!fits_in_row(&"a".repeat(ANSWER_IN_ROW + 1)));
        assert!(!fits_in_row("two\nlines"));
    }
}
