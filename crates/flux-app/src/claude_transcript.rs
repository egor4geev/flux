//! The conversation of a Claude chat: the user's messages, Claude's text (Markdown) and thinking,
//! the rows of Claude's actions with their results and subagents, notices, the ends of turns, and
//! the line of what Claude is doing now.
//!
//! The conversation is a gpui `list` anchored to the bottom, as a chat log: it follows new content
//! while the user is at the end and stays put when they scrolled up (a "↓" button brings them
//! back). Only the visible entries are drawn; an entry whose content changed is measured again
//! (`TranscriptState::sync` compares a fingerprint of each entry). Claude's Markdown is parsed
//! once per change of the text and highlighted off the UI thread when the text is complete.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt, AnyElement, App, ClipboardItem, Context, Entity, FontWeight, Hsla,
    Image, ImageFormat, InteractiveText, ListAlignment, ListOffset, ListState, SharedString,
    StyledImage, StyledText, TextRun, WeakEntity, Window, div, font, img, list, prelude::*, px,
};
use serde_json::Value;

use flux_claude::session::{TaskProgress, TurnSummary, UserEntry};
use flux_claude::{
    Activity, Entry, EntryId, EntryKind, Notice, Session, Status, ToolCall, ToolEntry, ToolState,
};

use crate::claude_chat::{ClaudeChat, ClaudeChatEvent};
use crate::claude_session::ClaudeSession;
use crate::i18n::{tr, trf, trn};
use crate::icons::{IconName, IconSource, file_icon, icon, source_icon};
use crate::markdown::{self, Block};
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_MD, RADIUS_SM};
use crate::workspace::Location;

/// The entries' inset from the island's edges.
const INSET: f32 = 12.;
/// A row of Claude's actions.
const ROW_HEIGHT: f32 = 24.;
/// Where a tool's details start: under the row's label, past the icon.
const DETAIL_INSET: f32 = 22.;
/// Output lines shown before "Show more".
const CLIP_LINES: usize = 12;
/// Diff lines shown before "Show more".
const DIFF_LINES: usize = 40;
/// How long a copy button shows its check mark.
const COPIED_FOR: Duration = Duration::from_millis(1500);

pub fn init(_cx: &mut App) {}

/// The transcript's state in its chat: the list, what the user expanded, parsed Markdown.
pub struct TranscriptState {
    list: ListState,
    /// What each top-level entry looked like when it was last measured.
    fingerprints: Vec<u64>,
    /// Entries whose expansion the user flipped (tool details, thinking, a subagent).
    pub(crate) toggled: HashSet<EntryId>,
    /// Long outputs and diffs the user asked to see whole.
    unclipped: HashSet<EntryId>,
    /// Claude's texts and thinking, parsed (and highlighted once complete), by entry.
    texts: HashMap<EntryId, Parsed>,
    /// Thumbnails of the user's pictures, by entry and index.
    images: HashMap<(EntryId, usize), Arc<Image>>,
    /// The copy button that shows a check mark, and since when.
    copied: Option<(EntryId, Instant)>,
    /// The session changed since the last sync.
    dirty: bool,
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

impl TranscriptState {
    pub fn new(chat: WeakEntity<ClaudeChat>) -> Self {
        let list = ListState::new(0, ListAlignment::Bottom, px(600.));
        // The "↓" button follows the scroll position.
        list.set_scroll_handler(move |_, _, cx| {
            chat.update(cx, |_, cx| cx.notify()).ok();
        });
        Self {
            list,
            fingerprints: Vec::new(),
            toggled: HashSet::new(),
            unclipped: HashSet::new(),
            texts: HashMap::new(),
            images: HashMap::new(),
            copied: None,
            dirty: true,
        }
    }

    /// The session changed: the next frame compares the entries again.
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// The end of the conversation is in view: new content scrolls in.
    pub fn at_bottom(&self) -> bool {
        self.list.logical_scroll_top().item_ix >= self.list.item_count()
    }

    pub fn scroll_to_bottom(&self) {
        self.list.scroll_to(ListOffset {
            item_ix: self.list.item_count(),
            offset_in_item: px(0.),
        });
    }

    fn expanded(&self, id: EntryId, default: bool) -> bool {
        (default || expand_all()) != self.toggled.contains(&id)
    }

    fn toggle(&mut self, id: EntryId) {
        if !self.toggled.remove(&id) {
            self.toggled.insert(id);
        }
        self.dirty = true;
    }

    fn just_copied(&self, id: EntryId) -> bool {
        self.copied
            .is_some_and(|(copied, at)| copied == id && at.elapsed() < COPIED_FOR)
    }
}

/// Brings the list up to date with the session: new and changed entries are measured again,
/// changed texts parsed, complete ones highlighted in the background, new pictures decoded.
pub fn sync(
    state: &mut TranscriptState,
    session: &Entity<ClaudeSession>,
    cx: &mut Context<ClaudeChat>,
) {
    if !state.dirty {
        return;
    }
    state.dirty = false;
    let mut texts = Vec::new();
    let mut pictures = Vec::new();
    let (fingerprints, user_sent) = {
        let model = session.read(cx).model();
        let fingerprints: Vec<u64> = model
            .entries
            .iter()
            .map(|entry| fingerprint(entry, state))
            .collect();
        collect_texts(&model.entries, &state.texts, &mut texts);
        for entry in &model.entries {
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
        // The user's own new message brings the view to the end.
        let user_sent = model.entries[state.fingerprints.len().min(model.entries.len())..]
            .iter()
            .any(|entry| matches!(entry.kind, EntryKind::User(_)));
        (fingerprints, user_sent)
    };

    // Measure changed entries again; new ones are added at the end.
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
                    if let Some(parsed) = chat.transcript.texts.get_mut(&id)
                        && parsed.len == len
                    {
                        parsed.blocks = Rc::new(highlighted);
                        parsed.highlighted = true;
                        parsed.highlighting = false;
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
    }
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

/// What an entry looks like, cheaply: when it changes, the list measures the entry again.
fn fingerprint(entry: &Entry, state: &TranscriptState) -> u64 {
    let mut hasher = DefaultHasher::new();
    hash_entry(entry, state, &mut hasher);
    hasher.finish()
}

fn hash_entry(entry: &Entry, state: &TranscriptState, hasher: &mut DefaultHasher) {
    entry.id.hash(hasher);
    state.toggled.contains(&entry.id).hash(hasher);
    state.unclipped.contains(&entry.id).hash(hasher);
    match &entry.kind {
        EntryKind::User(user) => (
            user.text.len(),
            user.images.len(),
            user.queued,
            user.cancelled,
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

fn image_format(media_type: &str) -> ImageFormat {
    match media_type {
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::Webp,
        _ => ImageFormat::Png,
    }
}

// --- The view ---

/// The conversation filling the chat (the "↓" button over it while scrolled up), and the line of
/// what Claude does now under it; tips in an empty session.
pub fn render(chat: &ClaudeChat, _window: &mut Window, cx: &mut Context<ClaudeChat>) -> AnyElement {
    let ui = Theme::ui(cx);
    let weak = cx.weak_entity();
    let state = &chat.transcript;
    let model = chat.session().read(cx).model();
    let activity = activity_line(model, ui);
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
    let jump = (!state.at_bottom()).then(|| {
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
                .tooltip(ui::tooltip(tr("Jump to Latest"), None))
                .child(icon(IconName::ArrowDown, ui.text_muted).size(px(14.)))
                .on_click(move |_, _, cx| {
                    weak.update(cx, |chat, cx| {
                        chat.transcript.scroll_to_bottom();
                        cx.notify();
                    })
                    .ok();
                }),
        )
    });
    let items = {
        let weak = weak.clone();
        list(state.list.clone(), move |index, _window, cx| {
            render_item(&weak, index, cx)
        })
        .size_full()
    };
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(items)
                .children(jump),
        )
        .children(activity)
        .into_any_element()
}

/// One entry of the main conversation, with the space above it.
fn render_item(chat: &WeakEntity<ClaudeChat>, index: usize, cx: &mut App) -> AnyElement {
    let Some(entity) = chat.upgrade() else {
        return div().into_any_element();
    };
    let chat_view = entity.read(cx);
    let session = chat_view.session().clone();
    let model = session.read(cx).model();
    let Some(entry) = model.entries.get(index) else {
        return div().into_any_element();
    };
    let theme = Theme::get(cx);
    let renderer = Renderer {
        ui: theme.ui,
        theme,
        state: &chat_view.transcript,
        chat: chat.clone(),
        session: &session,
        root: &model.info.cwd,
        tasks: &model.tasks,
    };
    if is_hidden(&entry.kind) {
        return div().into_any_element();
    }
    let previous = model.entries[..index]
        .iter()
        .rev()
        .find(|entry| !is_hidden(&entry.kind));
    let last = index + 1 == model.entries.len();
    div()
        .px(px(INSET))
        .pt(px(top_gap(previous.map(|entry| &entry.kind), &entry.kind)))
        .when(last, |item| item.pb(px(10.)))
        .child(renderer.entry(entry, 0))
        .into_any_element()
}

/// Thinking that is over and has no text to show (the CLI didn't summarize it): no row, the
/// activity line said it while it lasted.
fn is_hidden(kind: &EntryKind) -> bool {
    match kind {
        EntryKind::Thinking {
            text,
            streaming: false,
            ..
        }
        | EntryKind::Text {
            text,
            streaming: false,
        } => text.trim().is_empty(),
        _ => false,
    }
}

/// The space above an entry: tight between a turn's steps, wider before the user's messages.
fn top_gap(previous: Option<&EntryKind>, current: &EntryKind) -> f32 {
    match (previous, current) {
        (None, _) => 12.,
        (_, EntryKind::User(_)) => 14.,
        (Some(EntryKind::TurnEnd(_)), _) => 8.,
        (Some(EntryKind::Tool(_) | EntryKind::Thinking { .. }), EntryKind::Tool(_))
        | (Some(EntryKind::Tool(_)), EntryKind::Thinking { .. }) => 2.,
        _ => 8.,
    }
}

/// Draws entries; borrows what it needs for one frame.
struct Renderer<'a> {
    ui: UiColors,
    theme: &'a Theme,
    state: &'a TranscriptState,
    chat: WeakEntity<ClaudeChat>,
    session: &'a Entity<ClaudeSession>,
    root: &'a Path,
    /// Claude's task list: a task update names its task.
    tasks: &'a [flux_claude::TaskItem],
}

impl Renderer<'_> {
    fn entry(&self, entry: &Entry, depth: usize) -> AnyElement {
        match &entry.kind {
            EntryKind::User(user) => self.user(entry.id, user),
            EntryKind::Text { text, streaming } => self.text(entry.id, text, *streaming),
            EntryKind::Thinking {
                text,
                tokens,
                streaming,
                duration,
                started,
            } => self.thinking(entry.id, text, *tokens, *streaming, *duration, *started),
            EntryKind::Tool(tool) => self.tool(entry.id, tool, depth),
            EntryKind::Notice(notice) => self.notice(notice),
            EntryKind::TurnEnd(summary) => self.turn_end(entry.id, summary),
        }
    }

    // --- The user's messages ---

    fn user(&self, id: EntryId, user: &UserEntry) -> AnyElement {
        let ui = self.ui;
        let thumbnails = (0..user.images.len()).filter_map(|index| {
            self.state.images.get(&(id, index)).map(|image| {
                div()
                    .size(px(56.))
                    .rounded(px(RADIUS_SM))
                    .overflow_hidden()
                    .border_1()
                    .border_color(ui.divider)
                    .child(
                        img(image.clone())
                            .size_full()
                            .object_fit(gpui::ObjectFit::Cover),
                    )
            })
        });
        let status = if user.cancelled {
            Some(tr("Cancelled"))
        } else if user.queued {
            Some(tr("Queued"))
        } else {
            None
        };
        let cancel = user
            .uuid
            .clone()
            .filter(|_| user.queued)
            .map(|uuid: String| {
                let session = self.session.clone();
                ui::icon_button(("claude-cancel-queued", id), IconName::Close, ui)
                    .tooltip(ui::tooltip(tr("Don't Send"), None))
                    .on_click(move |_, _, cx| {
                        session.update(cx, |session, cx| session.cancel_queued(&uuid, cx))
                    })
            });
        div()
            .flex()
            .flex_col()
            .gap_1p5()
            .px(px(10.))
            .py(px(8.))
            .rounded(px(RADIUS_MD))
            .bg(UiColors::tint(ui.foreground, 0.05))
            .border_1()
            .border_color(UiColors::tint(ui.foreground, 0.07))
            .when(user.queued || user.cancelled, |bubble| bubble.opacity(0.6))
            .when(!user.text.is_empty(), |bubble| {
                bubble.child(self.user_text(id, &user.text, user.cancelled))
            })
            .when(!user.images.is_empty(), |bubble| {
                bubble.child(div().flex().flex_wrap().gap_1p5().children(thumbnails))
            })
            .children(status.map(|status| {
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(ui.dim)
                    .child(icon(IconName::Clock, ui.dim).size(px(12.)))
                    .child(status)
                    .child(div().flex_1())
                    .children(cancel)
            }))
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
        let styled = StyledText::new(display).with_runs(runs);
        if ranges.is_empty() {
            return div().child(styled).into_any_element();
        }
        let chat = self.chat.clone();
        InteractiveText::new(("claude-user-text", id), styled)
            .on_click(ranges, move |index, _, cx| {
                if let Some(location) = targets.get(index).cloned() {
                    chat.update(cx, |_, cx| cx.emit(ClaudeChatEvent::OpenLocation(location)))
                        .ok();
                }
            })
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
        div()
            .group(group.clone())
            .relative()
            .child(markdown::render_copyable(
                &parsed.blocks,
                ui.foreground,
                self.theme,
                format!("claude-code-{id}"),
            ))
            .children(copy.map(|copy| {
                div()
                    .absolute()
                    .right(px(-4.))
                    .bottom(px(-4.))
                    .rounded(px(ui::RADIUS_XS))
                    .bg(ui.island)
                    .when(!copied, |corner| {
                        corner
                            .invisible()
                            .group_hover(group.clone(), |style| style.visible())
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
            .child(
                icon(
                    if copied {
                        IconName::Check
                    } else {
                        IconName::Copy
                    },
                    if copied { ui.success } else { ui.dim },
                )
                .size(px(13.)),
            )
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                chat.update(cx, |chat, cx| {
                    chat.transcript.copied = Some((id, Instant::now()));
                    cx.notify();
                    cx.spawn(async move |chat, cx| {
                        cx.background_executor().timer(COPIED_FOR).await;
                        chat.update(cx, |_, cx| cx.notify()).ok();
                    })
                    .detach();
                })
                .ok();
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
        started: Instant,
    ) -> AnyElement {
        let ui = self.ui;
        let label = if streaming {
            if tokens > 0 {
                trf("Thinking… {0} tokens", &[&format_tokens(tokens)])
            } else {
                tr("Thinking…").to_string()
            }
        } else {
            let duration = duration.unwrap_or_else(|| started.elapsed());
            trf("Thought for {0}", &[&format_duration(duration)])
        };
        let expandable = !text.trim().is_empty();
        let expanded = expandable && self.state.expanded(id, false);
        let row = div()
            .id(("claude-thinking", id))
            .h(px(ROW_HEIGHT))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(theme::TEXT_SM))
            .text_color(ui.dim)
            .child(if streaming {
                pulse(("claude-thinking-pulse", id), ui).into_any_element()
            } else {
                icon(IconName::Claude, ui.dim)
                    .size(px(13.))
                    .into_any_element()
            })
            .child(label)
            .when(expandable, |row| {
                row.cursor_pointer()
                    .child(chevron(expanded, ui))
                    .on_click(self.toggle(id))
            });
        let body = expanded.then(|| {
            let blocks = self
                .state
                .texts
                .get(&id)
                .map(|parsed| parsed.blocks.clone());
            div()
                .ml(px(6.))
                .pl(px(DETAIL_INSET - 6.))
                .border_l_1()
                .border_color(ui.divider)
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.text_muted)
                .child(match blocks {
                    Some(blocks) => markdown::render(&blocks, ui.text_muted, self.theme),
                    None => div().child(text.to_string()).into_any_element(),
                })
        });
        div()
            .flex()
            .flex_col()
            .child(row)
            .children(body)
            .into_any_element()
    }

    // --- Claude's actions ---

    fn tool(&self, id: EntryId, tool: &ToolEntry, depth: usize) -> AnyElement {
        let ui = self.ui;
        let summary = tool_summary(tool, self.root, self.tasks);
        let running = matches!(tool.state, ToolState::Streaming | ToolState::Running);
        let is_agent = matches!(tool.call, ToolCall::Agent { .. });
        let details = self.details(id, tool, depth);
        // A running subagent shows its steps; a finished one folds them away.
        let default_open = is_agent && running;
        let expanded = details.is_some() && self.state.expanded(id, default_open);
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
                .child(summary.detail.clone());
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
            .children(summary.meta.clone().map(|meta| {
                div()
                    .flex_none()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(ui.dim)
                    .child(meta)
            }))
            .children(mark)
            .when(details.is_some(), |row| {
                row.cursor_pointer()
                    .hover(move |style| style.bg(ui.hover))
                    .child(chevron(expanded, ui))
                    .on_click(self.toggle(id))
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
            .when(expanded, |column| column.children(details))
            .into_any_element()
    }

    /// The flip of an entry's expansion, for a click handler.
    fn toggle(&self, id: EntryId) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
        let chat = self.chat.clone();
        move |_, _, cx| {
            chat.update(cx, |chat, cx| {
                chat.transcript.toggle(id);
                cx.notify();
            })
            .ok();
        }
    }

    /// What a row shows when expanded; `None` — nothing to show.
    fn details(&self, id: EntryId, tool: &ToolEntry, depth: usize) -> Option<AnyElement> {
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
                let progress = self.agent_progress(tool);
                // The subagent's own answer, when its messages didn't come (the result's text is
                // framed for the main model, not for the user).
                let summary = structured
                    .filter(|_| tool.children.is_empty())
                    .and_then(|result| result["content"].as_array())
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|part| part["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .filter(|text| !text.trim().is_empty());
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
                    .children(summary.map(|summary| {
                        div()
                            .mt_1()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.text_muted)
                            .child(clip_chars(&summary, 600))
                    }))
                    .into_any_element()
            }
            ToolCall::Edit { .. } | ToolCall::Write { .. } | ToolCall::NotebookEdit { .. } => {
                // A refused or failed edit changed nothing: its reason, not a diff.
                if matches!(tool.state, ToolState::Denied | ToolState::Failed) {
                    return self.result_text(id, tool);
                }
                let lines = structured.map(patch_lines).unwrap_or_default();
                let lines = if lines.is_empty() {
                    // A new file: its text as added lines.
                    match (&tool.call, structured) {
                        (ToolCall::Write { content, .. }, _) if tool.state == ToolState::Done => {
                            content
                                .lines()
                                .enumerate()
                                .map(|(index, text)| PatchLine {
                                    kind: LineKind::Added,
                                    old: None,
                                    new: Some(index + 1),
                                    text: text.to_string(),
                                })
                                .collect()
                        }
                        _ => Vec::new(),
                    }
                } else {
                    lines
                };
                if lines.is_empty() {
                    return self.result_text(id, tool);
                }
                return self.diff(id, &lines);
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
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when(command.contains('\n') || command.len() > 60, |column| {
                        column.child(code_box(command, ui.text_muted, ui))
                    })
                    .when(!stdout.trim().is_empty(), |column| {
                        column.child(self.output(id, stdout.trim_end(), ui.foreground))
                    })
                    .when(!stderr.trim().is_empty(), |column| {
                        column.child(self.output(id, stderr.trim_end(), ui.warning))
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
                    return self.result_text(id, tool);
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
                    _ => return self.result_text(id, tool),
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
                // One question: the row already says it and the answer.
                if answers.len() <= 1 {
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
                return self.result_text(id, tool);
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
            | ToolCall::Other => return self.result_text(id, tool),
        };
        Some(self.detail_box(body))
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
    fn result_text(&self, id: EntryId, tool: &ToolEntry) -> Option<AnyElement> {
        let result = tool.result.as_ref()?;
        let text = result.text.trim_end();
        if text.trim().is_empty() {
            return None;
        }
        // A refusal is the user's (or a rule's) words: prose, not output.
        if tool.state == ToolState::Denied {
            return Some(
                self.detail_box(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(self.ui.text_muted)
                        .child(clip_chars(text.trim_start_matches("Error: "), 600))
                        .into_any_element(),
                ),
            );
        }
        let color = if result.is_error {
            self.ui.error
        } else {
            self.ui.text_muted
        };
        Some(self.detail_box(self.output(id, text, color)))
    }

    /// Output in the code font, clipped to a few lines with "Show more".
    fn output(&self, id: EntryId, text: &str, color: Hsla) -> AnyElement {
        let ui = self.ui;
        let unclipped = self.state.unclipped.contains(&id);
        let (shown, hidden) = if unclipped {
            (text, 0)
        } else {
            clip_lines(text, CLIP_LINES)
        };
        div()
            .flex()
            .flex_col()
            .child(code_box(shown, color, ui))
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
                chat.update(cx, |chat, cx| {
                    chat.transcript.unclipped.insert(id);
                    chat.transcript.invalidate();
                    cx.notify();
                })
                .ok();
            })
            .into_any_element()
    }

    /// An edit's change: removed and added lines with their numbers, hunks apart.
    fn diff(&self, id: EntryId, lines: &[PatchLine]) -> Option<AnyElement> {
        let ui = self.ui;
        let unclipped = self.state.unclipped.contains(&id);
        let shown = if unclipped {
            lines.len()
        } else {
            lines.len().min(DIFF_LINES)
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
        let body = div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .py_1()
                    .rounded(px(RADIUS_SM))
                    .overflow_hidden()
                    .bg(UiColors::tint(ui.foreground, 0.03))
                    .border_1()
                    .border_color(ui.divider)
                    .font_family(theme::code_font())
                    .text_size(px(12.))
                    .line_height(px(18.))
                    .children(rows),
            )
            .children((shown < lines.len()).then(|| self.show_more(id, lines.len() - shown)))
            .into_any_element();
        Some(self.detail_box(body))
    }

    /// A subagent's progress while it runs (its last tool, tool calls, tokens), its totals when it
    /// is done.
    fn agent_progress(&self, tool: &ToolEntry) -> Option<AnyElement> {
        let ui = self.ui;
        let structured = tool
            .result
            .as_ref()
            .and_then(|result| result.structured.as_ref());
        let mut parts = Vec::new();
        match structured.filter(|result| result["status"] == "completed") {
            Some(result) => {
                if let Some(uses) = result["totalToolUseCount"].as_u64() {
                    parts.push(trn(uses as usize, "{n} tool use", "{n} tool uses"));
                }
                if let Some(tokens) = result["totalTokens"].as_u64() {
                    parts.push(trf("{0} tokens", &[&format_tokens(tokens)]));
                }
                if let Some(ms) = result["totalDurationMs"].as_u64() {
                    parts.push(format_duration(Duration::from_millis(ms)));
                }
            }
            None => {
                let task: &TaskProgress = tool.task.as_ref()?;
                if let Some(last) = &task.last_tool {
                    parts.push(last.clone());
                }
                if let Some(uses) = task.tool_uses {
                    parts.push(trn(uses as usize, "{n} tool use", "{n} tool uses"));
                }
                if let Some(tokens) = task.tokens {
                    parts.push(trf("{0} tokens", &[&format_tokens(tokens)]));
                }
            }
        }
        (!parts.is_empty()).then(|| {
            div()
                .mt_1()
                .text_size(px(theme::TEXT_XS))
                .text_color(ui.dim)
                .child(parts.join(" · "))
                .into_any_element()
        })
    }

    // --- Notices and the ends of turns ---

    fn notice(&self, notice: &Notice) -> AnyElement {
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
        }
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
                .rounded(px(RADIUS_MD))
                .border_1()
                .border_color(ui.divider)
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

    fn turn_end(&self, id: EntryId, summary: &TurnSummary) -> AnyElement {
        let ui = self.ui;
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
        let cost = (summary.cost_usd > 0.).then(|| {
            trf(
                "≈ ${0} at API prices (with a subscription, it counts towards its limits)",
                &[&format!("{:.2}", summary.cost_usd)],
            )
        });
        div()
            .id(("claude-turn-end", id))
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(theme::TEXT_XS))
            .text_color(if summary.is_error && !stopped(summary) {
                ui.warning
            } else {
                ui.dim
            })
            .child(div().flex_1().h(px(1.)).bg(ui.divider))
            .child(label)
            .child(div().flex_1().h(px(1.)).bg(ui.divider))
            .when_some(cost, |line, cost| line.tooltip(ui::tooltip(cost, None)))
            .into_any_element()
    }
}

/// The user stopped the turn (Esc): not an error to point at.
fn stopped(summary: &TurnSummary) -> bool {
    matches!(
        summary.reason.as_deref(),
        Some("aborted_streaming" | "aborted_tools")
    )
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

/// A pulsing dot: something runs.
fn pulse(id: impl Into<gpui::ElementId>, ui: UiColors) -> impl IntoElement {
    div()
        .flex_none()
        .size(px(7.))
        .rounded(px(4.))
        .bg(ui.accent)
        .with_animation(
            id,
            Animation::new(Duration::from_millis(900)).repeat(),
            |dot, delta| dot.opacity(0.3 + 0.7 * (1. - (2. * delta - 1.).abs())),
        )
}

fn chevron(expanded: bool, ui: UiColors) -> impl IntoElement {
    icon(
        if expanded {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        },
        ui.dim,
    )
    .size(px(12.))
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
                .child(icon(name, color).size(px(14.)))
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
        .children(action.map(|(label, action)| {
            div().flex().child(
                ui::primary_button("claude-notice-action", label, true, ui).on_click(
                    move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx),
                ),
            )
        }))
        .into_any_element()
}

/// The line under the conversation while Claude works: what it does, for how long, Esc to stop;
/// while a question waits — that it waits for the user.
fn activity_line(model: &Session, ui: UiColors) -> Option<AnyElement> {
    let row = || {
        div()
            .flex_none()
            .h(px(28.))
            .px(px(INSET))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(theme::TEXT_SM))
            .text_color(ui.text_muted)
    };
    match &model.status {
        Status::Working { since, activity } => {
            let (label, tokens) = activity_label(activity, model);
            let mut details = vec![format_duration(since.elapsed())];
            if let Some(tokens) = tokens.filter(|tokens| *tokens > 0) {
                details.push(trf("{0} tokens", &[&format_tokens(tokens)]));
            }
            Some(
                row()
                    .child(pulse("claude-activity", ui))
                    .child(div().text_color(ui.foreground).child(label))
                    .child(div().text_color(ui.dim).child(details.join(" · ")))
                    .child(div().flex_1())
                    .child(ui::keycap("Esc", ui))
                    .child(
                        div()
                            .text_size(px(theme::TEXT_XS))
                            .text_color(ui.dim)
                            .child(tr("to stop")),
                    )
                    .into_any_element(),
            )
        }
        Status::WaitingForUser => Some(
            row()
                .child(icon(IconName::Question, ui.warning).size(px(14.)))
                .child(
                    div()
                        .text_color(ui.warning)
                        .child(tr("Waiting for your answer")),
                )
                .into_any_element(),
        ),
        Status::Starting if !model.entries.is_empty() => Some(
            row()
                .child(pulse("claude-starting", ui))
                .child(tr("Starting Claude…"))
                .into_any_element(),
        ),
        _ => None,
    }
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

/// "350 ms", "12 s", "1 min 5 s", "1 h 2 min".
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

fn first_line(text: &str) -> String {
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
    fn long_output_is_clipped_by_lines() {
        let text = "1\n2\n3\n4\n5";
        assert_eq!(clip_lines(text, 3), ("1\n2\n3", 2));
        assert_eq!(clip_lines(text, 5), (text, 0));
        assert_eq!(clip_chars("abcdef", 3), "abc…");
        assert_eq!(host("https://docs.rs/gpui/latest"), "docs.rs");
    }
}
