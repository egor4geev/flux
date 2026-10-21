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

use flux_claude::{Session, Status};

use crate::claude_agent_view::{self, AgentView};
use crate::claude_cards::{self, CardsState};
use crate::claude_composer::Composer;
use crate::claude_search::{self, ChatSearch};
use crate::claude_session::{ClaudeSession, SessionEvent};
use crate::claude_transcript::{self, TranscriptState};
use crate::i18n::tr;
use crate::theme::{self, Theme};

/// How often the line of what Claude does is redrawn while it works (its clock).
const TICK: Duration = Duration::from_secs(1);
/// The redraw comes this long after the clock's second turns, so it shows the new second.
const TICK_SLACK: Duration = Duration::from_millis(15);

/// What the chat asks of the window.
#[derive(Debug, Clone)]
pub enum ClaudeChatEvent {
    /// "Open Diff" on an edit card (the CLI's request id).
    OpenProposal(String),
    /// A file Claude read or changed, clicked in the conversation.
    OpenLocation(crate::workspace::Location),
    /// A picture of the conversation clicked: an image tab (part 10.2).
    PreviewImage(crate::image_view::ImageSource),
    /// A file compared with a text of Claude's edits (part 10.2: a turn's Review, an edit's Diff).
    OpenCompare {
        path: PathBuf,
        left: crate::diff_view::DiffSide,
        right: crate::diff_view::DiffSide,
    },
    /// What came of an action on the conversation (a rollback): the notification center, Claude's
    /// group.
    Notify(crate::notifications::Notification),
}

pub struct ClaudeChat {
    session: Entity<ClaudeSession>,
    pub(crate) transcript: TranscriptState,
    pub(crate) cards: CardsState,
    composer: Entity<Composer>,
    /// The field of a subagent's view (made the first time one is opened): its own draft, the
    /// message goes to Claude for the agent ([`crate::claude_composer::AgentTarget`]).
    pub(crate) agent_composer: Option<Entity<Composer>>,
    pub(crate) focus_handle: FocusHandle,
    /// Subagents in sight, one per level (`claude_agent_view`, part 10.2); empty — the session's
    /// conversation.
    pub(crate) agents: Vec<AgentView>,
    /// The search (⌘F) while its bar is open (`claude_search`, part 10.2).
    pub(crate) search: Option<ChatSearch>,
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
                this.invalidate_transcripts();
                this.update_ticker(cx);
                match event {
                    // A question takes the keyboard, unless the user is typing a message. Not
                    // in a subagent's view: the questions aren't drawn there.
                    SessionEvent::PendingAdded(_)
                        if this.agents.is_empty() && this.composer.read(cx).is_empty(cx) =>
                    {
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
            agent_composer: None,
            focus_handle: cx.focus_handle(),
            agents: Vec::new(),
            search: None,
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
                let wait = chat
                    .read_with(cx, |chat, cx| {
                        next_tick(&chat.session.read(cx).model().status)
                    })
                    .unwrap_or(TICK);
                cx.background_executor().timer(wait).await;
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

/// Until the clock under the conversation turns to its next second (it counts from the moment
/// Claude started working, not from the chat's last redraw).
fn next_tick(status: &Status) -> Duration {
    match status {
        Status::Working { since, .. } => {
            let into_second = Duration::from_nanos(since.elapsed().subsec_nanos().into());
            TICK - into_second + TICK_SLACK
        }
        _ => TICK,
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
        self.refresh_search(cx);
        let session = self.session.clone();
        claude_transcript::sync(self.shown_transcript_mut(), &session, cx);
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
        let search_bar = claude_search::render_bar(self, window, cx);
        // A subagent in sight: its bar on top, its line at the bottom, no cards, its own field.
        let agent_header = claude_agent_view::render_header(self, cx);
        let agent_footer = claude_agent_view::render_footer(self, cx);
        let in_agent = !self.agents.is_empty();
        let field = if in_agent {
            self.sync_agent_composer(window, cx)
        } else {
            Some(self.composer.clone())
        };
        let light = without_entries(session.read(cx).model());
        let cards = if in_agent {
            None
        } else {
            claude_cards::render(&mut self.cards, &session, &light, window, cx)
        };
        // Files dragged from the tree or a tab, or from the Finder onto the conversation: mentions
        // in the message (the field takes the Finder's itself, pictures as pictures).
        let drop_style = move |style: gpui::StyleRefinement| style.bg(ui.drop_target);
        let root = crate::claude_turns::scenario_actions(div(), cx)
            .key_context(if in_agent {
                "ClaudeChat ClaudeAgentView"
            } else {
                "ClaudeChat"
            })
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .text_color(ui.foreground)
            .drag_over::<crate::file_tree::DraggedEntry>(move |style, _, _, _| drop_style(style))
            .drag_over::<crate::workspace::DraggedEditorTab>(move |style, _, _, _| {
                drop_style(style)
            })
            .drag_over::<gpui::ExternalPaths>(move |style, _, _, _| drop_style(style))
            .on_drop(cx.listener(
                |this, dragged: &crate::file_tree::DraggedEntry, window, cx| {
                    let path = dragged.path.clone();
                    this.drop_mentions(vec![path], window, cx)
                },
            ))
            .on_drop(cx.listener(
                |this, dragged: &crate::workspace::DraggedEditorTab, window, cx| {
                    let path = dragged
                        .editor
                        .read(cx)
                        .document
                        .path()
                        .map(|path| path.to_path_buf());
                    this.drop_mentions(path.into_iter().collect(), window, cx)
                },
            ))
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                    this.composer
                        .update(cx, |composer, cx| composer.drop_paths(paths, window, cx))
                }),
            )
            .on_action(
                cx.listener(|this, _: &claude_agent_view::Back, window, cx| {
                    this.agent_back(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &claude_agent_view::OpenLatest, window, cx| {
                    this.open_latest_agent(window, cx)
                }),
            );
        let root = crate::claude_select::actions(root, cx);
        claude_search::actions(root, cx)
            .children(agent_header)
            .children(search_bar)
            .child(transcript)
            // The cards and the field keep to the conversation's column (centered, at most
            // `COLUMN_MAX_WIDTH` wide in an editor tab); the field's own inset is 10 px.
            .children(cards.map(|cards| claude_transcript::column(cards).flex_none()))
            .children(agent_footer)
            .children(field.map(|field| {
                div().flex_none().w_full().flex().justify_center().child(
                    div()
                        .w_full()
                        .max_w(px(claude_transcript::COLUMN_MAX_WIDTH))
                        .px(px(claude_transcript::COLUMN_INSET - 10.))
                        .child(field),
                )
            }))
    }
}
