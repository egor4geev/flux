//! The chat of one Claude session: the conversation, the questions for the user and the task list,
//! the message field. The same view lives in the Claude window (the island on the right) or in an
//! editor tab; the workspace moves it between them.

use std::time::Duration;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, Render, Subscription, Task, Window,
    div, prelude::*,
};

use flux_claude::Session;

use crate::claude_cards::{self, CardsState};
use crate::claude_composer::Composer;
use crate::claude_session::{ClaudeSession, SessionEvent};
use crate::claude_transcript::{self, TranscriptState};
use crate::theme::Theme;

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
        let mut chat = Self {
            session,
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
        claude_transcript::sync(&mut self.transcript, &self.session, cx);
        let transcript = claude_transcript::render(self, window, cx);
        let session = self.session.clone();
        let light = without_entries(session.read(cx).model());
        let cards = claude_cards::render(&mut self.cards, &session, &light, window, cx);
        div()
            .key_context("ClaudeChat")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .text_color(ui.foreground)
            .child(transcript)
            .children(cards)
            .child(self.composer.clone())
    }
}
