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
//! - Under the field: the model, effort and permission mode pickers (⇧⇥ cycles Ask before edits →
//!   Accept edits → Plan mode, as in the CLI) and the context meter.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use flux_claude::{
    Effort, ImageAttachment, ModelInfo, PermissionMode, Priority, SlashCommand, Status, UserInput,
};
use flux_search::{PathMatcher, match_list, walk_files};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    Action, AnyElement, App, Bounds, ClickEvent, Context, Corner, DismissEvent, Entity,
    ExternalPaths, FocusHandle, Focusable, Hsla, Image, ImageFormat, KeyBinding,
    ObjectFit, Pixels, Render, SharedString, Subscription, Task, Window, actions, anchored,
    canvas, deferred, div, img, point, prelude::*, px,
};

use crate::claude_session::{ClaudeSession, SessionEvent};
use crate::context_menu::ContextMenu;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, file_icon, icon};
use crate::picker::highlighted_text;
use crate::popup;
use crate::prompt_input::{self, PromptInput, PromptInputEvent};
use crate::settings;
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_LG, RADIUS_SM};

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
const FOOTER_HEIGHT: f32 = 26.;
/// Redraws on nucleo's signals happen at most once per frame (as in ⌘P).
const REFRESH_INTERVAL: Duration = Duration::from_millis(16);

pub fn init(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("enter", Send, context),
        KeyBinding::new("cmd-enter", SendNow, context),
        KeyBinding::new("escape", Stop, context),
        KeyBinding::new("shift-tab", CycleMode, context),
    ]);
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

pub struct Composer {
    session: Entity<ClaudeSession>,
    input: Entity<PromptInput>,
    /// The active editor's file and selection: a chip above the field, sent along as a mention.
    editor_context: Option<EditorContext>,
    /// The chip was removed (or sent) for this context; it comes back when the context changes.
    context_hidden: bool,
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
    /// Where the card and the pickers were drawn: the list and the menus open above them.
    card_bounds: Option<Bounds<Pixels>>,
    picker_bounds: HashMap<Picker, Bounds<Pixels>>,
    _subscriptions: Vec<Subscription>,
}

impl Composer {
    pub fn new(session: Entity<ClaudeSession>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            PromptInput::new(tr("Ask Claude… @ for files, / for commands"), cx).max_rows(MESSAGE_ROWS)
        });
        let subscriptions = vec![
            cx.subscribe(&input, |this, _, event: &PromptInputEvent, cx| match event {
                PromptInputEvent::Changed => {
                    this.notice = None;
                    this.update_suggestions(cx);
                    cx.notify();
                }
                PromptInputEvent::SelectionChanged => this.update_suggestions(cx),
                PromptInputEvent::ImagePasted(image) => this.attach(image.clone(), cx),
            }),
            // The session refreshes the context meter after each turn itself.
            cx.subscribe(&session, |_, _, _: &SessionEvent, cx| cx.notify()),
        ];
        // The meter shows what the conversation starts with (the system prompt, the tools).
        session.update(cx, |session, cx| session.refresh_context(cx));
        #[allow(unused_mut)]
        let mut composer = Self {
            session,
            input,
            editor_context: None,
            context_hidden: false,
            images: Vec::new(),
            notice: None,
            suggestions: None,
            dismissed: None,
            model_choice: None,
            effort_choice: None,
            mode_choice: None,
            menu: None,
            card_bounds: None,
            picker_bounds: HashMap::new(),
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

    /// The workspace tells the field which file and lines the user works in (when sharing the
    /// selection is on in the settings).
    pub fn set_editor_context(&mut self, context: Option<EditorContext>, cx: &mut Context<Self>) {
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

    /// Files dropped on the field: pictures are attached, other files mentioned.
    fn drop_paths(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.session.read(cx).model().info.cwd.clone();
        let mut mentions = Vec::new();
        for path in paths.paths() {
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
        if body.is_empty() && self.images.is_empty() {
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
        let images = self
            .images
            .iter()
            .map(|image| ImageAttachment {
                media_type: image.format.mime_type().to_string(),
                data: image.bytes.clone(),
            })
            .collect();
        let working = self.session.read(cx).model().is_working();
        let input = UserInput {
            text: message,
            images,
            priority: priority.filter(|_| working),
        };
        let before = self.session.read(cx).model().entries.len();
        self.session
            .update(cx, |session, cx| session.send(input, cx));
        // The process couldn't start: the text stays for another try.
        if self.session.read(cx).model().entries.len() == before {
            return;
        }
        self.input.update(cx, |input, cx| input.set_text("", cx));
        self.images.clear();
        self.context_hidden = true;
        self.notice = None;
        self.close_suggestions(cx);
        self.focus(window, cx);
        cx.notify();
    }

    fn stop(&mut self, _: &Stop, _window: &mut Window, cx: &mut Context<Self>) {
        if self.session.read(cx).model().is_working() {
            self.session.update(cx, |session, cx| session.interrupt(cx));
        } else {
            cx.propagate();
        }
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
        let token = (!selection).then(|| token_at(&text, caret)).flatten();
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
                let commands: Vec<&SlashCommand> = model
                    .commands
                    .iter()
                    .filter(|command| {
                        !command.name.starts_with("__")
                            && !model.info.terminal_commands.contains(&command.name)
                    })
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
        Choice::shown(&self.effort_choice, &self.session.read(cx).model().info.effort)
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
        if self.menu.take().is_some() {
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
                            mark(current.as_deref() == Some(info.value.as_str()), &info.display_name),
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
                        menu = menu.entry(
                            mark(current == mode, mode_label(mode)),
                            ChooseMode { mode },
                        );
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

    fn close_menu(&mut self, menu: &Entity<ContextMenu>, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.as_ref().is_none_or(|open| open.menu != *menu) {
            return;
        }
        let had_focus = menu.focus_handle(cx).contains_focused(window, cx);
        self.menu = None;
        if had_focus {
            self.focus(window, cx);
        }
        cx.notify();
    }

    // --- Rendering ---

    fn render_chips(&self, ui: UiColors, cx: &mut Context<Self>) -> Option<AnyElement> {
        let context = self.shown_context().cloned();
        if context.is_none() && self.images.is_empty() && self.notice.is_none() {
            return None;
        }
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
        let thumbnails = self.images.iter().enumerate().map(|(index, image)| {
            let id = image.id();
            div()
                .id(("claude-image", index))
                .group("claude-image")
                .relative()
                .size(px(THUMBNAIL_SIZE))
                .flex_none()
                .rounded(px(RADIUS_SM))
                .border_1()
                .border_color(ui.input_border)
                .overflow_hidden()
                .child(
                    img(image.clone())
                        .size_full()
                        .object_fit(ObjectFit::Cover),
                )
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
                        .invisible()
                        .group_hover("claude-image", |style| style.visible())
                        .child(icon(IconName::Close, ui.foreground).size(px(10.)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.images.retain(|image| image.id() != id);
                            cx.notify();
                        })),
                )
        });
        Some(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.))
                .children(context_chip)
                .children(thumbnails)
                .children(self.notice.clone().map(|notice| {
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.warning)
                        .child(notice)
                }))
                .into_any_element(),
        )
    }

    /// A picker under the field: its label and a chevron; the menu opens above it.
    fn render_picker(
        &self,
        picker: Picker,
        label: SharedString,
        color: Hsla,
        tooltip: &'static str,
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
        div()
            .id(match picker {
                Picker::Model => "claude-model",
                Picker::Effort => "claude-effort",
                Picker::Mode => "claude-mode",
            })
            .relative()
            .h(px(22.))
            .px(px(6.))
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
            .when(open, |button| button.bg(ui.pressed))
            .hover(move |style| style.bg(ui.hover))
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
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let models = self.session.read(cx).model().models.clone();
        let model_name = self.model_name(&models, cx);
        let efforts = self
            .current_model(&models, cx)
            .map(|info| info.efforts.clone())
            .unwrap_or_default();
        let effort = self.shown_effort(cx);
        let mode = self.shown_mode(cx);
        let context = self.session.read(cx).model().context;
        let effort_picker = (!efforts.is_empty()).then(|| {
            let label = effort.map_or(tr("Effort"), effort_short);
            self.render_picker(Picker::Effort, label.into(), ui.text_muted, tr("Effort"), false, cx)
        });
        let meter = context.map(|usage| {
            let fraction = usage.fraction();
            let color = if fraction >= 0.9 {
                ui.error
            } else if fraction >= 0.7 {
                ui.warning
            } else {
                ui.dim
            };
            const BAR: f32 = 20.;
            div()
                .id("claude-context-meter")
                .flex()
                .flex_none()
                .items_center()
                .gap(px(5.))
                .pl(px(4.))
                .text_size(px(theme::TEXT_XS))
                .text_color(color)
                .tooltip(ui::tooltip(
                    trf(
                        "{0} / {1} tokens in the context",
                        &[&short_tokens(usage.used), &short_tokens(usage.max)],
                    ),
                    None,
                ))
                .child(
                    div()
                        .w(px(BAR))
                        .h(px(4.))
                        .rounded(px(2.))
                        .bg(ui.divider)
                        .child(
                            div()
                                .h_full()
                                .w(px((BAR * fraction).max(2.)))
                                .rounded(px(2.))
                                .bg(if fraction >= 0.7 { color } else { ui.text_muted }),
                        ),
                )
                .child(format!("{:.0}%", fraction * 100.))
        });
        div()
            .h(px(FOOTER_HEIGHT))
            .flex()
            .items_center()
            .gap(px(1.))
            .child(self.render_picker(
                Picker::Model,
                model_name,
                ui.text_muted,
                tr("Model"),
                false,
                cx,
            ))
            .children(effort_picker)
            .child(self.render_picker(
                Picker::Mode,
                mode_label(mode).into(),
                mode_color(mode, &ui),
                tr("Permission mode (⇧⇥)"),
                true,
                cx,
            ))
            .child(div().flex_1())
            .children(meter)
    }

    fn render_list(&mut self, ui: UiColors, cx: &mut Context<Self>) -> Option<AnyElement> {
        let card = self.card_bounds?;
        let items = self.items(cx);
        let selected = self.suggestions.as_ref()?.selected.min(items.len().saturating_sub(1));
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
                .when(index != selected, |row| row.hover(|style| style.bg(ui.hover)))
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
                        .child(
                            div()
                                .flex_none()
                                .child(highlighted_text(name, &name_positions, ui.match_text)),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(ui.dim)
                                .child(dir),
                        )
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

    fn render_send(&self, ui: UiColors, working: bool, has_message: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let button = |id: &'static str, name: IconName, accent: bool| {
            div()
                .id(id)
                .size(px(26.))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(px(RADIUS_SM))
                .cursor_pointer()
                .when(accent, |button| {
                    button
                        .bg(UiColors::tint(ui.accent, 0.85))
                        .hover(move |style| style.bg(ui.accent))
                })
                .when(!accent, |button| {
                    button
                        .bg(ui.hover)
                        .border_1()
                        .border_color(ui.input_border)
                        .hover(move |style| style.bg(ui.pressed))
                })
                .child(icon(name, if accent { ui.foreground } else { ui.text_muted }).size(px(14.)))
        };
        div()
            .flex()
            .flex_none()
            .gap(px(4.))
            .when(working, |row| {
                row.child(
                    button("claude-stop", IconName::Stop, false)
                        .tooltip(ui::tooltip(tr("Stop"), Some("Esc".into())))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.stop(&Stop, window, cx)
                        })),
                )
            })
            .when(has_message || !working, |row| {
                row.child(
                    button("claude-send", IconName::ArrowUp, has_message)
                        .when(!has_message, |button| button.opacity(0.5).cursor_default())
                        .tooltip(ui::tooltip(
                            if working { tr("Send after the turn") } else { tr("Send") },
                            Some("↵".into()),
                        ))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.send(None, window, cx)
                        })),
                )
            })
    }
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
        let placeholder = if waiting {
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
        let has_message = !self.input.read(cx).is_empty() || !self.images.is_empty();
        let focused = self.input.focus_handle(cx).is_focused(window);
        let chips = self.render_chips(ui, cx);
        let send = self.render_send(ui, working, has_message, cx);
        let footer = self.render_footer(cx);
        let list = self.render_list(ui, cx);
        let menu = self.menu.as_ref().map(|open| {
            deferred(
                anchored()
                    .anchor(Corner::BottomLeft)
                    .position(point(open.anchor.left(), open.anchor.top() - px(popup::POPUP_GAP)))
                    .snap_to_window_with_margin(px(popup::WINDOW_MARGIN))
                    .child(open.menu.clone()),
            )
            .with_priority(1)
        });
        let this = cx.entity().downgrade();
        let card = div()
            .id("claude-composer-card")
            .relative()
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(8.))
            .rounded(px(RADIUS_LG))
            .bg(ui.input_background)
            .border_1()
            .border_color(if focused { ui.focus_border } else { ui.input_border })
            .when(focused, |card| card.shadow(ui::focus_ring(ui)))
            .drag_over::<ExternalPaths>(move |style, _, _, _| {
                style.border_color(ui.focus_border).bg(ui.drop_target)
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.drop_paths(paths, window, cx)
            }))
            // A click on the card's padding goes to the field.
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.focus(window, cx)))
            .children(chips)
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap(px(6.))
                    .child(div().flex_1().min_w_0().child(self.input.clone()))
                    .child(send),
            )
            .child(
                canvas(
                    move |bounds, _, cx| {
                        this.update(cx, |this, _| this.card_bounds = Some(bounds)).ok();
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            );
        div()
            .key_context(CONTEXT)
            .on_action(cx.listener(|this, _: &Send, window, cx| this.send(None, window, cx)))
            .on_action(cx.listener(|this, _: &SendNow, window, cx| {
                this.send(Some(Priority::Now), window, cx)
            }))
            .on_action(cx.listener(Self::stop))
            .on_action(cx.listener(Self::cycle_mode))
            .on_action(cx.listener(|this, _: &prompt_input::MenuNext, _, cx| {
                this.move_selection(1, cx)
            }))
            .on_action(cx.listener(|this, _: &prompt_input::MenuPrevious, _, cx| {
                this.move_selection(-1, cx)
            }))
            .on_action(cx.listener(|this, _: &prompt_input::MenuAccept, window, cx| {
                let selected = this.suggestions.as_ref().map_or(0, |open| open.selected);
                this.accept(selected, window, cx)
            }))
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
            .on_action(cx.listener(|this, action: &ChooseMode, _, cx| {
                this.choose_mode(action.mode, cx)
            }))
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(px(10.))
            .pt(px(8.))
            .pb(px(6.))
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_MD))
            .child(card)
            .child(footer)
            .children(list)
            .children(menu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(next_mode(PermissionMode::Default), PermissionMode::AcceptEdits);
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
