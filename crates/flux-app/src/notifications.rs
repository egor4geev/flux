//! Notifications: cards in the bottom right corner of the window, above the status bar — the
//! balloons of JetBrains IDEs. An operation's result ("Checked out feature/x"), with actions as
//! links ("Restore", "Resolve…", "Details"): an action is a gpui action dispatched on the window,
//! so whoever handles it (the workspace, mostly) needs no reference to the card.
//!
//! The cards show what the window's [`NotificationCenter`](crate::notification_center) decides:
//! every card is a journal entry with the same id. How long a card stays is its [`CardLife`] — a
//! few seconds (longer with actions), or until closed (errors, a task in progress, sticky groups).
//! At most [`MAX_SHOWN`] are shown: a new card pushes the oldest out. Closing a card with × or
//! running one of its actions tells the center ([`CardEvent`]): the entry becomes read; a card that
//! times out leaves it unread.

use std::time::Duration;

use gpui::{
    Action, App, ClickEvent, Context, Entity, EventEmitter, FontWeight, Hsla, IntoElement, Render,
    SharedString, Task, Window, div, prelude::*, px, relative,
};

use crate::icons::{IconName, icon};
use crate::notification_center::{NotificationGroup, NotificationId};
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, GAP, RADIUS_LG, RADIUS_SM, STATUS_BAR_HEIGHT};

/// Width of a card.
pub const WIDTH: f32 = 360.;
/// How many cards are shown at most.
const MAX_SHOWN: usize = 4;
/// How long a card without actions stays, and one with actions.
pub const SHORT: Duration = Duration::from_secs(6);
pub const LONG: Duration = Duration::from_secs(14);
/// Height of the progress bar of a task in progress.
const PROGRESS_HEIGHT: f32 = 4.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    Info,
    Success,
    Warning,
    Error,
}

#[allow(dead_code)] // The plugin API (stage 8) and the progress of background tasks use it.
/// A background task's progress: unknown ("Indexing…") or a fraction of the work done.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Progress {
    Indeterminate,
    /// 0.0 to 1.0.
    Fraction(f32),
}

/// A notification: a title, an optional body, links that dispatch actions.
pub struct Notification {
    pub kind: NotificationKind,
    pub title: SharedString,
    pub body: Option<SharedString>,
    pub actions: Vec<(SharedString, Box<dyn Action>)>,
    /// Stays until closed (errors are sticky by default; a warning about something the window
    /// shows anyway — conflicts in their dialog — goes away like the rest).
    pub sticky: bool,
    /// Who tells: the group's display setting decides where the notification shows.
    pub group: NotificationGroup,
    /// A task in progress: its card stays until the progress is cleared (`None`).
    pub progress: Option<Progress>,
}

impl Notification {
    pub fn new(kind: NotificationKind, title: impl Into<SharedString>) -> Self {
        Self {
            kind,
            title: title.into(),
            body: None,
            actions: Vec::new(),
            sticky: kind == NotificationKind::Error,
            group: NotificationGroup::General,
            progress: None,
        }
    }

    pub fn info(title: impl Into<SharedString>) -> Self {
        Self::new(NotificationKind::Info, title)
    }

    pub fn success(title: impl Into<SharedString>) -> Self {
        Self::new(NotificationKind::Success, title)
    }

    pub fn warning(title: impl Into<SharedString>) -> Self {
        Self::new(NotificationKind::Warning, title)
    }

    pub fn error(title: impl Into<SharedString>) -> Self {
        Self::new(NotificationKind::Error, title)
    }

    /// A line or two under the title (plain text; it wraps).
    pub fn body(mut self, body: impl Into<SharedString>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// A link at the bottom of the card: dispatches `action` and closes the card.
    pub fn action(mut self, label: impl Into<SharedString>, action: impl Action) -> Self {
        self.actions.push((label.into(), Box::new(action)));
        self
    }

    pub fn group(mut self, group: NotificationGroup) -> Self {
        self.group = group;
        self
    }

    #[allow(dead_code)] // The plugin API (stage 8) and the progress of background tasks use it.
    /// A task in progress (see [`Progress`]).
    pub fn progress(mut self, progress: Progress) -> Self {
        self.progress = Some(progress);
        self
    }

    /// Goes away by itself.
    pub fn transient(mut self) -> Self {
        self.sticky = false;
        self
    }

    /// Whether one of the card's links runs an action named so ("git::ResolveConflicts").
    pub fn offers(&self, names: &[&str]) -> bool {
        self.actions
            .iter()
            .any(|(_, action)| names.contains(&action.name()))
    }
}

impl Clone for Notification {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind,
            title: self.title.clone(),
            body: self.body.clone(),
            actions: self
                .actions
                .iter()
                .map(|(label, action)| (label.clone(), action.boxed_clone()))
                .collect(),
            sticky: self.sticky,
            group: self.group.clone(),
            progress: self.progress,
        }
    }
}

impl std::fmt::Debug for Notification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notification")
            .field("kind", &self.kind)
            .field("title", &self.title)
            .field("body", &self.body)
            .field("group", &self.group)
            .field("progress", &self.progress)
            .field(
                "actions",
                &self
                    .actions
                    .iter()
                    .map(|(label, action)| (label, action.name()))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// How long a card stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardLife {
    /// Closes by itself after this long.
    Timed(Duration),
    /// Stays until closed (or until its notification changes its life).
    Sticky,
}

/// What the user did to a card; the center marks the entry read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardEvent {
    /// Closed with ×.
    Closed(NotificationId),
    /// One of its actions was run (the card closed).
    ActionRun(NotificationId),
}

struct Shown {
    id: NotificationId,
    notification: Notification,
    /// The group's name over the title (a plugin's cards say whose they are).
    source: Option<SharedString>,
    life: CardLife,
    _timer: Option<Task<()>>,
}

/// The cards of a window, newest at the bottom.
#[derive(Default)]
pub struct Notifications {
    shown: Vec<Shown>,
}

impl EventEmitter<CardEvent> for Notifications {}

impl Notifications {
    pub fn new() -> Self {
        Self::default()
    }

    /// Shows the card of journal entry `id`, or replaces it in place if it is shown.
    pub fn show(
        &mut self,
        id: NotificationId,
        notification: Notification,
        source: Option<SharedString>,
        life: CardLife,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.shown.iter().position(|shown| shown.id == id) {
            let restart = self.shown[index].life != life;
            let shown = &mut self.shown[index];
            shown.notification = notification;
            shown.source = source;
            if restart {
                shown.life = life;
                shown._timer = Self::timer(id, life, cx);
            }
        } else {
            self.shown.push(Shown {
                id,
                notification,
                source,
                life,
                _timer: Self::timer(id, life, cx),
            });
            if self.shown.len() > MAX_SHOWN {
                self.shown.remove(0);
            }
        }
        cx.notify();
    }

    fn timer(id: NotificationId, life: CardLife, cx: &mut Context<Self>) -> Option<Task<()>> {
        let CardLife::Timed(delay) = life else {
            return None;
        };
        Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |this, cx| this.dismiss(id, cx)).ok();
        }))
    }

    /// Whether the card of entry `id` is on screen.
    pub fn is_shown(&self, id: NotificationId) -> bool {
        self.shown.iter().any(|shown| shown.id == id)
    }

    /// Closes the card of entry `id` (the entry stays in the journal).
    pub fn dismiss(&mut self, id: NotificationId, cx: &mut Context<Self>) {
        let before = self.shown.len();
        self.shown.retain(|shown| shown.id != id);
        if self.shown.len() != before {
            cx.notify();
        }
    }

    /// Keeps only the cards `keep` says yes to (stale ones go: "Continue Rebase" after the rebase
    /// is over).
    pub fn retain(&mut self, keep: impl Fn(&Notification) -> bool, cx: &mut Context<Self>) {
        let before = self.shown.len();
        self.shown.retain(|shown| keep(&shown.notification));
        if self.shown.len() != before {
            cx.notify();
        }
    }

    /// Closes every card (Do Not Disturb turned on).
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if !self.shown.is_empty() {
            self.shown.clear();
            cx.notify();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.shown.is_empty()
    }

    fn render_card(&self, shown: &Shown, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let notification = &shown.notification;
        let id = shown.id;
        let (glyph, color) = kind_icon(notification.kind, ui);
        let actions: Vec<_> = notification
            .actions
            .iter()
            .enumerate()
            .map(|(index, (label, action))| {
                let action = action.boxed_clone();
                div()
                    .id(("notification-action", id as usize * 16 + index))
                    .px_1()
                    .py_0p5()
                    .rounded(px(RADIUS_SM))
                    .cursor_pointer()
                    .text_color(ui.accent_text)
                    .hover(move |style| style.bg(ui.hover))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.dismiss(id, cx);
                        cx.emit(CardEvent::ActionRun(id));
                        window.dispatch_action(action.boxed_clone(), cx);
                    }))
                    .child(label.clone())
            })
            .collect();
        let group = format!("notification-{id}");
        div()
            .id(("notification", id as usize))
            .group(group.clone())
            .relative()
            .w(px(WIDTH))
            .p_3()
            .flex()
            .gap_2p5()
            .rounded(px(RADIUS_LG))
            .bg(ui.elevated)
            .border_1()
            .border_color(ui.elevated_border)
            .shadow(ui::popover_shadow(ui))
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_MD))
            .text_color(ui.foreground)
            // Clicks on a card stay on it (the editor under it doesn't take focus).
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(ui::sheen(ui, RADIUS_LG))
            .child(
                div()
                    .flex_none()
                    .pt(px(1.))
                    .child(icon(glyph, color).size(px(16.))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(shown.source.clone().map(|source| {
                        div()
                            .pr_5()
                            .text_size(px(theme::TEXT_XS))
                            .text_color(ui.dim)
                            .truncate()
                            .child(source)
                    }))
                    .child(
                        div()
                            .pr_5()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(notification.title.clone()),
                    )
                    .children(notification.body.clone().map(|body| {
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.text_muted)
                            .child(body)
                    }))
                    .children(notification.progress.map(|progress| progress_bar(progress, ui)))
                    .when(!actions.is_empty(), |column| {
                        column.child(
                            div()
                                .pt_1()
                                .flex()
                                .flex_wrap()
                                .gap_2()
                                .ml(px(-4.))
                                .text_size(px(theme::TEXT_SM))
                                .children(actions),
                        )
                    }),
            )
            .child(
                div()
                    .absolute()
                    .top(px(6.))
                    .right(px(6.))
                    .opacity(0.)
                    .group_hover(group, |style| style.opacity(1.))
                    .child(
                        ui::icon_button(("notification-close", id as usize), IconName::Close, ui)
                            .on_click(
                                cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.dismiss(id, cx);
                                    cx.emit(CardEvent::Closed(id));
                                }),
                            ),
                    ),
            )
    }
}

/// A thin bar: the done part in the accent color; an unknown amount is the whole bar, dimmer (no
/// animation — the design system has none).
pub fn progress_bar(progress: Progress, ui: UiColors) -> impl IntoElement + use<> {
    let track = div()
        .mt_1()
        .w_full()
        .h(px(PROGRESS_HEIGHT))
        .rounded(px(PROGRESS_HEIGHT / 2.))
        .overflow_hidden()
        .bg(ui.input_border);
    match progress {
        Progress::Indeterminate => track.child(
            div()
                .size_full()
                .rounded(px(PROGRESS_HEIGHT / 2.))
                .bg(UiColors::tint(ui.accent, 0.45)),
        ),
        Progress::Fraction(done) => track.child(
            div()
                .h_full()
                .w(relative(done.clamp(0., 1.)))
                .rounded(px(PROGRESS_HEIGHT / 2.))
                .bg(ui.accent),
        ),
    }
}

pub fn kind_icon(kind: NotificationKind, ui: UiColors) -> (IconName, Hsla) {
    match kind {
        NotificationKind::Info => (IconName::Info, ui.info),
        NotificationKind::Success => (IconName::CheckCircle, ui.success),
        NotificationKind::Warning => (IconName::Warning, ui.warning),
        NotificationKind::Error => (IconName::Error, ui.error),
    }
}

impl Render for Notifications {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cards: Vec<_> = (0..self.shown.len())
            .map(|index| {
                let shown = &self.shown[index];
                self.render_card(shown, cx).into_any_element()
            })
            .collect();
        div().flex().flex_col().gap_2().children(cards)
    }
}

/// The cards over the window: in the bottom right corner, above the status bar.
pub fn overlay(
    notifications: &Entity<Notifications>,
    cx: &App,
) -> Option<impl IntoElement + use<>> {
    if notifications.read(cx).is_empty() {
        return None;
    }
    Some(
        div()
            .absolute()
            .bottom(px(STATUS_BAR_HEIGHT + GAP))
            .right(px(GAP + 4.))
            .child(notifications.clone()),
    )
}
