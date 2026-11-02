//! The Notifications tool window — the journal of the window's notification center, in the island
//! on the right (as the Notifications tool window of JetBrains IDEs): newest first, with the time,
//! the kind, the group, the body and the actions (disabled once expired). A filter by group, "Mark
//! All as Read", "Clear All", a way to the settings.
//!
//! Read state follows JetBrains: what arrives while the window is shown is read at once (the
//! counters on the launchpad and the status bar stay at zero), but stays highlighted as new until
//! the window is hidden — so opening the window shows what was missed.
//!
//! Keys (context "NotificationsPanel"): ↑/↓ select, ↵ runs the first action, ⌫ removes, Esc returns
//! to the editor, ⇧Esc hides the window. No shortcut opens it (as in JetBrains): the launchpad, the
//! bell in the status bar, the command palette.
//!
//! Motion: an entry that arrives while the window is shown fades in and drops into place
//! ([`arrival`]); a removed one fades out in its place, taking no input, while the gap it leaves
//! closes ([`Removed`]; Clear All — all of them together); another filter's list fades in. The
//! selection's color fades as the window gains or loses focus; the close button (over the time,
//! which fades out), a row's hover and the resize handle fade in under the pointer.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use gpui::{
    Action, AnyElement, App, ClickEvent, Context, CursorStyle, DismissEvent, DragMoveEvent, Entity,
    FocusHandle, Focusable, FontWeight, KeyBinding, MouseButton, Pixels, Point, Render,
    ScrollHandle, SharedString, Subscription, Window, actions, div, prelude::*, px,
};

use crate::context_menu::ContextMenu;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::motion::{self, Leaving, TransitionExt};
use crate::notification_center::{
    Entry, NotificationCenter, NotificationCenterEvent, NotificationGroup, NotificationId,
};
use crate::notifications::kind_icon;
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, GAP, RADIUS_MD, RADIUS_SM};

actions!(
    notifications_panel,
    [
        /// Shows or hides the Notifications window (the launchpad, the bell, the palette).
        Toggle,
        /// ⇧Esc: hides the window.
        Hide,
        /// Esc: focus goes back to the editor, the window stays.
        FocusEditor,
        SelectNext,
        SelectPrevious,
        /// ↵: the first action of the selected notification.
        RunAction,
        /// ⌫: removes the selected notification from the journal.
        Remove,
        MarkAllAsRead,
        ClearAll,
        /// The gear: Settings.
        OpenSettings,
    ]
);

/// Shows the notifications of one group, or of all (`None`).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = notifications_panel, no_json)]
pub struct SetFilter {
    pub group: Option<NotificationGroup>,
}

const CONTEXT: &str = "NotificationsPanel";
const HEADER_HEIGHT: f32 = 40.;
/// Inset of the rows from the island's edges, as in the tree.
const ROW_INSET: f32 = 6.;
const RESIZE_HANDLE_WIDTH: f32 = GAP;

pub fn init(cx: &mut App) {
    let context = Some(CONTEXT);
    crate::keymap::bind_keys(
        cx,
        [
            KeyBinding::new("down", SelectNext, context),
            KeyBinding::new("up", SelectPrevious, context),
            KeyBinding::new("enter", RunAction, context),
            KeyBinding::new("backspace", Remove, context),
            KeyBinding::new("delete", Remove, context),
            KeyBinding::new("escape", FocusEditor, context),
            KeyBinding::new("shift-escape", Hide, context),
        ],
    );
}

/// The filter menu and its subscriptions.
struct Menu {
    menu: Entity<ContextMenu>,
    position: Point<Pixels>,
    _subscriptions: [Subscription; 2],
}

pub struct NotificationsPanel {
    center: Entity<NotificationCenter>,
    width: ui::RightIslandWidth,
    focus_handle: FocusHandle,
    /// The window is shown: what arrives is read at once.
    visible: bool,
    /// Unread when the window was shown, or arrived while it is shown: highlighted until it hides.
    fresh: HashSet<NotificationId>,
    selected: Option<NotificationId>,
    filter: Option<NotificationGroup>,
    scroll: ScrollHandle,
    resizing: bool,
    menu: Option<Menu>,
    /// Rows removed from the journal, on their way out.
    leaving: Vec<Leaving<Removed>>,
    /// The list changed as a whole (another filter, the last rows gone): it fades in from then.
    list_in: Option<Instant>,
    _subscription: Subscription,
}

/// A row removed from the journal (⌫, ×, Clear All), kept for its exit as it looked: it fades out
/// in its place while the gap it leaves closes ([`motion::SMOOTH`]) and the rows below glide up.
struct Removed {
    entry: Entry,
    look: Look,
    /// Its height in the list, the gap to close; `None` — it only fades (no rows are left).
    height: Option<Pixels>,
}

/// How a row is drawn.
#[derive(Clone, Copy)]
struct Look {
    selected: bool,
    /// Highlighted as new.
    fresh: bool,
}

impl NotificationsPanel {
    pub fn new(
        center: Entity<NotificationCenter>,
        width: ui::RightIslandWidth,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.subscribe(&center, |this, _, _: &NotificationCenterEvent, cx| {
            if this.visible {
                this.absorb_unread(cx);
            }
            // Removed or filtered out: the selection follows what is left.
            if let Some(id) = this.selected
                && !this.rows(cx).contains(&id)
            {
                this.selected = None;
            }
            cx.notify();
        });
        Self {
            center,
            width,
            focus_handle: cx.focus_handle(),
            visible: false,
            fresh: HashSet::new(),
            selected: None,
            filter: None,
            scroll: ScrollHandle::new(),
            resizing: false,
            menu: None,
            leaving: Vec::new(),
            list_in: None,
            _subscription: subscription,
        }
    }

    /// The workspace shows or hides the window.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.absorb_unread(cx);
            // The newest first: the list opens at the top.
            self.scroll.set_offset(gpui::point(px(0.), px(0.)));
        } else {
            self.fresh.clear();
            self.menu = None;
            self.leaving.clear();
            self.list_in = None;
        }
        cx.notify();
    }

    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.contains_focused(window, cx)
    }

    /// Unread notifications become read, and new for as long as the window is shown.
    fn absorb_unread(&mut self, cx: &mut Context<Self>) {
        let unread: Vec<NotificationId> = self
            .center
            .read(cx)
            .entries()
            .iter()
            .filter(|entry| !entry.read)
            .map(|entry| entry.id)
            .collect();
        if unread.is_empty() {
            return;
        }
        self.fresh.extend(unread);
        self.center
            .update(cx, |center, cx| center.mark_all_read(cx));
    }

    /// The ids shown, newest first.
    fn rows(&self, cx: &App) -> Vec<NotificationId> {
        self.center
            .read(cx)
            .entries()
            .iter()
            .rev()
            .filter(|entry| self.passes(entry))
            .map(|entry| entry.id)
            .collect()
    }

    fn passes(&self, entry: &Entry) -> bool {
        self.filter
            .as_ref()
            .is_none_or(|group| entry.notification.group == *group)
    }

    fn entry(&self, id: NotificationId, cx: &App) -> Option<Entry> {
        self.center
            .read(cx)
            .entries()
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
    }

    fn select(&mut self, id: Option<NotificationId>, cx: &mut Context<Self>) {
        self.selected = id;
        if let Some(id) = id
            && let Some(index) = self.child_index(id, cx)
        {
            self.scroll.scroll_to_item(index);
        }
        cx.notify();
    }

    /// Where row `id` is among the list's children: the removed rows on their way out keep their
    /// places (newest first, as the rest).
    fn child_index(&self, id: NotificationId, cx: &App) -> Option<usize> {
        let index = self.rows(cx).iter().position(|row| *row == id)?;
        let above = self
            .leaving
            .iter()
            .filter(|removed| removed.value.entry.id > id)
            .count();
        Some(index + above)
    }

    fn look(&self, entry: &Entry) -> Look {
        Look {
            selected: self.selected == Some(entry.id),
            fresh: self.fresh.contains(&entry.id) || !entry.read,
        }
    }

    /// Row `id`, about to be removed from the journal, starts its exit — if it is in sight and
    /// animations are on.
    fn start_leaving(&mut self, id: NotificationId, cx: &App) {
        if !self.visible || !motion::enabled() {
            return;
        }
        let Some(entry) = self.entry(id, cx).filter(|entry| self.passes(entry)) else {
            return;
        };
        let height = self
            .child_index(id, cx)
            .and_then(|index| self.scroll.bounds_for_item(index))
            .map(|bounds| bounds.size.height);
        let look = self.look(&entry);
        self.leaving.push(Leaving::new(Removed {
            entry,
            look,
            height,
        }));
    }

    /// Every row in sight starts its exit together (Clear All): nothing is left to close up.
    fn start_leaving_all(&mut self, cx: &App) {
        if !self.visible || !motion::enabled() {
            return;
        }
        let entries: Vec<Entry> = self
            .center
            .read(cx)
            .entries()
            .iter()
            .filter(|entry| self.passes(entry))
            .cloned()
            .collect();
        for entry in entries {
            let look = self.look(&entry);
            self.leaving.push(Leaving::new(Removed {
                entry,
                look,
                height: None,
            }));
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let rows = self.rows(cx);
        if rows.is_empty() {
            return;
        }
        let current = self
            .selected
            .and_then(|id| rows.iter().position(|row| *row == id));
        let index = match current {
            Some(index) => index.saturating_add_signed(delta).min(rows.len() - 1),
            None if delta > 0 => 0,
            None => rows.len() - 1,
        };
        self.select(Some(rows[index]), cx);
    }

    fn run_action(
        &mut self,
        id: NotificationId,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.entry(id, cx) else {
            return;
        };
        if entry.expired {
            return;
        }
        let Some((_, action)) = entry.notification.actions.get(index) else {
            return;
        };
        self.center
            .update(cx, |center, cx| center.mark_read(id, cx));
        window.dispatch_action(action.boxed_clone(), cx);
    }

    fn remove(&mut self, id: NotificationId, cx: &mut Context<Self>) {
        self.start_leaving(id, cx);
        // The selection moves to the next row (older), or the previous one at the end.
        if self.selected == Some(id) {
            let rows = self.rows(cx);
            let next = rows.iter().position(|row| *row == id).and_then(|index| {
                rows.get(index + 1)
                    .or_else(|| index.checked_sub(1).and_then(|index| rows.get(index)))
                    .copied()
            });
            self.selected = next;
        }
        self.fresh.remove(&id);
        self.center.update(cx, |center, cx| center.remove(id, cx));
    }

    /// The groups present in the journal, built-in ones in their order, then plugins.
    fn groups(&self, cx: &App) -> Vec<NotificationGroup> {
        let center = self.center.read(cx);
        let mut present: Vec<NotificationGroup> = Vec::new();
        for entry in center.entries() {
            if !present.contains(&entry.notification.group) {
                present.push(entry.notification.group.clone());
            }
        }
        let mut groups: Vec<NotificationGroup> = NotificationGroup::BUILT_IN
            .into_iter()
            .filter(|group| present.contains(group))
            .collect();
        groups.extend(
            present
                .into_iter()
                .filter(|group| matches!(group, NotificationGroup::Plugin(_))),
        );
        groups
    }

    fn open_filter_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        let groups = self.groups(cx);
        let current = self.filter.clone();
        let mark = |on: bool, label: SharedString| -> SharedString {
            if on {
                format!("✓  {label}").into()
            } else {
                format!("    {label}").into()
            }
        };
        let menu = cx.new(|cx| {
            let mut menu = ContextMenu::new(window, cx).entry(
                mark(current.is_none(), tr("All Groups").into()),
                SetFilter { group: None },
            );
            if !groups.is_empty() {
                menu = menu.separator();
            }
            for group in groups {
                menu = menu.entry(
                    mark(current.as_ref() == Some(&group), group.title()),
                    SetFilter { group: Some(group) },
                );
            }
            menu
        });
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
        self.menu = Some(Menu {
            menu,
            position,
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
        self.menu = None;
        if had_focus {
            window.focus(&self.focus_handle);
        }
        cx.notify();
    }

    fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let (any, any_unread_fresh) = {
            let center = self.center.read(cx);
            (!center.entries().is_empty(), !self.fresh.is_empty())
        };
        let filter_label: SharedString = match &self.filter {
            Some(group) => group.title(),
            None => tr("All").into(),
        };
        let button =
            |id: &'static str, glyph: IconName, label: &'static str, action: Box<dyn Action>| {
                let keys = None::<SharedString>;
                ui::icon_button(id, glyph, ui)
                    .tooltip(ui::tooltip(label, keys))
                    .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
            };
        div()
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .pl(px(ROW_INSET + 8.))
            .pr(px(ROW_INSET))
            .flex()
            .items_center()
            .gap_0p5()
            .child(ui::section_label(tr("Notifications"), ui).flex_none())
            .child(
                div()
                    .id("notifications-filter")
                    .ml_2()
                    .h(px(22.))
                    .px_1p5()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .rounded(px(RADIUS_SM))
                    .text_size(px(theme::TEXT_SM))
                    .text_color(if self.filter.is_some() {
                        ui.accent_text
                    } else {
                        ui.text_muted
                    })
                    .cursor_pointer()
                    .hover(move |style| style.bg(ui.hover))
                    .tooltip(ui::tooltip(tr("Filter by Group"), None))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_filter_menu(event.position, window, cx)
                        }),
                    )
                    .child(div().max_w(px(140.)).truncate().child(filter_label))
                    .child(icon(IconName::ChevronDown, ui.dim).size(px(11.))),
            )
            .child(div().flex_1())
            .when(any_unread_fresh || any, |header| {
                header
                    .child(button(
                        "notifications-mark-read",
                        IconName::CheckAll,
                        tr("Mark All as Read"),
                        Box::new(MarkAllAsRead),
                    ))
                    .child(button(
                        "notifications-clear",
                        IconName::Trash,
                        tr("Clear All"),
                        Box::new(ClearAll),
                    ))
            })
            .child(button(
                "notifications-settings",
                IconName::Settings,
                tr("Notification Settings"),
                Box::new(OpenSettings),
            ))
            .child(
                ui::icon_button("notifications-hide", IconName::ChevronRight, ui)
                    .tooltip(ui::tooltip(
                        tr("Hide"),
                        ui::shortcut_in(&Hide, &self.focus_handle, window, cx),
                    ))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(Hide), cx)),
            )
    }

    fn render_entry(
        &self,
        entry: &Entry,
        look: Look,
        now: SystemTime,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let id = entry.id;
        let notification = &entry.notification;
        let Look { selected, fresh } = look;
        let (glyph, color) = kind_icon(notification.kind, ui);
        let group = format!("notification-entry-{id}");
        let title_only = notification.body.is_none() && notification.actions.is_empty();
        let group_title = notification.group.title();
        let group_label = || {
            div()
                .flex_none()
                .pt(px(2.))
                .text_size(px(theme::TEXT_XS))
                .text_color(ui.dim)
                .child(group_title.clone())
        };
        let actions: Vec<AnyElement> = notification
            .actions
            .iter()
            .enumerate()
            .map(|(index, (label, _))| {
                let link = div()
                    .id(("notification-entry-action", id as usize * 16 + index))
                    .px_1()
                    .py_0p5()
                    .rounded(px(RADIUS_SM))
                    .child(label.clone());
                if entry.expired {
                    link.text_color(ui.text_disabled).into_any_element()
                } else {
                    link.cursor_pointer()
                        .text_color(ui.accent_text)
                        .hover(move |style| style.bg(ui.hover))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.run_action(id, index, window, cx)
                        }))
                        .into_any_element()
                }
            })
            .collect();
        let selection = selected.then_some(if focused {
            ui.list_selected
        } else {
            ui.list_selected_inactive
        });
        let tint = (!selected && fresh).then(|| UiColors::tint(ui.accent, 0.07));
        div()
            .id(("notification-entry", id as usize))
            .w_full()
            .px(px(ROW_INSET))
            .py(px(2.))
            .child(
                div()
                    // An id keeps the fade of the hover.
                    .id(("notification-entry-row", id as usize))
                    .group(group.clone())
                    .relative()
                    .w_full()
                    .px_2()
                    .py_2()
                    .flex()
                    .gap_2()
                    .rounded(px(RADIUS_MD))
                    .when_some(tint, |row, color| row.bg(color))
                    .when(!selected && !fresh, |row| {
                        row.hover(move |style| style.bg(ui.hover))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            window.focus(&this.focus_handle);
                            this.select(Some(id), cx);
                        }),
                    )
                    // The selection, under the row: its color fades as the window gains or loses
                    // focus; a row the selection moves to has it at once (the layer is drawn only
                    // under the selected row, so it starts at its color).
                    .when_some(selection, |row, color| {
                        row.child(
                            div()
                                .absolute()
                                .inset_0()
                                .rounded(px(RADIUS_MD))
                                .with_transition(
                                    ("notification-selection", id as usize),
                                    color,
                                    motion::FADE,
                                    |layer, color| layer.bg(color),
                                ),
                        )
                    })
                    .child(
                        div()
                            .flex_none()
                            .pt(px(1.))
                            .child(icon(glyph, color).size(px(15.))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_start()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .when(fresh, |title| {
                                                title.font_weight(FontWeight::SEMIBOLD)
                                            })
                                            .when(!fresh, |title| {
                                                title.font_weight(FontWeight::MEDIUM)
                                            })
                                            .text_color(if entry.expired {
                                                ui.text_muted
                                            } else {
                                                ui.foreground
                                            })
                                            .child(notification.title.clone()),
                                    )
                                    .when(title_only, |row| row.child(group_label()))
                                    // The time; on hover, the close button fades in in its
                                    // place as the time fades out (ids keep the fades).
                                    .child(
                                        div()
                                            .flex_none()
                                            .relative()
                                            .min_w(px(20.))
                                            .h(px(18.))
                                            .flex()
                                            .items_center()
                                            .justify_end()
                                            .child(
                                                div()
                                                    .id("notification-entry-time")
                                                    .flex()
                                                    .items_center()
                                                    .gap(px(6.))
                                                    .text_size(px(theme::TEXT_XS))
                                                    .text_color(ui.dim)
                                                    .group_hover(group.clone(), |style| {
                                                        style.opacity(0.)
                                                    })
                                                    // A dot of a new notification, before the time.
                                                    .when(fresh, |time| {
                                                        time.child(
                                                            div()
                                                                .size(px(6.))
                                                                .rounded(px(3.))
                                                                .bg(ui.accent),
                                                        )
                                                    })
                                                    .child(format_time(entry.at, now)),
                                            )
                                            .child(
                                                div()
                                                    .id("notification-entry-reveal")
                                                    .absolute()
                                                    .top(px(-4.))
                                                    .right(px(-4.))
                                                    .opacity(0.)
                                                    .group_hover(group.clone(), |style| {
                                                        style.opacity(1.)
                                                    })
                                                    .child(
                                                        ui::icon_button(
                                                            (
                                                                "notification-entry-remove",
                                                                id as usize,
                                                            ),
                                                            IconName::Close,
                                                            ui,
                                                        )
                                                        .tooltip(ui::tooltip(tr("Remove"), None))
                                                        .on_click(cx.listener(
                                                            move |this, _: &ClickEvent, _, cx| {
                                                                cx.stop_propagation();
                                                                this.remove(id, cx)
                                                            },
                                                        )),
                                                    ),
                                            ),
                                    ),
                            )
                            // The group goes on the last line: the actions', the body's, or the
                            // title's.
                            .children(notification.body.clone().map(|body| {
                                div()
                                    .flex()
                                    .items_end()
                                    .gap_2()
                                    .text_size(px(theme::TEXT_SM))
                                    .text_color(ui.text_muted)
                                    .child(div().flex_1().min_w_0().child(body))
                                    .when(actions.is_empty(), |line| line.child(group_label()))
                            }))
                            .when(!actions.is_empty(), |column| {
                                column.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .flex_wrap()
                                        .gap_x_2()
                                        .ml(px(-4.))
                                        .text_size(px(theme::TEXT_SM))
                                        .children(actions)
                                        .child(div().flex_1())
                                        .child(group_label()),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_empty(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let filtered = self.filter.is_some();
        div()
            .size_full()
            .px_6()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .text_center()
            .child(icon(IconName::Bell, ui.dim).size(px(28.)))
            .child(div().text_color(ui.text_muted).child(if filtered {
                tr("No notifications in this group")
            } else {
                tr("No notifications")
            }))
            .child(
                div()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(tr(
                        "Results of Git operations, errors and messages of Flux will appear here",
                    )),
            )
    }
}

impl Focusable for NotificationsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for NotificationsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        self.resizing &= cx.has_active_drag();
        let now = SystemTime::now();
        let entries: Vec<Entry> = self
            .center
            .read(cx)
            .entries()
            .iter()
            .rev()
            .filter(|entry| self.passes(entry))
            .cloned()
            .collect();
        // The removed rows: with rows left, until the gap they leave has closed; else until they
        // have faded — then the empty window fades in.
        let close_up = !entries.is_empty();
        let had_leaving = !self.leaving.is_empty();
        self.leaving.retain(|removed| {
            let id = removed.value.entry.id;
            !entries.iter().any(|entry| entry.id == id) && still_leaving(removed, close_up)
        });
        if had_leaving && self.leaving.is_empty() && !close_up {
            self.list_in = Some(Instant::now());
        }
        if !self.leaving.is_empty() {
            // One frame past the last exit's end drops it, even if that end fell between the
            // check above and the drawing.
            window.request_animation_frame();
        }
        let list_in = self
            .list_in
            .map_or(1., |since| motion::animate(since, motion::FADE, window));
        if list_in >= 1. {
            self.list_in = None;
        }
        let focused = self.focus_handle.contains_focused(window, cx);
        // Newest first; a removed row keeps its place among the rest.
        let mut order: Vec<(NotificationId, Result<&Entry, &Leaving<Removed>>)> = entries
            .iter()
            .map(|entry| (entry.id, Ok(entry)))
            .chain(
                self.leaving
                    .iter()
                    .map(|removed| (removed.value.entry.id, Err(removed))),
            )
            .collect();
        order.sort_by_key(|(id, _)| Reverse(*id));
        let rows: Vec<AnyElement> = order
            .into_iter()
            .map(|(_, row)| match row {
                Ok(entry) => {
                    let row = self.render_entry(entry, self.look(entry), now, focused, cx);
                    let value = arrival(entry.at, window);
                    if value >= 1. {
                        return row;
                    }
                    motion::Appear::slide(0., -ARRIVAL_DROP)
                        .curve(motion::SMOOTH)
                        .style(div().child(row), value)
                        .into_any_element()
                }
                Err(removed) => {
                    let row = self.render_entry(
                        &removed.value.entry,
                        removed.value.look,
                        now,
                        focused,
                        cx,
                    );
                    leaving_row(removed, row, close_up, window)
                }
            })
            .collect();
        let list = if rows.is_empty() {
            self.render_empty(cx).into_any_element()
        } else {
            div()
                .id("notifications-list")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .pt_0p5()
                .pb_2()
                .children(rows)
                .into_any_element()
        };
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .relative()
            .flex_none()
            .w(px(self.width.get()))
            .h_full()
            .flex()
            .flex_col()
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_MD))
            .text_color(ui.foreground)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &RunAction, window, cx| {
                if let Some(id) = this.selected {
                    this.run_action(id, 0, window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &Remove, _, cx| {
                if let Some(id) = this.selected {
                    this.remove(id, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &MarkAllAsRead, _, cx| {
                this.fresh.clear();
                this.center
                    .update(cx, |center, cx| center.mark_all_read(cx));
            }))
            .on_action(cx.listener(|this, _: &ClearAll, _, cx| {
                this.start_leaving_all(cx);
                this.fresh.clear();
                this.selected = None;
                this.center.update(cx, |center, cx| center.clear(cx));
            }))
            .on_action(cx.listener(|this, action: &SetFilter, _, cx| {
                if this.filter != action.group {
                    // Another list: it fades in as a whole; the old one's removed rows go with it.
                    this.leaving.clear();
                    this.list_in = Some(Instant::now());
                }
                this.filter = action.group.clone();
                if let Some(id) = this.selected
                    && !this.rows(cx).contains(&id)
                {
                    this.selected = None;
                }
                this.scroll.set_offset(gpui::point(px(0.), px(0.)));
                cx.notify();
            }))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedEdge>, _, cx| {
                    // The handle sits in the gap to the left of the edge.
                    let width = f32::from(event.bounds.right() - event.event.position.x)
                        - ui::resize_handle_center(ui::style(cx));
                    this.width.set(width);
                    this.resizing = true;
                    cx.notify();
                }),
            )
            .child(self.render_header(window, cx))
            .child(ui::divider(ui).mx(px(GAP)))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when(list_in < 1., |content| content.opacity(list_in))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, _| window.focus(&this.focus_handle)),
                    )
                    .child(list),
            )
            .child(resize_handle(
                self.resizing,
                ui::resize_handle_center(ui::style(cx)),
                ui,
            ))
            .child(ContextMenu::slot(
                "notifications-menu",
                self.menu.as_ref().map(|menu| (&menu.menu, menu.position)),
            ))
    }
}

#[derive(Clone)]
struct DraggedEdge;

impl Render for DraggedEdge {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// Width handle in the gap to the left of the island: an accent line fading in on hover, shown
/// while dragged. Its middle is `center` past the edge (`ui::resize_handle_center`): in the gap
/// between islands, or on the line between Flat panels (part 10.4).
fn resize_handle(resizing: bool, center: f32, ui: UiColors) -> impl IntoElement {
    div()
        .id("notifications-resize")
        .group("notifications-resize")
        .absolute()
        .top_0()
        .bottom_0()
        .left(px(-(center + RESIZE_HANDLE_WIDTH / 2.)))
        .w(px(RESIZE_HANDLE_WIDTH))
        .py(px(ui::RADIUS_LG))
        .flex()
        .justify_center()
        .cursor(CursorStyle::ResizeLeftRight)
        .on_drag(DraggedEdge, |_, _, _, cx| cx.new(|_| DraggedEdge))
        .child(
            div()
                // An id keeps the fade.
                .id("notifications-resize-line")
                .w(px(2.))
                .h_full()
                .rounded(px(1.))
                .bg(ui.focus_border)
                .when(!resizing, |line| {
                    line.opacity(0.)
                        .group_hover("notifications-resize", |style| style.opacity(1.))
                }),
        )
}

/// Whether a removed row is still drawn: until the gap it leaves has closed (`close_up` — rows are
/// left), else until it has faded out.
fn still_leaving(removed: &Leaving<Removed>, close_up: bool) -> bool {
    if close_up && removed.value.height.is_some() {
        motion::running(removed.since, motion::SMOOTH)
    } else {
        !removed.done()
    }
}

/// `row`, removed, on its way out: fading and shrinking a little, taking no input, while its
/// height closes up (`close_up`) and the rows below glide into its place.
fn leaving_row(
    removed: &Leaving<Removed>,
    row: AnyElement,
    close_up: bool,
    window: &Window,
) -> AnyElement {
    let fading = motion::leaving_style(div().child(row), removed.progress(window), (0.5, 0.5));
    let row = match removed.value.height {
        Some(height) if close_up => {
            let open = 1. - motion::animate(removed.since, motion::SMOOTH, window);
            div().h(height * open).overflow_hidden().child(fading)
        }
        _ => div().child(fading),
    };
    motion::inert(row).into_any_element()
}

/// The time of a notification as the journal shows it: "10:42" today, "Yesterday 10:42", else
/// "2026-10-07 10:42" (local time).
/// How far a new entry drops into place as it fades in.
const ARRIVAL_DROP: f32 = 8.;

/// How far the entry that arrived `at` has come in, 0 → 1, asking for the next frame until it has.
/// The journal draws every entry anew on each change, so the entry's own time drives it, not the
/// row: one that came while the window was hidden is simply there.
fn arrival(at: SystemTime, window: &Window) -> f32 {
    let age = SystemTime::now().duration_since(at).unwrap_or_default();
    if age >= motion::SMOOTH.duration() {
        return 1.;
    }
    Instant::now()
        .checked_sub(age)
        .map_or(1., |since| motion::animate(since, motion::SMOOTH, window))
}

fn format_time(at: SystemTime, now: SystemTime) -> String {
    let seconds = |time: SystemTime| {
        time.duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs() as i64)
    };
    let local = |seconds: i64| seconds + crate::git_log::local_offset(seconds);
    time_label(local(seconds(at)), local(seconds(now)))
}

/// [`format_time`] on local seconds since the epoch.
fn time_label(at: i64, now: i64) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_today_yesterday_or_dated() {
        // 2026-10-09 10:42:05 and the same day later.
        let at = 1_791_542_525;
        assert_eq!(time_label(at, at + 3_600), "10:42");
        assert_eq!(time_label(at, at + 86_400), "Yesterday 10:42");
        assert_eq!(time_label(at, at + 3 * 86_400), "2026-10-09 10:42");
    }
}

/// Sample notifications for screenshots of the window (the `scenario` feature):
/// `action:notifications_panel::FillDemo`.
#[cfg(feature = "scenario")]
pub mod demo {
    use gpui::{Context, actions};

    use crate::notification_center::{NotificationCenter, NotificationGroup};
    use crate::notifications::Notification;

    actions!(notifications_panel, [FillDemo]);

    pub fn fill(center: &mut NotificationCenter, cx: &mut Context<NotificationCenter>) {
        let samples = [
            Notification::success("Pushed 2 commits to origin/main").group(NotificationGroup::Git),
            Notification::info("rust-analyzer 2026-10-06 installed")
                .body("Language server for Rust, in ~/Library/Application Support/flux/servers")
                .group(NotificationGroup::LanguageServers),
            Notification::warning("Merge conflicts in 2 files")
                .body("Merging 'feature/x' into 'main' stopped on conflicts.")
                .action("Resolve…", crate::git::Refresh)
                .action("Abort Merge", crate::git::Refresh)
                .group(NotificationGroup::Git),
            Notification::info("src/main.rs changed on disk").group(NotificationGroup::Files),
            Notification::error("Push rejected")
                .body("Updates were rejected because the remote contains work that you do not have locally.")
                .action("Update Project…", crate::git::Refresh)
                .action("Details", crate::git::Refresh)
                .group(NotificationGroup::Git),
        ];
        for notification in samples {
            center.notify(notification, cx);
        }
        // The two oldest have been seen; the stale conflict notification lost its actions.
        let ids: Vec<_> = center.entries().iter().map(|entry| entry.id).collect();
        center.mark_read(ids[0], cx);
        center.mark_read(ids[1], cx);
        center.expire(ids[2], cx);
    }
}
