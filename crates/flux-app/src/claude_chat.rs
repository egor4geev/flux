//! The chat of one Claude session: the conversation, the questions for the user and the task list,
//! the message field. The same view lives in the Claude window (the island on the right) or in an
//! editor tab; the workspace moves it between them.
//!
//! A resumed session (the history, Flux restarting) shows "Loading…" while its transcript is read
//! and starts its process the first time its chat is drawn — in sight — not before.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, Render, Subscription, Task, Window,
    div, prelude::*, px,
};

use flux_claude::Session;

use crate::claude_cards::{self, CardsState};
use crate::claude_composer::Composer;
use crate::claude_session::{ClaudeSession, SessionEvent};
use crate::claude_transcript::{self, TranscriptState};
use crate::i18n::tr;
use crate::theme::{self, Theme};

/// How often the line of what Claude does is redrawn while it works (its clock).
const TICK: Duration = Duration::from_secs(1);

/// What the chat asks of the window.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaudeChatEvent {
    /// "Open Diff" on an edit card (the CLI's request id).
    OpenProposal(String),
    /// A file Claude read or changed, clicked in the conversation.
    OpenLocation(crate::workspace::Location),
}

pub struct ClaudeChat {
    session: Entity<ClaudeSession>,
    pub(crate) transcript: TranscriptState,
    pub(crate) cards: CardsState,
    composer: Entity<Composer>,
    focus_handle: FocusHandle,
    /// Redraws the chat each second while Claude works.
    ticker: Option<Task<()>>,
    /// A resumed session without a process: it starts when the chat is first drawn.
    start_in_sight: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ClaudeChatEvent> for ClaudeChat {}

impl ClaudeChat {
    pub fn new(
        session: Entity<ClaudeSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| Composer::new(session.clone(), window, cx));
        let subscriptions = vec![cx.subscribe_in(
            &session,
            window,
            |this: &mut Self, session, event: &SessionEvent, window, cx| {
                this.transcript.invalidate();
                this.update_ticker(cx);
                match event {
                    // A question takes the keyboard, unless the user is typing a message.
                    SessionEvent::PendingAdded(_) if this.composer.read(cx).is_empty(cx) => {
                        claude_cards::take_focus(&mut this.cards, session.read(cx).model(), window)
                    }
                    // The last one answered: back to the message field.
                    SessionEvent::PendingRemoved(_)
                        if session.read(cx).model().pending.is_empty()
                            && claude_cards::has_focus(&this.cards, window) =>
                    {
                        this.focus_composer(window, cx)
                    }
                    _ => {}
                }
                cx.notify();
            },
        )];
        // A saved session back without a process (a new one that failed to start has no id).
        let start_in_sight = {
            let session = session.read(cx);
            !session.is_started() && session.session_id().is_some()
        };
        let mut chat = Self {
            session,
            start_in_sight,
            transcript: TranscriptState::new(cx.weak_entity()),
            cards: CardsState::default(),
            composer,
            focus_handle: cx.focus_handle(),
            ticker: None,
            _subscriptions: subscriptions,
        };
        chat.update_ticker(cx);
        chat
    }

    pub fn session(&self) -> &Entity<ClaudeSession> {
        &self.session
    }

    pub fn composer(&self) -> &Entity<Composer> {
        &self.composer
    }

    /// Focus to the message field.
    pub fn focus_composer(&self, window: &mut Window, cx: &App) {
        self.composer.read(cx).focus(window, cx);
    }

    /// Puts a mention or a prepared request into the message (⌥⌘K, "Fix with Claude").
    pub fn insert(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.composer
            .update(cx, |composer, cx| composer.insert(text, window, cx));
    }

    /// Files and folders dropped on the chat: mentions in the message, the field focused.
    fn drop_mentions(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.session.read(cx).model().info.cwd.clone();
        let mentions: Vec<String> = paths
            .iter()
            .map(|path| crate::claude_actions::path_mention(path, Some(&root)))
            .collect();
        if mentions.is_empty() {
            return;
        }
        self.insert(&(mentions.join(" ") + " "), window, cx);
        self.focus_composer(window, cx);
    }

    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.contains_focused(window, cx)
    }

    /// Starts the clock while Claude works, stops it when it doesn't.
    fn update_ticker(&mut self, cx: &mut Context<Self>) {
        let working = self.session.read(cx).model().is_working();
        if !working {
            self.ticker = None;
            return;
        }
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |chat, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let working = chat.update(cx, |chat, cx| {
                    cx.notify();
                    chat.session.read(cx).model().is_working()
                });
                if !matches!(working, Ok(true)) {
                    break;
                }
            }
        }));
    }
}

/// What the cards need of the session: everything but the conversation. Copying a long
/// conversation (tool results hold whole files) on every frame would be too slow.
fn without_entries(model: &Session) -> Session {
    let mut light = Session::new(model.info.cwd.clone());
    light.info = model.info.clone();
    light.status = model.status.clone();
    light.pending = model.pending.clone();
    light.tasks = model.tasks.clone();
    light.background = model.background.clone();
    light.limits = model.limits.clone();
    light.context = model.context;
    light.cost_usd = model.cost_usd;
    light
}

impl Focusable for ClaudeChat {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }
}

impl Render for ClaudeChat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        if std::mem::take(&mut self.start_in_sight) {
            let session = self.session.clone();
            cx.defer(move |cx| session.update(cx, |session, cx| session.ensure_started(cx)));
        }
        let loading = {
            let session = self.session.read(cx);
            session.is_loading() && session.model().entries.is_empty()
        };
        claude_transcript::sync(&mut self.transcript, &self.session, cx);
        let transcript = if loading {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.dim)
                .child(tr("Loading…"))
                .into_any_element()
        } else {
            claude_transcript::render(self, window, cx)
        };
        let session = self.session.clone();
        let light = without_entries(session.read(cx).model());
        let cards = claude_cards::render(&mut self.cards, &session, &light, window, cx);
        // Files dragged from the tree or a tab, or from the Finder onto the conversation: mentions
        // in the message (the field takes the Finder's itself, pictures as pictures).
        let drop_style = move |style: gpui::StyleRefinement| style.bg(ui.drop_target);
        div()
            .key_context("ClaudeChat")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .text_color(ui.foreground)
            .drag_over::<crate::file_tree::DraggedEntry>(move |style, _, _, _| drop_style(style))
            .drag_over::<crate::workspace::DraggedEditorTab>(move |style, _, _, _| drop_style(style))
            .drag_over::<gpui::ExternalPaths>(move |style, _, _, _| drop_style(style))
            .on_drop(cx.listener(|this, dragged: &crate::file_tree::DraggedEntry, window, cx| {
                let path = dragged.path.clone();
                this.drop_mentions(vec![path], window, cx)
            }))
            .on_drop(cx.listener(|this, dragged: &crate::workspace::DraggedEditorTab, window, cx| {
                let path = dragged.editor.read(cx).document.path().map(|path| path.to_path_buf());
                this.drop_mentions(path.into_iter().collect(), window, cx)
            }))
            .on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                this.composer
                    .update(cx, |composer, cx| composer.drop_paths(paths, window, cx))
            }))
            .child(transcript)
            .children(cards)
            .child(self.composer.clone())
    }
}
