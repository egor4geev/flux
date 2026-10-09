//! The notification center of a window — the Notifications tool window of JetBrains IDEs. Every
//! event worth telling (a git operation's result, an error with git's output, a language server
//! installed, a file changed on disk) is one [`Notification`] sent to [`NotificationCenter::notify`]:
//! it goes to the journal (the Notifications window on the right) and, as its group's display
//! setting says, to a card in the corner. The journal lives as long as the window (not kept between
//! launches, as in JetBrains).
//!
//! A source is a [`NotificationGroup`]; a plugin (stage 8) is one more source,
//! `NotificationGroup::Plugin(id)`: it registers its group ([`register_group`]) with a title and a
//! default display, which then shows in Settings → Notifications; it can update its notifications
//! by id (a task's [`Progress`]), expire and remove them. Its actions are gpui actions (the plugin's
//! commands); questions go through [`crate::dialog`].
//!
//! Display (as in JetBrains): per group, a card that goes away by itself, a sticky card, the
//! journal only, or nothing; Do Not Disturb turns the cards off. Whether and how long a card shows
//! is [`card_life`].

use std::time::SystemTime;

use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global, SharedString};

use crate::i18n::tr;
use crate::notifications::{
    CardEvent, CardLife, LONG, Notification, NotificationKind, Notifications, Progress, SHORT,
};
use crate::settings::{self, Settings};

/// Who tells: the display of notifications is set per group (Settings → Notifications).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum NotificationGroup {
    #[default]
    General,
    Git,
    LanguageServers,
    Terminal,
    Files,
    Editor,
    /// The plugins themselves: one stopped, a plugin under development rebuilt or reloaded.
    Plugins,
    /// Claude Code: it waits for the user, finished, failed (stage 9).
    Claude,
    /// A plugin, by its id.
    Plugin(SharedString),
}

impl NotificationGroup {
    /// The built-in groups, in the order of the settings page.
    pub const BUILT_IN: [NotificationGroup; 8] = [
        NotificationGroup::General,
        NotificationGroup::Git,
        NotificationGroup::LanguageServers,
        NotificationGroup::Terminal,
        NotificationGroup::Files,
        NotificationGroup::Editor,
        NotificationGroup::Plugins,
        NotificationGroup::Claude,
    ];

    /// The key in `settings.json` ("git", "plugin:claude").
    pub fn key(&self) -> SharedString {
        match self {
            NotificationGroup::General => "general".into(),
            NotificationGroup::Git => "git".into(),
            NotificationGroup::LanguageServers => "language-servers".into(),
            NotificationGroup::Terminal => "terminal".into(),
            NotificationGroup::Files => "files".into(),
            NotificationGroup::Editor => "editor".into(),
            NotificationGroup::Plugins => "plugins".into(),
            NotificationGroup::Claude => "claude".into(),
            NotificationGroup::Plugin(id) => format!("plugin:{id}").into(),
        }
    }

    /// The group's name; a plugin's is the title it registered ([`register_group`]), else its id.
    pub fn title_in(&self, cx: &App) -> SharedString {
        match self {
            NotificationGroup::Plugin(_) => cx
                .try_global::<PluginGroups>()
                .and_then(|groups| groups.0.iter().find(|info| &info.group == self))
                .map_or_else(|| self.title(), |info| info.title.clone()),
            _ => self.title(),
        }
    }

    pub fn title(&self) -> SharedString {
        match self {
            NotificationGroup::General => tr("General").into(),
            NotificationGroup::Git => tr("Git").into(),
            NotificationGroup::LanguageServers => tr("Language Servers").into(),
            NotificationGroup::Terminal => tr("Terminal").into(),
            NotificationGroup::Files => tr("Files").into(),
            NotificationGroup::Editor => tr("Editor").into(),
            NotificationGroup::Plugins => tr("Plugins").into(),
            NotificationGroup::Claude => tr("Claude Code").into(),
            NotificationGroup::Plugin(id) => id.clone(),
        }
    }
}

/// How a group's notifications are shown (as in JetBrains: Balloon / Sticky balloon / No popup).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Display {
    /// A card that goes away by itself (errors stay).
    #[default]
    Balloon,
    /// A card that stays until closed.
    StickyBalloon,
    /// Only the journal.
    LogOnly,
    /// Nowhere: not even the journal.
    Hidden,
}

impl Display {
    pub const ALL: [Display; 4] = [
        Display::Balloon,
        Display::StickyBalloon,
        Display::LogOnly,
        Display::Hidden,
    ];

    /// The value in `settings.json`.
    pub fn key(self) -> &'static str {
        match self {
            Display::Balloon => "balloon",
            Display::StickyBalloon => "sticky",
            Display::LogOnly => "log",
            Display::Hidden => "hidden",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|display| display.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            Display::Balloon => tr("Balloon"),
            Display::StickyBalloon => tr("Sticky balloon"),
            Display::LogOnly => tr("Log only"),
            Display::Hidden => tr("Don't show"),
        }
    }
}

/// A group as Settings list it: its title and the display it has until the user picks one.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupInfo {
    pub group: NotificationGroup,
    pub title: SharedString,
    pub default_display: Display,
}

/// The groups plugins registered.
#[derive(Default)]
struct PluginGroups(Vec<GroupInfo>);

impl Global for PluginGroups {}

/// Registers a plugin's group (again — replaces it): its notifications get a title, a default
/// display, and a row in Settings → Notifications.
pub fn register_group(info: GroupInfo, cx: &mut App) {
    let groups = &mut cx.default_global::<PluginGroups>().0;
    groups.retain(|known| known.group != info.group);
    groups.push(info);
}

/// Every group, in the order of the settings page: the built-in ones, then the plugins'.
pub fn groups(cx: &App) -> Vec<GroupInfo> {
    let built_in = NotificationGroup::BUILT_IN.into_iter().map(|group| GroupInfo {
        title: group.title(),
        default_display: Display::default(),
        group,
    });
    let plugins = cx
        .try_global::<PluginGroups>()
        .map(|groups| groups.0.clone())
        .unwrap_or_default();
    built_in.chain(plugins).collect()
}

/// How `group`'s notifications are shown: the user's choice, else the group's default.
pub fn display_of(group: &NotificationGroup, cx: &App) -> Display {
    settings::notification_display(&group.key(), cx).unwrap_or_else(|| default_display(group, cx))
}

fn default_display(group: &NotificationGroup, cx: &App) -> Display {
    match group {
        NotificationGroup::Plugin(_) => cx
            .try_global::<PluginGroups>()
            .and_then(|groups| groups.0.iter().find(|info| &info.group == group))
            .map_or(Display::default(), |info| info.default_display),
        _ => Display::default(),
    }
}

/// Whether `notification` gets a card, and for how long: none in Do Not Disturb or for a group
/// shown only in the journal; a sticky group's, an error's (unless made transient) and a running
/// task's stay; the rest go after a few seconds — longer when they offer actions.
pub fn card_life(notification: &Notification, display: Display, do_not_disturb: bool) -> Option<CardLife> {
    if do_not_disturb {
        return None;
    }
    match display {
        Display::LogOnly | Display::Hidden => None,
        Display::StickyBalloon => Some(CardLife::Sticky),
        Display::Balloon if notification.sticky || notification.progress.is_some() => {
            Some(CardLife::Sticky)
        }
        Display::Balloon if notification.actions.is_empty() => Some(CardLife::Timed(SHORT)),
        Display::Balloon => Some(CardLife::Timed(LONG)),
    }
}

pub type NotificationId = u64;

/// A notification in the journal.
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: NotificationId,
    pub at: SystemTime,
    pub notification: Notification,
    pub read: bool,
    /// Its actions no longer apply ("Continue Rebase" after the rebase is over): shown disabled.
    pub expired: bool,
}

pub enum NotificationCenterEvent {
    /// A notification was added, changed, read, expired or removed.
    Changed,
}

/// How many entries are unread.
pub fn unread(entries: &[Entry]) -> usize {
    entries.iter().filter(|entry| !entry.read).count()
}

/// The most severe kind among the unread entries (the counter's color: an unread error is red).
pub fn unread_kind(entries: &[Entry]) -> Option<NotificationKind> {
    entries
        .iter()
        .filter(|entry| !entry.read)
        .map(|entry| entry.notification.kind)
        .max_by_key(|kind| severity(*kind))
}

fn severity(kind: NotificationKind) -> u8 {
    match kind {
        NotificationKind::Info => 0,
        NotificationKind::Success => 1,
        NotificationKind::Warning => 2,
        NotificationKind::Error => 3,
    }
}

/// The journal and the cards of a window.
pub struct NotificationCenter {
    entries: Vec<Entry>,
    cards: Entity<Notifications>,
    next_id: NotificationId,
    /// Do Not Disturb as last seen: turning it on closes the cards.
    do_not_disturb: bool,
    /// The Notifications window is shown: as in JetBrains, no cards while the journal is in sight.
    journal_in_sight: bool,
}

impl EventEmitter<NotificationCenterEvent> for NotificationCenter {}

impl NotificationCenter {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let cards = cx.new(|_| Notifications::new());
        cx.observe(&cards, |_, _, cx| cx.notify()).detach();
        // Closing a card or running its action: the user has seen it.
        cx.subscribe(&cards, |this, _, event, cx| match *event {
            CardEvent::Closed(id) | CardEvent::ActionRun(id) => this.mark_read(id, cx),
        })
        .detach();
        // Do Not Disturb or a group's display changed in Settings: the cards follow.
        cx.observe_global::<Settings>(|this, cx| this.settings_changed(cx))
            .detach();
        Self {
            entries: Vec::new(),
            cards,
            next_id: 0,
            do_not_disturb: settings::do_not_disturb(cx),
            journal_in_sight: false,
        }
    }

    /// The Notifications window was shown or hidden; showing it closes the cards.
    pub fn set_journal_in_sight(&mut self, in_sight: bool, cx: &mut Context<Self>) {
        self.journal_in_sight = in_sight;
        if in_sight {
            self.cards.update(cx, |cards, cx| cards.retain(|_| false, cx));
        }
    }

    /// The cards in the corner of the window.
    pub fn cards(&self) -> &Entity<Notifications> {
        &self.cards
    }

    /// Records `notification` in the journal and shows it as its group's display says. The id is
    /// for later changes; a notification of a hidden group is not recorded (changes to it do
    /// nothing).
    pub fn notify(&mut self, notification: Notification, cx: &mut Context<Self>) -> NotificationId {
        let id = self.next_id;
        self.next_id += 1;
        if display_of(&notification.group, cx) == Display::Hidden {
            return id;
        }
        self.entries.push(Entry {
            id,
            at: SystemTime::now(),
            notification,
            read: false,
            expired: false,
        });
        self.show_card(id, cx);
        self.changed(cx);
        id
    }

    /// Shows, updates or closes the card of entry `id` as its notification and group say now.
    fn show_card(&mut self, id: NotificationId, cx: &mut Context<Self>) {
        let Some(entry) = self.entry(id) else {
            return self.cards.update(cx, |cards, cx| cards.dismiss(id, cx));
        };
        let notification = entry.notification.clone();
        let display = display_of(&notification.group, cx);
        let life = card_life(&notification, display, settings::do_not_disturb(cx))
            .filter(|_| !entry.expired && !self.journal_in_sight);
        let source = matches!(notification.group, NotificationGroup::Plugin(_))
            .then(|| notification.group.title_in(cx));
        self.cards.update(cx, |cards, cx| match life {
            Some(life) => cards.show(id, notification, source, life, cx),
            None => cards.dismiss(id, cx),
        });
    }

    /// Changes a notification in place (a plugin's progress, a new body); a card on screen shows
    /// the change. A card that was closed doesn't come back, unless a task is done (its progress
    /// cleared): the result is news.
    pub fn update(
        &mut self,
        id: NotificationId,
        change: impl FnOnce(&mut Notification),
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) else {
            return;
        };
        let was_running = entry.notification.progress.is_some();
        change(&mut entry.notification);
        let finished = was_running && entry.notification.progress.is_none();
        if finished {
            entry.read = false;
        }
        if finished || self.cards.read(cx).is_shown(id) {
            self.show_card(id, cx);
        }
        self.changed(cx);
    }

    /// Sets or clears (`None` — the task is done) a notification's progress.
    pub fn set_progress(
        &mut self,
        id: NotificationId,
        progress: Option<Progress>,
        cx: &mut Context<Self>,
    ) {
        self.update(id, |notification| notification.progress = progress, cx);
    }

    /// The notification's actions no longer apply: its card closes, the journal shows it without
    /// them.
    pub fn expire(&mut self, id: NotificationId, cx: &mut Context<Self>) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id)
            && !entry.expired
        {
            entry.expired = true;
            self.cards.update(cx, |cards, cx| cards.dismiss(id, cx));
            self.changed(cx);
        }
    }

    /// Expires the notifications `stale` says yes to (cards offering "Continue Rebase" after the
    /// rebase is over).
    pub fn expire_where(
        &mut self,
        stale: impl Fn(&Notification) -> bool,
        cx: &mut Context<Self>,
    ) {
        let mut any = false;
        for entry in &mut self.entries {
            if !entry.expired && stale(&entry.notification) {
                entry.expired = true;
                any = true;
            }
        }
        self.cards
            .update(cx, |cards, cx| cards.retain(|card| !stale(card), cx));
        if any {
            self.changed(cx);
        }
    }

    /// Oldest first.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn entry(&self, id: NotificationId) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    pub fn unread_count(&self) -> usize {
        unread(&self.entries)
    }

    /// The most severe kind among the unread notifications.
    pub fn unread_kind(&self) -> Option<NotificationKind> {
        unread_kind(&self.entries)
    }

    pub fn mark_read(&mut self, id: NotificationId, cx: &mut Context<Self>) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id)
            && !entry.read
        {
            entry.read = true;
            self.changed(cx);
        }
    }

    pub fn mark_all_read(&mut self, cx: &mut Context<Self>) {
        if self.entries.iter().all(|entry| entry.read) {
            return;
        }
        for entry in &mut self.entries {
            entry.read = true;
        }
        self.changed(cx);
    }

    /// Removes a notification from the journal (and its card).
    pub fn remove(&mut self, id: NotificationId, cx: &mut Context<Self>) {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.id != id);
        self.cards.update(cx, |cards, cx| cards.dismiss(id, cx));
        if self.entries.len() != before {
            self.changed(cx);
        }
    }

    /// Empties the journal (and closes the cards).
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.entries.clear();
        self.cards.update(cx, |cards, cx| cards.clear(cx));
        self.changed(cx);
    }

    /// Settings changed: Do Not Disturb turned on closes every card; a group switched to the
    /// journal only (or hidden) loses its cards.
    fn settings_changed(&mut self, cx: &mut Context<Self>) {
        let do_not_disturb = settings::do_not_disturb(cx);
        if do_not_disturb != self.do_not_disturb {
            self.do_not_disturb = do_not_disturb;
            if do_not_disturb {
                self.cards.update(cx, |cards, cx| cards.clear(cx));
            }
        }
        let hidden: Vec<NotificationGroup> = groups(cx)
            .into_iter()
            .filter(|info| matches!(display_of(&info.group, cx), Display::LogOnly | Display::Hidden))
            .map(|info| info.group)
            .collect();
        if !hidden.is_empty() {
            self.cards
                .update(cx, |cards, cx| cards.retain(|card| !hidden.contains(&card.group), cx));
        }
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(NotificationCenterEvent::Changed);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_keys_round_trip() {
        for display in Display::ALL {
            assert_eq!(Display::from_key(display.key()), Some(display));
        }
        assert_eq!(Display::from_key("loud"), None);
    }

    #[test]
    fn group_keys_are_stable() {
        assert_eq!(NotificationGroup::LanguageServers.key().as_ref(), "language-servers");
        assert_eq!(
            NotificationGroup::Plugin("claude".into()).key().as_ref(),
            "plugin:claude"
        );
    }

    #[test]
    fn cards_follow_the_display_and_do_not_disturb() {
        let info = Notification::info("Pushed");
        let with_action = Notification::info("Pushed").action("Undo", gpui::NoAction);
        let error = Notification::error("Push failed");
        assert_eq!(card_life(&info, Display::Balloon, false), Some(CardLife::Timed(SHORT)));
        assert_eq!(card_life(&with_action, Display::Balloon, false), Some(CardLife::Timed(LONG)));
        assert_eq!(card_life(&error, Display::Balloon, false), Some(CardLife::Sticky));
        assert_eq!(
            card_life(&error.clone().transient(), Display::Balloon, false),
            Some(CardLife::Timed(SHORT))
        );
        assert_eq!(card_life(&info, Display::StickyBalloon, false), Some(CardLife::Sticky));
        assert_eq!(card_life(&error, Display::LogOnly, false), None);
        assert_eq!(card_life(&error, Display::Hidden, false), None);
        assert_eq!(card_life(&error, Display::StickyBalloon, true), None);
    }

    #[test]
    fn a_running_task_keeps_its_card() {
        let task = Notification::info("Installing pyright").progress(Progress::Fraction(0.3));
        assert_eq!(card_life(&task, Display::Balloon, false), Some(CardLife::Sticky));
        let task = task.transient().progress(Progress::Indeterminate);
        assert_eq!(card_life(&task, Display::Balloon, false), Some(CardLife::Sticky));
    }

    #[test]
    fn unread_counts_and_the_worst_kind() {
        let entry = |id, notification: Notification, read| Entry {
            id,
            at: SystemTime::UNIX_EPOCH,
            notification,
            read,
            expired: false,
        };
        let entries = vec![
            entry(0, Notification::error("Push failed"), true),
            entry(1, Notification::warning("Conflicts"), false),
            entry(2, Notification::success("Pushed"), false),
        ];
        assert_eq!(unread(&entries), 2);
        assert_eq!(unread_kind(&entries), Some(NotificationKind::Warning));
        assert_eq!(unread_kind(&entries[..1]), None);
    }
}
