//! The message field of a Claude chat: what the user writes and sends (↵), and how the session
//! runs — the model, effort, the permission mode, how full the context is.
//!
//! - The field ([`PromptInput`]) wraps and grows; ⇧↵ starts a new line. While Claude works a
//!   message waits for the turn to end (the CLI queues it); ⌘↵ sends it at once, stopping the
//!   turn. Esc stops a running turn.
//! - Chips above the text: the editor's file and selected lines (sent as an `@path#L12-20`
//!   mention unless removed), pictures pasted (⌘V) or dropped on the field.
//! - `@` lists the project's files (fuzzy, as ⌘P), `/` at the start the session's slash commands;
//!   the list opens above the field, ↑ ↓ ↵ ⇥ Esc drive it.
//! - The card's bottom row: the model, effort and permission mode pickers (⇧⇥ cycles Ask before
//!   edits → Accept edits → Plan mode, as in the CLI), the context ring, attach, Send / Stop
//!   (part 10.2, the chat's new look).
//! - `!` typed first in the empty field: shell mode (part 10.2) — a green `!` badge, ↵ runs the
//!   command in the project's root ([`ClaudeSession::run_shell`]), its output goes to Claude; ⌫ in
//!   the empty field or Esc leaves the mode.
//! - A subagent's view has a field of its own ([`AgentTarget`]): the CLI can't take a message for a
//!   subagent (a `parent_tool_use_id` on the host's message is ignored — `PROTOCOL.md` §10.1), so
//!   the message goes to Claude, asking it to pass it on with SendMessage — that reaches a running
//!   background agent and resumes a finished one. While a foreground agent runs, Claude waits for
//!   it: the message waits too. No `!`, no ⌘↵ and no Stop there — they would stop Claude's turn and
//!   the agent with it; Esc goes back to the session.
//! - Part «Флаг страны»: while the country check blocks Claude ([`crate::region_guard`]) a banner on
//!   top of the card says why, with "Check Again"; Send is off and ↵ keeps the text in the field.
//!   A message that waits for the check's answer shows "Checking the country…" by Send; one the
//!   check sends back comes back into the field as it was written ([`Composer::put_back`]).

use std::cell::Cell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use flux_claude::{
    Effort, ImageAttachment, ModelInfo, PermissionMode, Priority, SlashCommand, Status, UserInput,
};
use flux_search::{PathMatcher, match_list, walk_files};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    Action, Animation, AnimationExt, AnyElement, App, Bounds, ClickEvent, Context, Corner,
    DismissEvent, Entity, ExternalPaths, FocusHandle, Focusable, Hsla, Image, ImageFormat,
    KeyBinding, ObjectFit, Pixels, Point, Render, SharedString, Subscription, Task, Window,
    actions, anchored, canvas, deferred, div, img, point, prelude::*, px,
};

use crate::claude_session::{Attachment, ClaudeSession, EditPoint, SessionEvent, with_attachments};
use crate::context_menu::ContextMenu;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, file_icon, icon};
use crate::motion::{self, Appear, TransitionExt};
use crate::picker::highlighted_text;
use crate::popup;
use crate::prompt_input::{self, PromptInput, PromptInputEvent};
use crate::region_guard::{self, Refusal, Verdict};
use crate::settings;
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_LG, RADIUS_SM};
use gpui::PathBuilder;

actions!(
    claude_composer,
    [
        /// ↵: sends the message (while Claude works it waits for the turn to end).
        Send,
        /// ⌘↵: sends the message now, stopping a running turn.
        SendNow,
        /// Esc: stops the running turn.
        Stop,
        /// ⇧⇥: Ask before edits → Accept edits → Plan mode, as in the CLI.
        CycleMode,
    ]
);

/// The model picker's choice; `None` — the account's default.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = claude_composer, no_json)]
pub struct ChooseModel {
    pub value: Option<SharedString>,
}

#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = claude_composer, no_json)]
pub struct ChooseEffort {
    pub effort: Effort,
}

#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = claude_composer, no_json)]
pub struct ChooseMode {
    pub mode: PermissionMode,
}

const CONTEXT: &str = "ClaudeComposer";
/// The field grows up to this many rows, then scrolls.
const MESSAGE_ROWS: usize = 10;
/// The rows of the list for `@` and `/`.
const LIST_ROWS: usize = 8;
const LIST_ROW_HEIGHT: f32 = 28.;
/// A picture larger than this isn't attached (the API's limit is lower; the CLI scales).
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const THUMBNAIL_SIZE: f32 = 44.;
const FOOTER_HEIGHT: f32 = 28.;
/// The gap between the rows of the card (banners, chips, the field, the bottom row).
const CARD_GAP: f32 = 4.;
/// A card narrower than this (the island at its default width) shortens the bottom row: the
/// model without its version, the effort as the CLI writes it, the context without its percent.
const NARROW_CARD: f32 = 380.;
/// Send / Stop: a rounded square in the card's corner.
const SEND_SIZE: f32 = 28.;
/// The context ring: diameter and line.
const RING_SIZE: f32 = 14.;
const RING_LINE: f32 = 2.;
/// Redraws on nucleo's signals happen at most once per frame (as in ⌘P).
const REFRESH_INTERVAL: Duration = Duration::from_millis(16);
/// How many sent messages the field remembers as they were written: one the country check sends
/// back goes into the field as the user wrote it.
const SENT_KEPT: usize = 16;

pub fn init(cx: &mut App) {
    let context = Some(CONTEXT);
    crate::keymap::bind_keys(
        cx,
        [
            KeyBinding::new("enter", Send, context),
            KeyBinding::new("cmd-enter", SendNow, context),
            KeyBinding::new("escape", Stop, context),
            KeyBinding::new("shift-tab", CycleMode, context),
        ],
    );
}

/// The editor the user works in, as the message mentions it (`@src/main.rs#L12-20`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorContext {
    /// Relative to the project root when inside it.
    pub path: String,
    /// The selected lines, 1-based, inclusive; `None` — no selection (the file itself).
    pub lines: Option<(usize, usize)>,
}

impl EditorContext {
    /// The mention the CLI expands: `@path`, `@path#L12`, `@path#L12-20`.
    pub fn mention(&self) -> String {
        let path = mention_path(&self.path);
        match self.lines {
            None => format!("@{path}"),
            Some((first, last)) if first == last => format!("@{path}#L{first}"),
            Some((first, last)) => format!("@{path}#L{first}-{last}"),
        }
    }

    /// The chip's label: "main.rs:12–20".
    fn label(&self) -> String {
        let name = Path::new(&self.path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.clone());
        match self.lines {
            None => name,
            Some((first, last)) if first == last => format!("{name}:{first}"),
            Some((first, last)) => format!("{name}:{first}–{last}"),
        }
    }
}

/// A path as a mention writes it: quoted when it has spaces.
fn mention_path(path: &str) -> String {
    if path.chars().any(char::is_whitespace) {
        format!("\"{path}\"")
    } else {
        path.to_string()
    }
}

/// What the text at the caret asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// `@query`: a project file.
    Mention {
        range: std::ops::Range<usize>,
        query: String,
    },
    /// `/query` at the start of the message: a slash command.
    Command {
        range: std::ops::Range<usize>,
        query: String,
    },
}

impl Token {
    fn range(&self) -> &std::ops::Range<usize> {
        match self {
            Token::Mention { range, .. } | Token::Command { range, .. } => range,
        }
    }

    fn query(&self) -> &str {
        match self {
            Token::Mention { query, .. } | Token::Command { query, .. } => query,
        }
    }
}

/// The word at `caret` (a character index), if it is a mention or a command being typed: from its
/// `@` (at a word's start) or its `/` (at the message's start) to the end of the word; the query
/// is what is typed before the caret.
fn token_at(text: &str, caret: usize) -> Option<Token> {
    let chars: Vec<char> = text.chars().collect();
    let caret = caret.min(chars.len());
    let mut start = caret;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    let mut end = caret;
    while end < chars.len() && !chars[end].is_whitespace() {
        end += 1;
    }
    let first = *chars.get(start)?;
    if start == caret {
        return None;
    }
    let query: String = chars[start + 1..caret].iter().collect();
    match first {
        '@' => Some(Token::Mention {
            range: start..end,
            query,
        }),
        '/' if start == 0 => Some(Token::Command {
            range: start..end,
            query,
        }),
        _ => None,
    }
}

/// A model id as people say it: "claude-opus-5-5" → "Opus 5.5", "claude-haiku-4-5-20251001" →
/// "Haiku 4.5".
fn model_label(id: &str) -> String {
    let id = id.strip_prefix("claude-").unwrap_or(id);
    let mut parts = id
        .split('-')
        .filter(|part| !(part.len() == 8 && part.chars().all(|c| c.is_ascii_digit())));
    let Some(family) = parts.next() else {
        return id.to_string();
    };
    let mut chars = family.chars();
    let family: String = chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    let version: Vec<&str> = parts.collect();
    if version.is_empty() {
        family
    } else {
        format!("{family} {}", version.join("."))
    }
}

fn mode_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Default => tr("Ask before edits"),
        PermissionMode::AcceptEdits => tr("Accept edits"),
        PermissionMode::Plan => tr("Plan mode"),
        PermissionMode::DontAsk => tr("Don't ask"),
        PermissionMode::Auto => tr("Auto mode"),
        PermissionMode::BypassPermissions => tr("Bypass permissions"),
    }
}

fn mode_color(mode: PermissionMode, ui: &UiColors) -> Hsla {
    match mode {
        PermissionMode::Default => ui.text_muted,
        PermissionMode::AcceptEdits => ui.success,
        PermissionMode::Plan => ui.info,
        PermissionMode::DontAsk | PermissionMode::Auto => ui.warning,
        PermissionMode::BypassPermissions => ui.error,
    }
}

fn effort_label(effort: Effort) -> &'static str {
    match effort {
        Effort::Low => tr("Low effort"),
        Effort::Medium => tr("Medium effort"),
        Effort::High => tr("High effort"),
        Effort::XHigh => tr("Extra high effort"),
        Effort::Max => tr("Max effort"),
    }
}

/// Effort as the footer shows it.
fn effort_short(effort: Effort) -> &'static str {
    match effort {
        Effort::Low => tr("Low"),
        Effort::Medium => tr("Medium"),
        Effort::High => tr("High"),
        Effort::XHigh => tr("Extra high"),
        Effort::Max => tr("Max"),
    }
}

/// ⇧⇥: the CLI's cycle of the three everyday modes.
fn next_mode(mode: PermissionMode) -> PermissionMode {
    match mode {
        PermissionMode::Default => PermissionMode::AcceptEdits,
        PermissionMode::AcceptEdits => PermissionMode::Plan,
        _ => PermissionMode::Default,
    }
}

/// "28.9k".
fn short_tokens(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => {
            let thousands = tokens as f64 / 1000.;
            if thousands < 100. {
                format!("{thousands:.1}k").replace(".0k", "k")
            } else {
                format!("{thousands:.0}k")
            }
        }
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.).replace(".0M", "M"),
    }
}

/// The pictures the API takes.
fn image_format(path: &Path) -> Option<ImageFormat> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::Webp,
        _ => return None,
    })
}

fn is_sendable(format: ImageFormat) -> bool {
    matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::Webp
    )
}

/// The project's files for `@`: walked in the background when the list opens, matched by nucleo.
struct FileSearch {
    matcher: PathMatcher,
    cancel: Arc<AtomicBool>,
    _tasks: [Task<()>; 2],
}

impl Drop for FileSearch {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The list for the token at the caret.
struct Suggestions {
    token: Token,
    files: Option<FileSearch>,
    selected: usize,
}

/// A row of the list.
#[derive(Debug, Clone, PartialEq)]
enum Item {
    File {
        path: String,
        positions: Vec<usize>,
    },
    Command {
        name: String,
        hint: Option<String>,
        description: String,
        positions: Vec<usize>,
    },
}

/// Flux's own slash command: the history popup instead of the CLI's terminal-only `/resume`.
const RESUME: &str = "resume";

/// The message is `/resume` (with or without words after it).
fn is_resume(body: &str) -> bool {
    body.strip_prefix('/')
        .and_then(|rest| rest.strip_prefix(RESUME))
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

impl Item {
    /// What the token becomes.
    fn replacement(&self) -> String {
        match self {
            Item::File { path, .. } => format!("@{} ", mention_path(path)),
            Item::Command { name, .. } => format!("/{name} "),
        }
    }
}

/// A choice made here (a model, effort, a mode), shown until the CLI reports the session's value
/// anew: the confirmation comes with the next status or turn, an unsupported choice is never
/// confirmed and the CLI's value wins as soon as it changes.
#[derive(Debug, Clone, PartialEq)]
struct Choice<T> {
    value: T,
    /// What the session reported when the choice was made.
    reported: T,
}

impl<T: Clone + PartialEq> Choice<T> {
    fn shown(choice: &Option<Self>, reported: &T) -> T {
        match choice {
            Some(choice) if choice.reported == *reported => choice.value.clone(),
            _ => reported.clone(),
        }
    }
}

/// The pickers under the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Picker {
    Model,
    Effort,
    Mode,
}

/// A picker's open menu.
struct OpenMenu {
    menu: Entity<ContextMenu>,
    anchor: Bounds<Pixels>,
    _subscriptions: [Subscription; 2],
}

impl OpenMenu {
    /// The menu's bottom-left corner: above the chip it opened from.
    fn corner(&self) -> Point<Pixels> {
        point(self.anchor.left(), self.anchor.top() - px(popup::POPUP_GAP))
    }
}

/// What the chips above the field show: the editor's file, a plugin's attachments, the pictures,
/// a note — the field's own, or those that went with a sent message as they leave.
#[derive(Default)]
struct Chips {
    context: Option<EditorContext>,
    attachments: Vec<Attachment>,
    images: Vec<Arc<Image>>,
    notice: Option<SharedString>,
}

impl Chips {
    fn is_empty(&self) -> bool {
        self.context.is_none()
            && self.attachments.is_empty()
            && self.images.is_empty()
            && self.notice.is_none()
    }
}

pub struct Composer {
    session: Entity<ClaudeSession>,
    input: Entity<PromptInput>,
    /// The active editor's file and selection: a chip above the field, sent along as a mention.
    editor_context: Option<EditorContext>,
    /// The chip was removed (or sent) for this context; it comes back when the context changes.
    context_hidden: bool,
    /// What a plugin gave the session (part 10.3): chips, their text before the next message.
    attachments: Vec<Attachment>,
    images: Vec<Arc<Image>>,
    /// Why a picture wasn't attached; goes with the next change.
    notice: Option<SharedString>,
    suggestions: Option<Suggestions>,
    /// Where the token starts that the user closed the list for (Esc): it stays closed while the
    /// word is typed on.
    dismissed: Option<usize>,
    /// The model picked in this session (its `value`), until the CLI reports the model anew.
    model_choice: Option<Choice<Option<String>>>,
    effort_choice: Option<Choice<Option<Effort>>>,
    mode_choice: Option<Choice<PermissionMode>>,
    menu: Option<OpenMenu>,
    /// The menu that closed, fading out toward its chip without input; its bottom-left corner.
    menu_leaving: Option<motion::Leaving<(Entity<ContextMenu>, Point<Pixels>)>>,
    /// The chips that went with a sent message, fading out as their row folds away; the row's
    /// height as last drawn.
    chips_leaving: Option<motion::Leaving<Chips>>,
    chips_height: Rc<Cell<f32>>,
    /// Where the card and the pickers were drawn: the list and the menus open above them.
    card_bounds: Option<Bounds<Pixels>>,
    picker_bounds: HashMap<Picker, Bounds<Pixels>>,
    /// `!` typed first in the empty field: what's typed is a shell command (part 10.2).
    shell_mode: bool,
    /// Which of Stop and Send the corner showed when last drawn; when that changed, and what it
    /// showed before: the button that comes pops in.
    buttons: Option<(bool, bool)>,
    swapped: Option<(Instant, (bool, bool))>,
    /// The field was empty before the last change: a `!` typed into it starts shell mode.
    was_empty: bool,
    /// A sent message being edited (part 10.2): sending continues the conversation from just
    /// before it.
    editing: Option<Editing>,
    /// The field of a subagent's view: messages go to Claude for the agent.
    agent: Option<AgentTarget>,
    /// Why the country check blocks Claude, as the banner on top of the card says it; kept while
    /// the country is checked again after a block (part «Флаг страны»).
    region_refusal: Option<Refusal>,
    /// The last messages sent from here, as they were written (part «Флаг страны»).
    sent: Vec<Sent>,
    _subscriptions: Vec<Subscription>,
}

/// A message as the field held it when it went to the session.
struct Sent {
    /// What the session got: with the subagent's request and the plugin's attachments.
    message: String,
    returned: Returned,
}

/// A message the country check sent back, as the field shows it again: the text as written (with
/// the mention of the editor's file), the pictures and the plugin's attachments as chips.
pub struct Returned {
    text: String,
    images: Vec<Arc<Image>>,
    attachments: Vec<Attachment>,
}

impl Returned {
    /// A message no field remembers (an edited one): its text and pictures as they went.
    pub fn of(input: &UserInput) -> Self {
        let images = input
            .images
            .iter()
            .filter_map(|image| {
                let format = ImageFormat::from_mime_type(&image.media_type)?;
                Some(Arc::new(Image::from_bytes(format, image.data.clone())))
            })
            .collect();
        Self {
            text: input.text.clone(),
            images,
            attachments: Vec::new(),
        }
    }
}

/// The subagent a field in its view writes to (through Claude).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTarget {
    /// The Agent call's description: «Agent · its task».
    pub description: String,
    /// The CLI's agent id (`task_started.task_id`, the result's `agentId`): what SendMessage
    /// takes; `None` before the CLI reports it.
    pub agent_id: Option<String>,
    /// A foreground agent at work: Claude waits for it, and so does the message.
    pub waits: bool,
}

impl AgentTarget {
    /// What Claude gets: the message and whom to pass it on to.
    pub fn message(&self, body: &str) -> String {
        let head = match &self.agent_id {
            Some(id) => trf(
                "A message for the subagent «{0}» (agent id {1}), written in its view — pass it on to the agent with SendMessage:",
                &[&self.description, id],
            ),
            None => trf(
                "A message for the subagent «{0}», written in its view — pass it on to the agent with SendMessage:",
                &[&self.description],
            ),
        };
        format!("{head}\n\n{body}")
    }
}

/// A sent message back in the field (part 10.2), and what the field held before it — back on Esc.
struct Editing {
    point: EditPoint,
    draft: String,
    draft_images: Vec<Arc<Image>>,
}

impl Composer {
    pub fn new(
        session: Entity<ClaudeSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            PromptInput::new(tr("Ask Claude… @ for files, / for commands"), cx)
                .max_rows(MESSAGE_ROWS)
        });
        let subscriptions = vec![
            cx.subscribe(
                &input,
                |this, _, event: &PromptInputEvent, cx| match event {
                    PromptInputEvent::Changed => {
                        this.notice = None;
                        this.enter_shell_mode(cx);
                        this.update_suggestions(cx);
                        cx.notify();
                    }
                    PromptInputEvent::SelectionChanged => this.update_suggestions(cx),
                    PromptInputEvent::ImagePasted(image) if !this.shell_mode => {
                        this.attach(image.clone(), cx)
                    }
                    PromptInputEvent::ImagePasted(_) => {}
                    PromptInputEvent::BackspaceAtStart if this.shell_mode => {
                        this.leave_shell_mode(cx)
                    }
                    PromptInputEvent::BackspaceAtStart => {}
                },
            ),
            // The session refreshes the context meter after each turn itself.
            cx.subscribe(&session, |_, _, _: &SessionEvent, cx| cx.notify()),
        ];
        // The banner and Send follow the country check: the chat's tab is a cached view, it isn't
        // drawn again with the window.
        let mut subscriptions = subscriptions;
        let mut region_refusal = None;
        follow_block(&mut region_refusal, &region_guard::verdict(cx));
        if let Some(guard) = region_guard::global(cx) {
            subscriptions.push(cx.observe(&guard, |this: &mut Self, _, cx| {
                follow_block(&mut this.region_refusal, &region_guard::verdict(cx));
                cx.notify();
            }));
        }
        // The meter shows what the conversation starts with (the system prompt, the tools).
        session.update(cx, |session, cx| session.refresh_context(cx));
        // A plugin's session starts with its context and text, the field taking the keyboard
        // (part 10.3).
        let draft = session.update(cx, |session, _| session.take_draft());
        let mut attachments = Vec::new();
        if let Some(draft) = draft {
            attachments = draft.attachments;
            if let Some(prompt) = draft.prompt {
                input.update(cx, |input, cx| input.set_text(&prompt, cx));
            }
            window.focus(&input.focus_handle(cx));
        }
        #[allow(unused_mut)]
        let mut composer = Self {
            session,
            input,
            editor_context: None,
            context_hidden: false,
            attachments,
            images: Vec::new(),
            notice: None,
            suggestions: None,
            dismissed: None,
            model_choice: None,
            effort_choice: None,
            mode_choice: None,
            menu: None,
            menu_leaving: None,
            chips_leaving: None,
            chips_height: Rc::default(),
            card_bounds: None,
            picker_bounds: HashMap::new(),
            shell_mode: false,
            buttons: None,
            swapped: None,
            was_empty: true,
            editing: None,
            agent: None,
            region_refusal,
            sent: Vec::new(),
            _subscriptions: subscriptions,
        };
        #[cfg(feature = "scenario")]
        composer.scenario_chips();
        composer
    }

    /// UI scenarios: chips without the workspace and the clipboard —
    /// `FLUX_SCENARIO_CLAUDE_CONTEXT=src/main.rs:12:20`, `FLUX_SCENARIO_CLAUDE_IMAGES=a.png:b.jpg`.
    #[cfg(feature = "scenario")]
    fn scenario_chips(&mut self) {
        if let Ok(spec) = std::env::var("FLUX_SCENARIO_CLAUDE_CONTEXT") {
            let mut parts = spec.split(':');
            let path = parts.next().unwrap_or_default().to_string();
            let first = parts.next().and_then(|line| line.parse().ok());
            let last = parts.next().and_then(|line| line.parse().ok());
            self.editor_context = Some(EditorContext {
                path,
                lines: first.map(|first| (first, last.unwrap_or(first))),
            });
        }
        if let Ok(paths) = std::env::var("FLUX_SCENARIO_CLAUDE_IMAGES") {
            for path in std::env::split_paths(&paths) {
                if let (Some(format), Ok(bytes)) = (image_format(&path), std::fs::read(&path)) {
                    self.images.push(Arc::new(Image::from_bytes(format, bytes)));
                }
            }
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &App) {
        window.focus(&self.input.focus_handle(cx));
    }

    /// Makes this the field of a subagent's view (the chat sets it as the agent in sight changes).
    pub fn set_agent(&mut self, agent: Option<AgentTarget>, cx: &mut Context<Self>) {
        if self.agent != agent {
            self.agent = agent;
            cx.notify();
        }
    }

    /// A `!` typed (or pasted) first into the empty field: shell mode, the `!` becomes its badge.
    fn enter_shell_mode(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text();
        let was_empty = std::mem::replace(&mut self.was_empty, text.is_empty());
        if self.shell_mode
            || self.editing.is_some()
            || self.agent.is_some()
            || !was_empty
            || !text.starts_with('!')
        {
            return;
        }
        self.shell_mode = true;
        let rest = text[1..].to_string();
        self.was_empty = rest.is_empty();
        self.input.update(cx, |input, cx| input.set_text(&rest, cx));
        self.close_suggestions(cx);
        cx.notify();
    }

    /// Back to a message for Claude (⌫ in the empty field, Esc); the text stays.
    fn leave_shell_mode(&mut self, cx: &mut Context<Self>) {
        self.shell_mode = false;
        cx.notify();
    }

    /// A sent message back in the field, to change and send again (part 10.2): its text and
    /// pictures under a banner; sending continues the conversation from just before it, Esc puts
    /// the field's draft back.
    pub fn start_edit(
        &mut self,
        point: EditPoint,
        text: &str,
        images: Vec<Arc<Image>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft = self.input.read(cx).text();
        let draft_images = std::mem::replace(&mut self.images, images);
        // Editing another message keeps the draft from before the first one.
        let (draft, draft_images) = match self.editing.take() {
            Some(previous) => (previous.draft, previous.draft_images),
            None => (draft, draft_images),
        };
        self.editing = Some(Editing {
            point,
            draft,
            draft_images,
        });
        self.shell_mode = false;
        // The message's mentions are in its text already.
        self.context_hidden = true;
        self.was_empty = text.is_empty();
        self.input.update(cx, |input, cx| input.set_text(text, cx));
        self.close_suggestions(cx);
        self.focus(window, cx);
        cx.notify();
    }

    /// Esc on an edit: the field gets back what it held before.
    fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        self.images = editing.draft_images;
        self.was_empty = editing.draft.is_empty();
        self.input
            .update(cx, |input, cx| input.set_text(&editing.draft, cx));
        cx.notify();
    }

    /// Nothing is typed: a new question card may take the keyboard from the field.
    pub fn is_empty(&self, cx: &App) -> bool {
        self.input.read(cx).is_empty()
    }

    /// Puts text at the caret (a mention from ⌥⌘K, "Fix with Claude") and focuses the field.
    pub fn insert(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            let before = input.text();
            let caret = input.cursor();
            let needs_space = before[..char_byte(&before, caret)]
                .chars()
                .last()
                .is_some_and(|c| !c.is_whitespace());
            let mut text = if needs_space {
                format!(" {text}")
            } else {
                text.to_string()
            };
            // A mention ends with a space: typing goes on after it, and no list opens for it.
            if !text.ends_with(char::is_whitespace) {
                text.push(' ');
            }
            input.insert(&text, cx);
        });
        self.focus(window, cx);
    }

    /// Types `text` at the caret, as typed in the field: a key pressed while the conversation had
    /// the keyboard (a selection in it).
    pub fn type_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.insert(text, cx));
        self.focus(window, cx);
    }

    /// The workspace tells the field which file and lines the user works in (when sharing the
    /// selection is on in the settings).
    pub fn set_editor_context(&mut self, context: Option<EditorContext>, cx: &mut Context<Self>) {
        // UI scenarios: the chip of `FLUX_SCENARIO_CLAUDE_CONTEXT` stays — no editor is open
        // there to give it, and the window's update would take it away.
        #[cfg(feature = "scenario")]
        if std::env::var_os("FLUX_SCENARIO_CLAUDE_CONTEXT").is_some() {
            return;
        }
        if self.editor_context != context {
            self.editor_context = context;
            self.context_hidden = false;
            cx.notify();
        }
    }

    /// The chip that goes with the message, if any.
    fn shown_context(&self) -> Option<&EditorContext> {
        self.editor_context
            .as_ref()
            .filter(|_| !self.context_hidden)
    }

    fn attach(&mut self, image: Image, cx: &mut Context<Self>) {
        if !is_sendable(image.format) {
            self.notice = Some(tr("Only PNG, JPEG, GIF and WebP pictures can be attached").into());
        } else if image.bytes.len() > MAX_IMAGE_BYTES {
            self.notice = Some(tr("The picture is larger than 10 MB").into());
        } else if !self.images.iter().any(|known| known.id() == image.id()) {
            self.images.push(Arc::new(image));
            self.notice = None;
        }
        cx.notify();
    }

    /// Files dropped on the field (or elsewhere on the chat): pictures are attached, other files and
    /// folders mentioned.
    pub(crate) fn drop_paths(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_paths(paths.paths().to_vec(), window, cx)
    }

    /// Pictures attached, other files and folders mentioned.
    fn add_paths(
        &mut self,
        paths: Vec<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.shell_mode {
            return;
        }
        let root = self.session.read(cx).model().info.cwd.clone();
        let mut mentions = Vec::new();
        for path in &paths {
            if path.is_dir() {
                mentions.push(crate::claude_actions::path_mention(path, Some(&root)));
                continue;
            }
            match image_format(path) {
                Some(format) => match std::fs::read(path) {
                    Ok(bytes) => self.attach(Image::from_bytes(format, bytes), cx),
                    Err(err) => self.notice = Some(err.to_string().into()),
                },
                None => {
                    let shown = path
                        .strip_prefix(&root)
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|_| path.clone());
                    mentions.push(format!("@{}", mention_path(&shown.to_string_lossy())));
                }
            }
        }
        if !mentions.is_empty() {
            self.insert(&(mentions.join(" ") + " "), window, cx);
        }
        self.focus(window, cx);
        cx.notify();
    }

    // --- Sending ---

    fn send(&mut self, priority: Option<Priority>, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text();
        let body = text.trim();
        if self.shell_mode {
            if body.is_empty() {
                return;
            }
            let command = body.to_string();
            // ⌘↵ (`SendNow`) runs it the other way than Settings → Claude Code says.
            let answer = settings::claude(cx).answer_commands != priority.is_some();
            self.session
                .update(cx, |session, cx| session.run_shell(command, answer, cx));
            self.shell_mode = false;
            self.input.update(cx, |input, cx| input.set_text("", cx));
            self.was_empty = true;
            self.focus(window, cx);
            cx.notify();
            return;
        }
        if body.is_empty() && self.images.is_empty() && self.attachments.is_empty() {
            return;
        }
        // `/resume` is Flux's: the project's saved sessions in the history popup.
        if self.agent.is_none() && is_resume(body) {
            self.editing = None;
            self.input.update(cx, |input, cx| input.set_text("", cx));
            self.close_suggestions(cx);
            window.dispatch_action(Box::new(crate::claude::ResumeSession), cx);
            return;
        }
        // The country check blocks Claude: the message stays in the field, the banner says why.
        if region_guard::verdict(cx).blocks() {
            return;
        }
        // A slash command must stay first: no mention in front of it.
        let mut message = String::new();
        if let Some(context) = self.shown_context()
            && !body.starts_with('/')
        {
            message.push_str(&context.mention());
            message.push(' ');
        }
        message.push_str(body);
        let written = message.clone();
        // A subagent's view: Claude passes the message on (the CLI has no way to the agent).
        if let Some(agent) = &self.agent {
            message = agent.message(&message);
        }
        // A plugin's attachments go first (part 10.3); a slash command keeps them for later.
        let attach = !self.attachments.is_empty() && !body.starts_with('/');
        if attach {
            message = with_attachments(&self.attachments, &message);
        }
        let images = self
            .images
            .iter()
            .map(|image| ImageAttachment {
                media_type: image.format.mime_type().to_string(),
                data: image.bytes.clone(),
            })
            .collect();
        let working = self.session.read(cx).model().is_working();
        // ⌘↵ in a subagent's view would stop Claude's turn, and a foreground agent with it.
        let priority = priority.filter(|_| working && self.agent.is_none());
        let input = UserInput {
            text: message,
            images,
            priority,
        };
        // An edited message: the conversation goes on from just before the old one.
        if let Some(editing) = self.editing.take() {
            let input = UserInput {
                priority: None,
                ..input
            };
            self.session.update(cx, |session, cx| {
                session.send_edited(editing.point, input, cx)
            });
            self.images = editing.draft_images;
            self.was_empty = editing.draft.is_empty();
            self.input
                .update(cx, |input, cx| input.set_text(&editing.draft, cx));
            self.notice = None;
            self.close_suggestions(cx);
            self.focus(window, cx);
            cx.notify();
            return;
        }
        let sent = input.text.clone();
        let before = self.session.read(cx).model().entries.len();
        self.session
            .update(cx, |session, cx| session.send(input, cx));
        // The process couldn't start: the text stays for another try. A message that waits for
        // the country check leaves the field like a sent one (it comes back if Claude is blocked).
        let session = self.session.read(cx);
        if session.model().entries.len() == before && !session.waits_for_region() {
            return;
        }
        self.remember(
            sent,
            Returned {
                text: written,
                images: self.images.clone(),
                attachments: if attach {
                    self.attachments.clone()
                } else {
                    Vec::new()
                },
            },
        );
        // The chips go with the message: they fade out as their row folds away (when none
        // stays).
        let leaving = Chips {
            context: self.shown_context().cloned(),
            attachments: if attach {
                self.attachments.clone()
            } else {
                Vec::new()
            },
            images: self.images.clone(),
            notice: self.notice.clone(),
        };
        self.input.update(cx, |input, cx| input.set_text("", cx));
        self.images.clear();
        if attach {
            self.attachments.clear();
        }
        self.context_hidden = true;
        self.notice = None;
        self.chips_leaving = (motion::enabled()
            && !self.shell_mode
            && !leaving.is_empty()
            && self.attachments.is_empty())
        .then(|| motion::Leaving::new(leaving));
        self.close_suggestions(cx);
        self.focus(window, cx);
        cx.notify();
    }

    /// Esc: cancels editing a sent message; otherwise stops Claude's turn; otherwise leaves shell
    /// mode; otherwise stops the running `!` command.
    fn stop(&mut self, _: &Stop, _window: &mut Window, cx: &mut Context<Self>) {
        let running_shell = self.session.read(cx).running_shell();
        if self.editing.is_some() {
            self.cancel_edit(cx);
        } else if self.agent.is_some() {
            // A subagent's view: Esc goes back to the session; the bar above stops the agent.
            cx.propagate();
        } else if self.session.read(cx).model().is_working() {
            self.session.update(cx, |session, cx| session.interrupt(cx));
        } else if self.shell_mode {
            self.leave_shell_mode(cx);
        } else if let Some(id) = running_shell {
            self.session
                .update(cx, |session, cx| session.stop_shell(id, cx));
        } else {
            cx.propagate();
        }
    }

    // --- The country check (part «Флаг страны») ---

    fn remember(&mut self, message: String, returned: Returned) {
        if self.sent.len() == SENT_KEPT {
            self.sent.remove(0);
        }
        self.sent.push(Sent { message, returned });
    }

    /// What this field sent as `input`, as it was written; `None` — it didn't send it.
    pub fn recall(&mut self, input: &UserInput) -> Option<Returned> {
        let index = self
            .sent
            .iter()
            .rposition(|sent| sent.message == input.text)?;
        Some(self.sent.remove(index).returned)
    }

    /// Messages the country check sent back: their text goes before what is typed, a blank line
    /// apart, their pictures and attachments back to the chips. While a sent message is edited
    /// they go into the draft that comes back after it.
    pub fn put_back(&mut self, returned: Vec<Returned>, cx: &mut Context<Self>) {
        if returned.is_empty() {
            return;
        }
        let mut texts = Vec::new();
        let mut images = Vec::new();
        for message in returned {
            if !message.text.trim().is_empty() {
                texts.push(message.text);
            }
            images.extend(message.images);
            for attachment in message.attachments {
                if !self.attachments.contains(&attachment) {
                    self.attachments.push(attachment);
                }
            }
        }
        let joined = |typed: String| {
            let mut parts = texts.clone();
            if !typed.trim().is_empty() {
                parts.push(typed);
            }
            parts.join("\n\n")
        };
        let mut seen = std::collections::HashSet::new();
        if let Some(editing) = &mut self.editing {
            editing.draft = joined(std::mem::take(&mut editing.draft));
            images.append(&mut editing.draft_images);
            images.retain(|image| seen.insert(image.id()));
            editing.draft_images = images;
        } else {
            // A message isn't a shell command: a command being typed stays, with its `!`.
            let mut typed = self.input.read(cx).text();
            if std::mem::take(&mut self.shell_mode) && !typed.is_empty() {
                typed.insert(0, '!');
            }
            let text = joined(typed);
            // Not empty before the change: a message starting with `!` doesn't start shell mode.
            self.was_empty = false;
            self.input.update(cx, |input, cx| input.set_text(&text, cx));
            images.append(&mut self.images);
            images.retain(|image| seen.insert(image.id()));
            self.images = images;
            self.close_suggestions(cx);
        }
        self.notice = None;
        cx.notify();
    }

    /// The banner on top of the card while the country check blocks Claude: the flag (a warning
    /// sign when the country is unknown), why, and "Check Again"; while the country is checked
    /// again after the block — that it is.
    fn render_region_banner(
        &self,
        checking: bool,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let refusal = self.region_refusal.as_ref()?;
        let tone = if checking { ui.dim } else { ui.warning };
        let lead = if checking {
            pulse_dot("claude-region-checking", ui).into_any_element()
        } else {
            match &refusal.country {
                Some(code) => div()
                    .text_size(px(14.))
                    .child(region_guard::flag(code))
                    .into_any_element(),
                None => icon(IconName::Warning, ui.warning)
                    .size(px(14.))
                    .into_any_element(),
            }
        };
        let (text, color) = if checking {
            (tr("Checking the country…").to_string(), ui.text_muted)
        } else {
            (refusal.message(), ui.foreground)
        };
        let check_again = (!checking).then(|| {
            div()
                .id("claude-region-check-again")
                .flex_none()
                .h(px(20.))
                .px(px(6.))
                .mr(px(-4.))
                .flex()
                .items_center()
                .rounded(px(RADIUS_SM))
                .cursor_pointer()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(ui.accent_text)
                .hover(move |style| style.bg(ui.hover))
                .active(motion::press)
                .child(tr("Check Again"))
                .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    region_guard::check_again(cx);
                }))
        });
        let banner = div()
            .flex()
            .items_start()
            .gap(px(8.))
            .ml(px(-4.))
            .py(px(5.))
            .pl(px(9.))
            .pr(px(8.))
            .rounded(px(ui::RADIUS_MD))
            .border_1()
            .text_size(px(theme::TEXT_SM))
            .line_height(px(20.))
            .child(
                div()
                    .flex_none()
                    .w(px(16.))
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(lead),
            )
            .child(div().flex_1().min_w_0().text_color(color).child(text))
            .children(check_again)
            .with_transition("claude-region-tone", tone, motion::FADE, |banner, tone| {
                banner
                    .bg(UiColors::tint(tone, 0.1))
                    .border_color(UiColors::tint(tone, 0.3))
            });
        Some(
            Appear::slide(0., 6.)
                .curve(motion::SMOOTH)
                .apply("claude-region-banner", div().child(banner)),
        )
    }

    /// The attach button: pictures to attach, other files to mention.
    fn choose_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: Some(tr("Attach").into()),
        });
        let this = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let Ok(Ok(Some(paths))) = paths.await else {
                    return;
                };
                this.update_in(cx, |this, window, cx| this.add_paths(paths, window, cx))
                    .ok();
            })
            .detach();
    }

    fn cycle_mode(&mut self, _: &CycleMode, _window: &mut Window, cx: &mut Context<Self>) {
        let mode = next_mode(self.shown_mode(cx));
        self.choose_mode(mode, cx);
    }

    // --- The list for `@` and `/` ---

    fn update_suggestions(&mut self, cx: &mut Context<Self>) {
        let (text, caret, selection) = {
            let input = self.input.read(cx);
            (input.text(), input.cursor(), input.has_selection())
        };
        // A shell command: no mentions, no slash commands.
        let token = (!selection && !self.shell_mode)
            .then(|| token_at(&text, caret))
            .flatten();
        let Some(token) = token else {
            self.dismissed = None;
            return self.close_suggestions(cx);
        };
        if self.dismissed == Some(token.range().start) {
            return self.close_suggestions(cx);
        }
        self.dismissed = None;
        let same_kind = match (&self.suggestions, &token) {
            (Some(open), Token::Mention { .. }) => matches!(open.token, Token::Mention { .. }),
            (Some(open), Token::Command { .. }) => matches!(open.token, Token::Command { .. }),
            (None, _) => false,
        };
        if !same_kind {
            let files = match token {
                Token::Mention { .. } => Some(self.start_file_search(cx)),
                Token::Command { .. } => None,
            };
            self.suggestions = Some(Suggestions {
                token: token.clone(),
                files,
                selected: 0,
            });
        }
        if let Some(open) = &mut self.suggestions {
            if open.token.query() != token.query() {
                open.selected = 0;
            }
            if let Some(files) = &mut open.files {
                files.matcher.set_query(token.query());
                files.matcher.tick();
            }
            open.token = token;
        }
        self.sync_menu(cx);
        cx.notify();
    }

    fn start_file_search(&mut self, cx: &mut Context<Self>) -> FileSearch {
        let root = self.session.read(cx).model().info.cwd.clone();
        // nucleo calls `notify` from its own threads; the UI task coalesces the signals.
        let (notify, mut signals) = mpsc::unbounded::<()>();
        let matcher = PathMatcher::new(Arc::new(move || {
            notify.unbounded_send(()).ok();
        }));
        let refresh = cx.spawn(async move |this, cx| {
            while signals.next().await.is_some() {
                while signals.try_recv().is_ok() {}
                let refreshed = this.update(cx, |this, cx| {
                    if let Some(files) = this
                        .suggestions
                        .as_mut()
                        .and_then(|open| open.files.as_mut())
                    {
                        files.matcher.tick();
                    }
                    this.sync_menu(cx);
                    cx.notify();
                });
                if refreshed.is_err() {
                    break;
                }
                cx.background_executor().timer(REFRESH_INTERVAL).await;
            }
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let injector = matcher.injector();
        let walk = cx.background_spawn({
            let cancel = cancel.clone();
            async move {
                walk_files(&root, &cancel, |path| injector.push(path));
            }
        });
        FileSearch {
            matcher,
            cancel,
            _tasks: [refresh, walk],
        }
    }

    fn close_suggestions(&mut self, cx: &mut Context<Self>) {
        if self.suggestions.take().is_some() {
            cx.notify();
        }
        self.sync_menu(cx);
    }

    /// The rows of the open list.
    fn items(&mut self, cx: &App) -> Vec<Item> {
        let Some(open) = &mut self.suggestions else {
            return Vec::new();
        };
        match &open.token {
            Token::Mention { .. } => {
                let Some(files) = &mut open.files else {
                    return Vec::new();
                };
                let count = files.matcher.match_count().min(LIST_ROWS);
                (0..count)
                    .filter_map(|index| files.matcher.get(index))
                    .map(|found| Item::File {
                        path: found.path.to_string(),
                        positions: found.positions,
                    })
                    .collect()
            }
            Token::Command { query, .. } => {
                let model = self.session.read(cx).model();
                // Flux's `/resume` (the history popup) first, then the CLI's commands.
                let resume = SlashCommand {
                    name: RESUME.to_string(),
                    description: tr("Resume a session of this project").to_string(),
                    argument_hint: None,
                    aliases: Vec::new(),
                    builtin: true,
                };
                let commands: Vec<&SlashCommand> = std::iter::once(&resume)
                    .chain(model.commands.iter().filter(|command| {
                        !command.name.starts_with("__")
                            && command.name != RESUME
                            && !model.info.terminal_commands.contains(&command.name)
                    }))
                    .collect();
                let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
                match_list(query, &names)
                    .into_iter()
                    .take(LIST_ROWS)
                    .map(|found| {
                        let command = commands[found.index];
                        Item::Command {
                            name: command.name.clone(),
                            hint: command.argument_hint.clone(),
                            description: command.description.clone(),
                            positions: found.positions,
                        }
                    })
                    .collect()
            }
        }
    }

    /// ↑ ↓ ↵ ⇥ Esc go to the list only while it has rows (an empty one lets ↵ send).
    fn sync_menu(&mut self, cx: &mut Context<Self>) {
        let open = !self.items(cx).is_empty();
        self.input
            .update(cx, |input, cx| input.set_menu_open(open, cx));
    }

    fn move_selection(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.items(cx).len();
        if let Some(open) = &mut self.suggestions
            && count > 0
        {
            open.selected = (open.selected as isize + step).rem_euclid(count as isize) as usize;
            cx.notify();
        }
    }

    fn accept(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let items = self.items(cx);
        let (Some(item), Some(open)) = (items.get(index), &self.suggestions) else {
            return;
        };
        let range = open.token.range().clone();
        let replacement = item.replacement();
        self.suggestions = None;
        self.input
            .update(cx, |input, cx| input.replace(range, &replacement, cx));
        self.focus(window, cx);
        self.sync_menu(cx);
        cx.notify();
    }

    fn dismiss_suggestions(&mut self, cx: &mut Context<Self>) {
        self.dismissed = self
            .suggestions
            .as_ref()
            .map(|open| open.token.range().start);
        self.close_suggestions(cx);
    }

    // --- The pickers ---

    /// The model entry the session runs with: the one just picked here; the one the CLI reports
    /// (by the id the alias resolves to, otherwise by name); before the first turn, the settings'
    /// default or "default".
    fn current_model<'a>(&self, models: &'a [ModelInfo], cx: &App) -> Option<&'a ModelInfo> {
        let reported = self.session.read(cx).model().info.model.clone();
        if let Some(choice) = &self.model_choice
            && choice.reported == reported
            && let Some(value) = &choice.value
        {
            return models.iter().find(|model| &model.value == value);
        }
        if let Some(id) = &reported {
            // Several aliases may resolve to the same id ("default" and "opus"): the settings'
            // choice wins, then the first.
            let preferred = settings::claude(cx).model;
            let resolving: Vec<&ModelInfo> = models
                .iter()
                .filter(|model| model.resolved.as_deref() == Some(id.as_str()))
                .collect();
            if let Some(model) = resolving
                .iter()
                .find(|model| Some(&model.value) == preferred.as_ref())
                .or(resolving.first())
            {
                return Some(model);
            }
            let name = model_label(id);
            return models.iter().find(|model| model.display_name == name);
        }
        let value = settings::claude(cx)
            .model
            .unwrap_or_else(|| "default".to_string());
        models.iter().find(|model| model.value == value)
    }

    /// The model as the footer names it: "Opus 5.5" (the default entry names its model in the
    /// description: "Opus 5.5 · Best for everyday, complex tasks").
    fn model_name(&self, models: &[ModelInfo], cx: &App) -> SharedString {
        let reported = self.session.read(cx).model().info.model.clone();
        let picked = self
            .model_choice
            .as_ref()
            .is_some_and(|choice| choice.reported == reported);
        if !picked && let Some(id) = &reported {
            return model_label(id).into();
        }
        match self.current_model(models, cx) {
            Some(info) if info.value == "default" => info
                .description
                .split(" · ")
                .next()
                .filter(|name| !name.is_empty() && name.len() < 24)
                .map(str::to_string)
                .unwrap_or_else(|| info.display_name.clone())
                .into(),
            Some(info) => info.display_name.clone().into(),
            None => settings::claude(cx)
                .model
                .map(|value| model_label(&value))
                .unwrap_or_else(|| tr("Model").to_string())
                .into(),
        }
    }

    fn shown_effort(&self, cx: &App) -> Option<Effort> {
        Choice::shown(
            &self.effort_choice,
            &self.session.read(cx).model().info.effort,
        )
    }

    fn shown_mode(&self, cx: &App) -> PermissionMode {
        Choice::shown(
            &self.mode_choice,
            &self.session.read(cx).model().info.permission_mode,
        )
    }

    fn choose_mode(&mut self, mode: PermissionMode, cx: &mut Context<Self>) {
        let reported = self.session.read(cx).model().info.permission_mode;
        self.mode_choice = Some(Choice {
            value: mode,
            reported,
        });
        self.session
            .update(cx, |session, cx| session.set_permission_mode(mode, cx));
        cx.notify();
    }

    fn open_menu(&mut self, picker: Picker, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(open) = self.menu.take() {
            self.let_go(open);
            cx.notify();
            return;
        }
        let Some(anchor) = self.picker_bounds.get(&picker).copied() else {
            return;
        };
        // The menu sends its choice to whoever had focus: the field, which bubbles it up here.
        self.focus(window, cx);
        let model = self.session.read(cx).model();
        let mark = |on: bool, label: &str| -> SharedString {
            if on {
                format!("✓  {label}").into()
            } else {
                format!("    {label}").into()
            }
        };
        let menu = match picker {
            Picker::Model => {
                let models = model.models.clone();
                let current = self.current_model(&models, cx).map(|m| m.value.clone());
                cx.new(|cx| {
                    let mut menu = ContextMenu::new(window, cx).title(tr("Model"));
                    for info in &models {
                        menu = menu.entry_if(
                            !info.disabled,
                            mark(
                                current.as_deref() == Some(info.value.as_str()),
                                &info.display_name,
                            ),
                            ChooseModel {
                                value: Some(info.value.clone().into()),
                            },
                        );
                    }
                    menu
                })
            }
            Picker::Effort => {
                let models = model.models.clone();
                let efforts = self
                    .current_model(&models, cx)
                    .map(|info| info.efforts.clone())
                    .unwrap_or_default();
                let current = self.shown_effort(cx);
                cx.new(|cx| {
                    let mut menu = ContextMenu::new(window, cx).title(tr("Effort"));
                    for effort in efforts {
                        menu = menu.entry(
                            mark(current == Some(effort), effort_label(effort)),
                            ChooseEffort { effort },
                        );
                    }
                    menu
                })
            }
            Picker::Mode => {
                let current = self.shown_mode(cx);
                let bypass = settings::claude(cx).extra_args.iter().any(|arg| {
                    arg == "--allow-dangerously-skip-permissions"
                        || arg == "--dangerously-skip-permissions"
                });
                cx.new(|cx| {
                    let mut menu = ContextMenu::new(window, cx).title(tr("Permission Mode"));
                    for mode in [
                        PermissionMode::Default,
                        PermissionMode::AcceptEdits,
                        PermissionMode::Plan,
                        PermissionMode::DontAsk,
                    ] {
                        menu = menu
                            .entry(mark(current == mode, mode_label(mode)), ChooseMode { mode });
                    }
                    if bypass || current == PermissionMode::BypassPermissions {
                        menu = menu.separator().entry(
                            mark(
                                current == PermissionMode::BypassPermissions,
                                mode_label(PermissionMode::BypassPermissions),
                            ),
                            ChooseMode {
                                mode: PermissionMode::BypassPermissions,
                            },
                        );
                    }
                    menu
                })
            }
        };
        let focus = menu.focus_handle(cx);
        let subscriptions = [
            cx.subscribe_in(&menu, window, |this, menu, _: &DismissEvent, window, cx| {
                this.close_menu(menu, window, cx)
            }),
            cx.on_focus_out(&focus, window, {
                let menu = menu.clone();
                move |this, _, window, cx| this.close_menu(&menu, window, cx)
            }),
        ];
        window.focus(&focus);
        self.menu = Some(OpenMenu {
            menu,
            anchor,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    fn close_menu(
        &mut self,
        menu: &Entity<ContextMenu>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu.as_ref().is_none_or(|open| open.menu != *menu) {
            return;
        }
        let had_focus = menu.focus_handle(cx).contains_focused(window, cx);
        if let Some(open) = self.menu.take() {
            self.let_go(open);
        }
        if had_focus {
            self.focus(window, cx);
        }
        cx.notify();
    }

    /// A closed menu fades out toward its chip, taking no input; its subscriptions go at once.
    fn let_go(&mut self, open: OpenMenu) {
        let corner = open.corner();
        self.menu_leaving = motion::enabled().then(|| motion::Leaving::new((open.menu, corner)));
    }

    // --- Rendering ---

    /// The chips above the field: `chips`, or the field's own (`None`).
    fn render_chips(
        &self,
        chips: Option<&Chips>,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (context, attachments, images, notice) = match chips {
            Some(chips) => (
                chips.context.clone(),
                &chips.attachments,
                &chips.images,
                chips.notice.clone(),
            ),
            None => (
                self.shown_context().cloned(),
                &self.attachments,
                &self.images,
                self.notice.clone(),
            ),
        };
        if context.is_none() && attachments.is_empty() && images.is_empty() && notice.is_none() {
            return None;
        }
        // A plugin's attachment (part 10.3): its label; the whole text in the tooltip's place is
        // too long — the tooltip says it goes along.
        let attachment_chips = attachments
            .iter()
            .enumerate()
            .map(|(index, attachment)| {
                div()
                    .id(("claude-attachment-chip", index))
                    .h(px(24.))
                    .pl(px(6.))
                    .pr(px(2.))
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .rounded(px(RADIUS_SM))
                    .bg(ui.hover)
                    .border_1()
                    .border_color(ui.input_border)
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.foreground)
                    .tooltip(ui::tooltip(
                        trf("{0} goes with the message", &[&attachment.label]),
                        None,
                    ))
                    .child(icon(IconName::Paperclip, ui.text_muted).size(px(13.)))
                    .child(div().whitespace_nowrap().child(attachment.label.clone()))
                    .child(
                        ui::icon_button(("claude-attachment-remove", index), IconName::Close, ui)
                            .size(px(18.))
                            .tooltip(ui::tooltip(tr("Remove"), None))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if index < this.attachments.len() {
                                    this.attachments.remove(index);
                                }
                                cx.notify();
                            })),
                    )
            })
            // A chip that joins pops in.
            .enumerate()
            .map(|(index, chip)| {
                Appear::pop((0.5, 0.5)).apply(("claude-attachment-chip-appear", index), chip)
            });
        let context_chip = context.map(|context| {
            let file = file_icon(&context.path, &ui);
            div()
                .id("claude-context-chip")
                .h(px(24.))
                .pl(px(6.))
                .pr(px(2.))
                .flex()
                .items_center()
                .gap(px(5.))
                .rounded(px(RADIUS_SM))
                .bg(ui.hover)
                .border_1()
                .border_color(ui.input_border)
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.foreground)
                .tooltip(ui::tooltip(
                    trf("{0} goes with the message", &[&context.mention()]),
                    None,
                ))
                .child(file.render().size(px(13.)))
                .child(div().whitespace_nowrap().child(context.label()))
                .child(
                    ui::icon_button("claude-context-remove", IconName::Close, ui)
                        .size(px(18.))
                        .tooltip(ui::tooltip(tr("Remove"), None))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.context_hidden = true;
                            cx.notify();
                        })),
                )
        });
        let context_chip = context_chip
            .map(|chip| Appear::pop((0.5, 0.5)).apply("claude-context-chip-appear", chip));
        let thumbnails = images.iter().enumerate().map(|(index, image)| {
            let id = image.id();
            let thumbnail = div()
                .id(("claude-image", index))
                .group("claude-image")
                .relative()
                .size(px(THUMBNAIL_SIZE))
                .flex_none()
                .rounded(px(RADIUS_SM))
                .border_1()
                .border_color(ui.input_border)
                .overflow_hidden()
                .child(img(image.clone()).size_full().object_fit(ObjectFit::Cover))
                .child(
                    div()
                        .id(("claude-image-remove", index))
                        .absolute()
                        .top(px(2.))
                        .right(px(2.))
                        .size(px(16.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(ui.elevated)
                        .border_1()
                        .border_color(ui.elevated_border)
                        .cursor_pointer()
                        .opacity(0.)
                        .group_hover("claude-image", |style| style.opacity(1.))
                        .child(icon(IconName::Close, ui.foreground).size(px(10.)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.images.retain(|image| image.id() != id);
                            cx.notify();
                        })),
                );
            Appear::pop((0.5, 0.5)).apply(
                gpui::ElementId::NamedInteger("claude-image-appear".into(), id),
                thumbnail,
            )
        });
        Some(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.))
                .children(attachment_chips)
                .children(context_chip)
                .children(thumbnails)
                .children(notice.map(|notice| {
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.warning)
                        .child(notice)
                }))
                .into_any_element(),
        )
    }

    /// A picker under the field: its label and a chevron; the menu opens above it.
    /// The card is narrower than [`NARROW_CARD`] (as last drawn): the bottom row is shortened.
    fn narrow(&self) -> bool {
        self.card_bounds
            .is_some_and(|bounds| bounds.size.width < px(NARROW_CARD))
    }

    fn render_picker(
        &self,
        picker: Picker,
        label: SharedString,
        color: Hsla,
        tooltip: SharedString,
        shrink: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let this = cx.entity().downgrade();
        let open = self.menu.is_some()
            && self
                .picker_bounds
                .get(&picker)
                .zip(self.menu.as_ref())
                .is_some_and(|(bounds, menu)| *bounds == menu.anchor);
        let id = match picker {
            Picker::Model => "claude-model",
            Picker::Effort => "claude-effort",
            Picker::Mode => "claude-mode",
        };
        // Its backdrop crossfades as its menu opens and closes.
        let backdrop = if open {
            ui.pressed
        } else {
            gpui::transparent_black()
        };
        div()
            .id(id)
            .relative()
            .h(px(22.))
            .px(px(if self.narrow() { 4. } else { 6. }))
            .flex()
            .when(shrink, |button| button.min_w_0())
            .when(!shrink, |button| button.flex_none())
            .items_center()
            .gap(px(3.))
            .rounded(px(RADIUS_SM))
            .text_size(px(theme::TEXT_SM))
            .text_color(color)
            .whitespace_nowrap()
            .cursor_pointer()
            .hover(move |style| style.bg(ui.hover))
            .active(motion::press)
            .tooltip(ui::tooltip(tooltip, None))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_menu(picker, window, cx)
            }))
            .child(div().min_w_0().truncate().child(label))
            .child(
                icon(IconName::ChevronDown, ui.dim)
                    .size(px(11.))
                    .flex_none(),
            )
            .child(
                canvas(
                    move |bounds, _, cx| {
                        this.update(cx, |this, _| {
                            this.picker_bounds.insert(picker, bounds);
                        })
                        .ok();
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .with_transition(
                gpui::ElementId::NamedChild(Box::new(id.into()), "open".into()),
                backdrop,
                motion::FADE,
                |button, backdrop| button.bg(backdrop),
            )
    }

    /// The card's bottom row: the pickers, the context ring, attach, Send / Stop; in shell mode —
    /// what the command does with its output, and Run.
    fn render_footer(
        &self,
        has_message: bool,
        blocked: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let send = self.render_send(ui, has_message, blocked, cx);
        let row = div().h(px(FOOTER_HEIGHT)).flex().items_center().gap(px(1.));
        if self.shell_mode {
            // What ↵ does with the output, and how ⌘↵ runs it the other way.
            let answer = settings::claude(cx).answer_commands;
            let other_keys = ui::shortcut_for(&SendNow, cx).unwrap_or_default();
            let (note, other) = if answer {
                (
                    tr("Claude answers the output"),
                    trf("{0} without an answer", &[&other_keys]),
                )
            } else {
                (
                    tr("Claude only reads the output"),
                    trf("{0} with an answer", &[&other_keys]),
                )
            };
            return row
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(5.))
                        .px(px(6.))
                        .text_size(px(theme::TEXT_SM))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(ui.success)
                        .child(icon(IconName::Terminal, ui.success).size(px(13.)))
                        // In a narrow card the badge and the icon say it: room for the note.
                        .when(!self.narrow(), |label| label.child(tr("Shell command"))),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .child(if self.narrow() {
                            note.to_string()
                        } else {
                            format!("· {note} · {other}")
                        }),
                )
                .child(div().flex_1())
                .child(send);
        }
        let models = self.session.read(cx).model().models.clone();
        let narrow = self.narrow();
        let mut model_name = self.model_name(&models, cx);
        // "Opus 5.5" → "Opus": the version is in the menu.
        let family = model_name
            .split_once(' ')
            .map(|(family, _)| SharedString::from(family.to_string()));
        if narrow && let Some(family) = family {
            model_name = family;
        }
        let efforts = self
            .current_model(&models, cx)
            .map(|info| info.efforts.clone())
            .unwrap_or_default();
        let effort = self.shown_effort(cx);
        let mode = self.shown_mode(cx);
        let context = self.session.read(cx).model().context;
        let effort_picker = (!efforts.is_empty()).then(|| {
            let label = match effort {
                // The CLI's word ("xhigh"): short in any language.
                Some(effort) if narrow => effort.wire(),
                Some(effort) => effort_short(effort),
                None => tr("Effort"),
            };
            self.render_picker(
                Picker::Effort,
                label.into(),
                ui.text_muted,
                tr("Effort").into(),
                false,
                cx,
            )
        });
        // A message waits for the country check (it takes a moment): said where the meter is.
        let waits = self.session.read(cx).waits_for_region();
        let checking = waits.then(|| {
            let label = tr("Checking the country…");
            let status = div()
                .id("claude-region-wait")
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .px(px(6.))
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.text_muted)
                .child(pulse_dot("claude-region-wait-dot", ui))
                // In a narrow card the dot says it, the tooltip tells.
                .when(!narrow, |status| status.child(label))
                .when(narrow, |status| status.tooltip(ui::tooltip(label, None)));
            Appear::fade().apply("claude-region-wait-appear", status)
        });
        let ring = context.filter(|_| !waits).map(|usage| {
            context_ring(
                usage.fraction(),
                !narrow,
                trf(
                    "{0} / {1} tokens in the context",
                    &[&short_tokens(usage.used), &short_tokens(usage.max)],
                )
                .into(),
                ui,
            )
        });
        let attach = ui::icon_button("claude-attach", IconName::Paperclip, ui)
            .tooltip(ui::tooltip(tr("Attach pictures or files"), None))
            .on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| this.choose_files(window, cx)),
            );
        row.child(self.render_picker(
            Picker::Model,
            model_name,
            ui.text_muted,
            tr("Model").into(),
            false,
            cx,
        ))
        .children(effort_picker)
        .child(
            self.render_picker(
                Picker::Mode,
                mode_label(mode).into(),
                mode_color(mode, &ui),
                trf(
                    "Permission mode ({0})",
                    &[&crate::keymap::shortcut(
                        "claude_composer::CycleMode",
                        "shift-tab",
                    )],
                )
                .into(),
                true,
                cx,
            ),
        )
        .child(div().flex_1())
        .children(checking)
        .children(ring)
        .child(attach)
        .child(send)
    }

    fn render_list(&mut self, ui: UiColors, cx: &mut Context<Self>) -> Option<AnyElement> {
        let card = self.card_bounds?;
        let items = self.items(cx);
        let selected = self
            .suggestions
            .as_ref()?
            .selected
            .min(items.len().saturating_sub(1));
        if items.is_empty() {
            return None;
        }
        let rows = items.into_iter().enumerate().map(|(index, item)| {
            let row = div()
                .id(("claude-suggestion", index))
                .h(px(LIST_ROW_HEIGHT))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded(px(RADIUS_SM))
                .whitespace_nowrap()
                .overflow_hidden()
                .cursor_pointer()
                .when(index == selected, |row| row.bg(ui.list_selected))
                .when(index != selected, |row| {
                    row.hover(|style| style.bg(ui.hover))
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.accept(index, window, cx)
                }));
            match item {
                Item::File { path, positions } => {
                    let name_start = path.rfind('/').map_or(0, |slash| slash + 1);
                    let name = path[name_start..].to_string();
                    let dir = path[..name_start].trim_end_matches('/').to_string();
                    let skip = path[..name_start].chars().count();
                    let name_positions: Vec<usize> = positions
                        .iter()
                        .filter(|&&position| position >= skip)
                        .map(|position| position - skip)
                        .collect();
                    row.child(file_icon(&name, &ui).render().size(px(14.)))
                        .child(div().flex_none().child(highlighted_text(
                            name,
                            &name_positions,
                            ui.match_text,
                        )))
                        .child(div().min_w_0().truncate().text_color(ui.dim).child(dir))
                }
                Item::Command {
                    name,
                    hint,
                    description,
                    positions,
                } => row
                    .child(
                        div()
                            .flex_none()
                            .font_family(theme::code_font())
                            .text_size(px(theme::TEXT_SM))
                            .child(highlighted_text(
                                format!("/{name}"),
                                &positions.iter().map(|p| p + 1).collect::<Vec<_>>(),
                                ui.match_text,
                            )),
                    )
                    // A long hint ("<optional custom summarization instructions>") would push the
                    // description out: the description says it.
                    .children(hint.filter(|hint| hint.chars().count() <= 18).map(|hint| {
                        div()
                            .flex_none()
                            .font_family(theme::code_font())
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .child(hint)
                    }))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(ui.text_muted)
                            .child(description),
                    ),
            }
        });
        let list = popup::panel(ui)
            .w(card.size.width)
            .p(px(5.))
            .flex()
            .flex_col()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss_suggestions(cx)))
            .children(rows);
        // It grows out of the field's corner it hangs over.
        let list = Appear::pop((0., 1.)).apply("claude-suggestions-appear", list);
        Some(
            deferred(
                anchored()
                    .anchor(Corner::BottomLeft)
                    .position(point(card.left(), card.top() - px(popup::POPUP_GAP)))
                    .snap_to_window_with_margin(px(popup::WINDOW_MARGIN))
                    .child(list),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// Which of Stop and Send the corner shows.
    fn shown_buttons(&self, has_message: bool, cx: &App) -> (bool, bool) {
        if self.shell_mode {
            return (false, true);
        }
        let session = self.session.read(cx);
        let working = session.model().is_working();
        let shell_running = session.running_shell().is_some();
        let stop = self.agent.is_none() && (working || shell_running && !has_message);
        (stop, has_message || !stop)
    }

    /// Stop and Send take each other's place (a turn starts or ends, a message is typed while
    /// Claude works): the one that comes pops in where it sits (`POP`).
    fn note_buttons(&mut self, has_message: bool, window: &Window, cx: &App) {
        let shown = self.shown_buttons(has_message, cx);
        if let Some(before) = self.buttons.replace(shown)
            && before != shown
            && motion::enabled()
        {
            self.swapped = Some((Instant::now(), before));
        }
        if let Some((since, _)) = self.swapped {
            if motion::running(since, motion::POP) {
                window.request_animation_frame();
            } else {
                self.swapped = None;
            }
        }
    }

    /// Send (an accent square; green in shell mode — Run) and Stop: Claude's turn, or the running
    /// `!` command when Claude is idle.
    fn render_send(
        &self,
        ui: UiColors,
        has_message: bool,
        blocked: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let session = self.session.read(cx);
        let working = session.model().is_working();
        let shell_running = session.running_shell().is_some();
        // Send is off while the country check blocks Claude (part «Флаг страны»).
        let off = blocked && !self.shell_mode;
        let button = move |id: &'static str, name: IconName, fill: Option<Hsla>, enabled: bool| {
            div()
                .id(id)
                .size(px(SEND_SIZE))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(px(ui::RADIUS_MD))
                .when(enabled, |button| {
                    button.cursor_pointer().active(motion::press)
                })
                .when_some(fill, |button, fill| {
                    button
                        .bg(UiColors::tint(fill, 0.85))
                        .when(enabled, |button| button.hover(move |style| style.bg(fill)))
                })
                .when(fill.is_none(), |button| {
                    button
                        .bg(ui.hover)
                        .border_1()
                        .border_color(ui.input_border)
                        .hover(move |style| style.bg(ui.pressed))
                })
                .child(
                    icon(
                        name,
                        if fill.is_some() {
                            ui.on_accent
                        } else {
                            ui.foreground
                        },
                    )
                    .size(px(14.)),
                )
        };
        // Without a message Send dims; it lights up as one is typed (`FADE`). Off, it stays dim.
        let lit = if off {
            0.35
        } else if has_message {
            1.
        } else {
            0.5
        };
        if self.shell_mode {
            return div().flex().flex_none().child(
                button("claude-run", IconName::ArrowUp, Some(ui.success), true)
                    .when(!has_message, |button| button.cursor_default())
                    .tooltip(ui::tooltip(tr("Run"), Some("↵".into())))
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.send(None, window, cx)),
                    )
                    .with_transition("claude-run-lit", lit, motion::FADE, |button, opacity| {
                        button.opacity(opacity)
                    })
                    .into_any_element(),
            );
        }
        // The button that just took the other's place pops in.
        let popping = self
            .swapped
            .filter(|(since, _)| motion::running(*since, motion::POP));
        let pop_in = move |button: AnyElement, was_shown: bool| match popping {
            Some((since, _)) if !was_shown => Appear::pop((0.5, 0.5))
                .scale(0.6)
                .style(div().child(button), motion::progress(since, motion::POP))
                .into_any_element(),
            _ => button,
        };
        let before = popping.map_or((true, true), |(_, before)| before);
        let stop =
            (self.agent.is_none() && (working || shell_running && !has_message)).then(|| {
                let label = if working {
                    tr("Stop")
                } else {
                    tr("Stop the command")
                };
                let stop =
                    button("claude-stop", IconName::Stop, None, true)
                        .tooltip(ui::tooltip(label, Some("Esc".into())))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.stop(&Stop, window, cx)
                        }));
                pop_in(stop.into_any_element(), before.0)
            });
        let send = (has_message || stop.is_none()).then(|| {
            let send = button("claude-send", IconName::ArrowUp, Some(ui.accent), !off)
                .when(!has_message, |button| button.cursor_default())
                .tooltip(if off {
                    ui::tooltip(tr("Claude is blocked by the country check"), None)
                } else {
                    ui::tooltip(
                        if working {
                            tr("Send after the turn")
                        } else {
                            tr("Send")
                        },
                        Some("↵".into()),
                    )
                })
                .on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.send(None, window, cx)),
                )
                .with_transition("claude-send-lit", lit, motion::FADE, |button, opacity| {
                    button.opacity(opacity)
                });
            pop_in(send.into_any_element(), before.1)
        });
        div()
            .flex()
            .flex_none()
            .gap(px(4.))
            .pl(px(2.))
            .children(stop)
            .children(send)
    }
}

/// Follows the block of the country check in force (part «Флаг страны»): its refusal while Claude
/// is blocked, kept while the country is checked again after it; `None` once Claude may go, or
/// while a check runs that no block came before (Flux has just started).
pub(crate) fn follow_block(kept: &mut Option<Refusal>, verdict: &Verdict) {
    match verdict.refusal() {
        Some(refusal) => *kept = Some(refusal),
        None if *verdict != Verdict::Checking => *kept = None,
        None => {}
    }
}

/// A pulsing dot: the country is being checked. Animations off: a steady dot.
fn pulse_dot(id: &'static str, ui: UiColors) -> AnyElement {
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

/// How full the context is: a ring that fills clockwise from the top, and the percentage. A new
/// reading (after a turn) glides there, its color with it ([`motion::SMOOTH`]).
fn context_ring(
    fraction: f32,
    percent: bool,
    tooltip: SharedString,
    ui: UiColors,
) -> impl IntoElement {
    let fraction = fraction.clamp(0., 1.);
    // How alarming it is: 0 calm, 1 a warning (70 %), 2 an error (90 %) — the color glides
    // through the levels.
    let level = if fraction >= 0.9 {
        2.
    } else if fraction >= 0.7 {
        1.
    } else {
        0.
    };
    let label: Option<SharedString> = percent.then(|| format!("{:.0}%", fraction * 100.).into());
    div()
        .id("claude-context-meter")
        .flex()
        .flex_none()
        .items_center()
        .gap(px(5.))
        .px(px(4.))
        .text_size(px(theme::TEXT_XS))
        .tooltip(ui::tooltip(tooltip, None))
        .with_transition(
            "claude-context-ring",
            (fraction, level),
            motion::SMOOTH,
            move |meter, (fraction, level)| {
                let by_level = |colors: [Hsla; 3]| {
                    if level <= 1. {
                        motion::mix(colors[0], colors[1], level)
                    } else {
                        motion::mix(colors[1], colors[2], level - 1.)
                    }
                };
                let color = by_level([ui.accent_text, ui.warning, ui.error]);
                meter
                    .text_color(by_level([ui.text_muted, ui.warning, ui.error]))
                    .child(ring_canvas(fraction, color, ui.divider))
                    .children(label.clone())
            },
        )
}

/// The ring of [`context_ring`]: the track, and the arc of `fraction` over it.
fn ring_canvas(fraction: f32, color: Hsla, track: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let center = bounds.center();
            let radius = px((RING_SIZE - RING_LINE) / 2.);
            let top = point(center.x, center.y - radius);
            let bottom = point(center.x, center.y + radius);
            let circle = |path: &mut PathBuilder| {
                path.move_to(top);
                path.arc_to(point(radius, radius), px(0.), false, true, bottom);
                path.arc_to(point(radius, radius), px(0.), false, true, top);
            };
            let mut back = PathBuilder::stroke(px(RING_LINE));
            circle(&mut back);
            if let Ok(path) = back.build() {
                window.paint_path(path, track);
            }
            if fraction < 0.005 {
                return;
            }
            let mut arc = PathBuilder::stroke(px(RING_LINE));
            if fraction >= 0.995 {
                circle(&mut arc);
            } else {
                let angle = fraction * std::f32::consts::TAU;
                let end = point(
                    center.x + radius * angle.sin(),
                    center.y - radius * angle.cos(),
                );
                arc.move_to(top);
                arc.arc_to(point(radius, radius), px(0.), fraction > 0.5, true, end);
            }
            if let Ok(path) = arc.build() {
                window.paint_path(path, color);
            }
        },
    )
    .size(px(RING_SIZE))
    .flex_none()
}

/// The byte offset of character `index` in `text`.
fn char_byte(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map_or(text.len(), |(byte, _)| byte)
}

impl Focusable for Composer {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let (working, waiting, exited) = {
            let model = self.session.read(cx).model();
            (
                model.is_working(),
                !model.pending.is_empty(),
                matches!(model.status, Status::Exited { .. }),
            )
        };
        // The country check: the banner keeps its refusal while the country is checked again.
        let verdict = region_guard::verdict(cx);
        let blocked = verdict.blocks();
        let rechecking = self.region_refusal.is_some() && verdict == Verdict::Checking;
        let placeholder = if (blocked || rechecking) && !self.shell_mode {
            tr("The text stays here until Claude is unblocked")
        } else if let Some(agent) = &self.agent {
            if agent.waits {
                tr("Delivered when the agent is done")
            } else {
                tr("A message for the agent")
            }
        } else if self.shell_mode {
            tr("A command, e.g. git status")
        } else if waiting {
            tr("Answer Claude above, or write here")
        } else if working {
            tr("Claude is working — your message will wait")
        } else if exited {
            tr("Claude stopped — a message resumes the session")
        } else {
            tr("Ask Claude… @ for files, / for commands")
        };
        self.input
            .update(cx, |input, _| input.set_placeholder(placeholder));
        let has_message =
            !self.input.read(cx).is_empty() || !self.shell_mode && !self.images.is_empty();
        self.note_buttons(has_message, window, cx);
        let focused = self.input.focus_handle(cx).is_focused(window);
        let chips = (!self.shell_mode)
            .then(|| self.render_chips(None, ui, cx))
            .flatten();
        if chips.is_some()
            || self
                .chips_leaving
                .as_ref()
                .is_some_and(motion::Leaving::done)
        {
            self.chips_leaving = None;
        }
        let chips = match (chips, &self.chips_leaving) {
            // Measured: the row folds away from its height when the chips go with a message.
            (Some(chips), _) => {
                let height = self.chips_height.clone();
                Some(
                    div()
                        .relative()
                        .child(chips)
                        .child(
                            canvas(
                                move |bounds, _, _| height.set(bounds.size.height.into()),
                                |_, _, _, _| {},
                            )
                            .absolute()
                            .size_full(),
                        )
                        .into_any_element(),
                )
            }
            (None, Some(leaving)) => {
                let progress = leaving.progress(window);
                self.render_chips(Some(&leaving.value), ui, cx)
                    .map(|chips| {
                        // The card's gap goes with the row.
                        div()
                            .h(px(self.chips_height.get() * (1. - progress)))
                            .mb(px(-CARD_GAP * progress))
                            .overflow_hidden()
                            .child(motion::inert(motion::leaving_style(
                                div().child(chips),
                                progress,
                                (0., 1.),
                            )))
                            .into_any_element()
                    })
            }
            (None, None) => None,
        };
        let footer = self.render_footer(has_message, blocked, cx);
        let region_banner = self.render_region_banner(rechecking, ui, cx);
        // The `!` of shell mode: a badge in front of the command.
        let badge = self.shell_mode.then(|| {
            div()
                .flex_none()
                .h(px(20.))
                .px(px(6.))
                .mt(px(1.))
                .flex()
                .items_center()
                .rounded(px(RADIUS_SM))
                .bg(UiColors::tint(ui.success, 0.16))
                .font_family(theme::code_font())
                .text_size(px(theme::TEXT_SM))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(ui.success)
                .child("!")
        });
        // A subagent's view: whom the message is for.
        let agent_banner = self
            .agent
            .as_ref()
            .filter(|_| self.editing.is_none())
            .map(|agent| {
                div()
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(theme::TEXT_SM))
                    .child(
                        icon(IconName::Agent, ui.accent_text)
                            .size(px(12.))
                            .flex_none(),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(ui.accent_text)
                            .child(trf(
                                "Through Claude to the agent «{0}»",
                                &[&agent.description],
                            )),
                    )
            });
        let banner = self.editing.as_ref().map(|_| {
            div()
                .h(px(20.))
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(theme::TEXT_SM))
                .child(
                    icon(IconName::Pencil, ui.accent_text)
                        .size(px(12.))
                        .flex_none(),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(ui.accent_text)
                        .child(tr("Editing a message")),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .id("claude-cancel-edit")
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(4.))
                        .px(px(4.))
                        .rounded(px(RADIUS_SM))
                        .cursor_pointer()
                        .hover(move |style| style.bg(ui.hover))
                        .text_color(ui.dim)
                        .child(ui::keys("Esc", ui))
                        .child(tr("cancel"))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.cancel_edit(cx)
                        })),
                )
        });
        let list = self.render_list(ui, cx);
        // The menu grows out of its chip; a closed one fades out toward it, without input.
        let menu = self.menu.as_ref().map(|open| {
            let corner = open.corner();
            let view = Appear::pop((0., 1.)).apply_from(
                ("claude-composer-menu", open.menu.entity_id().as_u64()),
                corner,
                div().child(open.menu.clone()),
            );
            place_menu(corner, view)
        });
        if self
            .menu_leaving
            .as_ref()
            .is_some_and(motion::Leaving::done)
        {
            self.menu_leaving = None;
        }
        let leaving_menu = self.menu_leaving.as_ref().map(|leaving| {
            let (menu, corner) = leaving.value.clone();
            let view = motion::leaving_from(div().child(menu), leaving.progress(window), corner);
            place_menu(corner, motion::inert(view))
        });
        let this = cx.entity().downgrade();
        let card = div()
            .id("claude-composer-card")
            .relative()
            .flex()
            .flex_col()
            .gap(px(CARD_GAP))
            .pt(px(8.))
            .pb(px(6.))
            .pl(px(12.))
            .pr(px(8.))
            .rounded(px(RADIUS_LG))
            .bg(ui.input_background)
            .border_1()
            .drag_over::<ExternalPaths>(move |style, _, _, _| {
                style.border_color(ui.focus_border).bg(ui.drop_target)
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.drop_paths(paths, window, cx)
            }))
            // A click on the card's padding goes to the field.
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.focus(window, cx)))
            // The country check blocks Claude (part «Флаг страны»).
            .children(region_banner)
            // Editing a sent message (part 10.2).
            .children(banner)
            .children(agent_banner)
            .children(chips)
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(6.))
                    .children(badge)
                    .child(div().flex_1().min_w_0().child(self.input.clone())),
            )
            .child(div().ml(px(-6.)).child(footer))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        this.update(cx, |this, cx| {
                            let narrow = |bounds: Option<Bounds<Pixels>>| {
                                bounds.is_some_and(|bounds| bounds.size.width < px(NARROW_CARD))
                            };
                            let was = narrow(this.card_bounds);
                            this.card_bounds = Some(bounds);
                            // The bottom row follows the card's width: the next frame, at once.
                            if narrow(Some(bounds)) != was {
                                cx.notify();
                            }
                        })
                        .ok();
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            );
        let card = card_ring(card, focused, self.shell_mode, ui);
        div()
            .key_context(CONTEXT)
            .on_action(cx.listener(|this, _: &Send, window, cx| this.send(None, window, cx)))
            .on_action(cx.listener(|this, _: &SendNow, window, cx| {
                this.send(Some(Priority::Now), window, cx)
            }))
            .on_action(cx.listener(Self::stop))
            .on_action(cx.listener(Self::cycle_mode))
            .on_action(
                cx.listener(|this, _: &prompt_input::MenuNext, _, cx| this.move_selection(1, cx)),
            )
            .on_action(cx.listener(|this, _: &prompt_input::MenuPrevious, _, cx| {
                this.move_selection(-1, cx)
            }))
            .on_action(
                cx.listener(|this, _: &prompt_input::MenuAccept, window, cx| {
                    let selected = this.suggestions.as_ref().map_or(0, |open| open.selected);
                    this.accept(selected, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &prompt_input::MenuDismiss, _, cx| {
                this.dismiss_suggestions(cx)
            }))
            .on_action(cx.listener(|this, action: &ChooseModel, _, cx| {
                let value = action.value.as_ref().map(|value| value.to_string());
                let reported = this.session.read(cx).model().info.model.clone();
                this.model_choice = Some(Choice {
                    value: value.clone(),
                    reported,
                });
                this.session
                    .update(cx, |session, cx| session.set_model(value, cx));
                cx.notify();
            }))
            .on_action(cx.listener(|this, action: &ChooseEffort, _, cx| {
                let effort = action.effort;
                let reported = this.session.read(cx).model().info.effort;
                this.effort_choice = Some(Choice {
                    value: Some(effort),
                    reported,
                });
                this.session
                    .update(cx, |session, cx| session.set_effort(Some(effort), cx));
                cx.notify();
            }))
            .on_action(
                cx.listener(|this, action: &ChooseMode, _, cx| this.choose_mode(action.mode, cx)),
            )
            .flex()
            .flex_col()
            .px(px(10.))
            .pt(px(8.))
            .pb(px(10.))
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_MD))
            .child(card)
            .children(list)
            .children(leaving_menu)
            .children(menu)
    }
}

/// The card's focus ring, as [`ui::focus_field`] draws a field's: it comes in wide and settles
/// around the card when the field takes focus ([`motion::POP`]), fades when it leaves
/// ([`motion::FADE`]). In shell mode it stays, green: entering and leaving the mode crossfade the
/// colors.
fn card_ring(
    card: gpui::Stateful<gpui::Div>,
    focused: bool,
    shell: bool,
    ui: UiColors,
) -> impl IntoElement {
    let on = focused || shell;
    let target = (if on { 1. } else { 0. }, if shell { 1. } else { 0. });
    let curve = if on { motion::POP } else { motion::FADE };
    card.with_transition(
        "claude-composer-ring",
        target,
        curve,
        move |card, (t, green)| {
            let ui = UiColors {
                focus_border: motion::mix(ui.focus_border, ui.success, green),
                focus_ring: motion::mix(ui.focus_ring, UiColors::tint(ui.success, 0.2), green),
                ..ui
            };
            let card = card.border_color(motion::mix(ui.input_border, ui.focus_border, t));
            if t <= 0. {
                card
            } else {
                card.shadow(ui::focus_ring_at(ui, t, on))
            }
        },
    )
}

/// A picker's menu over the window, its bottom-left corner at `corner` (above the chip); near the
/// window's edge it is shifted inward.
fn place_menu(corner: Point<Pixels>, view: impl IntoElement) -> AnyElement {
    deferred(
        anchored()
            .anchor(Corner::BottomLeft)
            .position(corner)
            .snap_to_window_with_margin(px(popup::WINDOW_MARGIN))
            .child(view),
    )
    .with_priority(1)
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_for_a_subagent_names_it_for_send_message() {
        let mut agent = AgentTarget {
            description: "Find the tests".into(),
            agent_id: Some("a928daf3b62218c72".into()),
            waits: false,
        };
        let message = agent.message("Skip the slow ones");
        assert!(message.contains("«Find the tests»"));
        assert!(message.contains("agent id a928daf3b62218c72"));
        assert!(message.contains("SendMessage"));
        assert!(message.ends_with("\n\nSkip the slow ones"));
        // Before the CLI reports the id: the description only.
        agent.agent_id = None;
        assert!(!agent.message("x").contains("agent id"));
    }

    #[test]
    fn resume_is_flux_s_own_command() {
        assert!(is_resume("/resume"));
        assert!(is_resume("/resume  fix login"));
        assert!(!is_resume("/resumes"));
        assert!(!is_resume("resume"));
        assert!(!is_resume("/compact"));
    }

    #[test]
    fn mentions_and_commands_are_found_at_the_caret() {
        assert_eq!(
            token_at("look at @src/ma", 15),
            Some(Token::Mention {
                range: 8..15,
                query: "src/ma".into()
            })
        );
        // The caret inside the word: the query is before it, the token runs to the word's end.
        assert_eq!(
            token_at("@main.rs please", 3),
            Some(Token::Mention {
                range: 0..8,
                query: "ma".into()
            })
        );
        assert_eq!(
            token_at("/comp", 5),
            Some(Token::Command {
                range: 0..5,
                query: "comp".into()
            })
        );
        // A slash in the middle of the message, an e-mail, a finished word: nothing.
        assert_eq!(token_at("see a/b", 7), None);
        assert_eq!(token_at("me@host", 7), None);
        assert_eq!(token_at("@file ", 6), None);
        assert_eq!(token_at("", 0), None);
    }

    #[test]
    fn the_editor_context_becomes_a_mention() {
        let context = |lines| EditorContext {
            path: "src/main.rs".into(),
            lines,
        };
        assert_eq!(context(None).mention(), "@src/main.rs");
        assert_eq!(context(Some((12, 12))).mention(), "@src/main.rs#L12");
        assert_eq!(context(Some((12, 20))).mention(), "@src/main.rs#L12-20");
        assert_eq!(context(Some((12, 20))).label(), "main.rs:12–20");
        let spaced = EditorContext {
            path: "my docs/a b.md".into(),
            lines: None,
        };
        assert_eq!(spaced.mention(), "@\"my docs/a b.md\"");
    }

    #[test]
    fn model_ids_read_as_names() {
        assert_eq!(model_label("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_label("claude-haiku-4-5-20251001"), "Haiku 4.5");
        assert_eq!(model_label("claude-sonnet-5"), "Sonnet 5");
        assert_eq!(model_label("opus"), "Opus");
    }

    #[test]
    fn modes_cycle_like_the_cli() {
        assert_eq!(
            next_mode(PermissionMode::Default),
            PermissionMode::AcceptEdits
        );
        assert_eq!(next_mode(PermissionMode::AcceptEdits), PermissionMode::Plan);
        assert_eq!(next_mode(PermissionMode::Plan), PermissionMode::Default);
        assert_eq!(next_mode(PermissionMode::DontAsk), PermissionMode::Default);
    }

    #[test]
    fn token_counts_are_short() {
        assert_eq!(short_tokens(950), "950");
        assert_eq!(short_tokens(28_900), "28.9k");
        assert_eq!(short_tokens(200_000), "200k");
        assert_eq!(short_tokens(1_000_000), "1M");
    }
}
