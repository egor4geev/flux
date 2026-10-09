//! The Claude window in the island on the right (as a tool window of JetBrains IDEs): the sessions
//! as pills with their status (working, waiting for you, stopped), "+" for a new one, a menu, the
//! chat of the active session. While `claude` isn't ready, the window says what to do about it:
//! install (the command typed into a terminal), sign in, try again.
//!
//! A session's pill can be dragged to the editor's tabs (its chat moves there) and an editor tab
//! of a chat back onto the pills. Keys (context "ClaudePanel"): Esc returns to the editor when the
//! message field doesn't take it, ⇧Esc hides the window.

use std::collections::HashSet;
use std::time::Duration;

use gpui::{
    Action, Animation, AnimationExt, AnyElement, App, ClickEvent, Context, CursorStyle,
    DismissEvent, DragMoveEvent, Entity, EntityId, EventEmitter, FocusHandle, Focusable,
    FontWeight, KeyBinding, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, Render,
    ScrollHandle, SharedString, Subscription, Window, div, prelude::*, px,
};

use flux_claude::Status;

use crate::claude::{self, ClaudeStore, ClaudeStoreEvent, CliState};
use crate::claude_chat::ClaudeChat;
use crate::context_menu::ContextMenu;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::notifications_panel;
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, GAP, RADIUS_MD, RADIUS_SM};

const CONTEXT: &str = "ClaudePanel";

/// Makes the session at an index active (the sessions' list).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = claude_panel, no_json)]
pub struct ActivateSession(pub usize);
/// The same header as the other windows of the island (Notifications).
const HEADER_HEIGHT: f32 = 40.;
const TAB_HEIGHT: f32 = 26.;
/// Long session titles are shortened in the middle; the pills of other sessions more.
const TAB_LABEL_MAX_CHARS: usize = 24;
const INACTIVE_TAB_LABEL_MAX_CHARS: usize = 16;
/// Hover group of a pill: its × shows.
const TAB_GROUP: &str = "claude-tab";
const RESIZE_HANDLE_WIDTH: f32 = GAP;
const RESIZE_HANDLE_OFFSET: f32 = 1. + RESIZE_HANDLE_WIDTH / 2.;
/// The onboarding column's width at most: it stays readable in a wide island.
const ONBOARDING_WIDTH: f32 = 340.;

pub fn init(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("shift-escape", notifications_panel::Hide, context),
        KeyBinding::new("escape", notifications_panel::FocusEditor, context),
    ]);
}

/// What the panel asks of the window.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaudePanelEvent {
    /// A chat dragged from the editor's tabs was dropped on the pills at `index`: the window takes
    /// it out of its tabs and gives it to [`ClaudePanel::add_chat`].
    MoveToPanel {
        chat: Entity<ClaudeChat>,
        index: usize,
    },
    /// × on a pill, its middle click: the window ends the session (asking first if it works).
    CloseChat(Entity<ClaudeChat>),
}

/// A session's pill or editor tab being dragged: also the label next to the pointer.
#[derive(Clone)]
pub struct DraggedClaudeChat {
    pub chat: Entity<ClaudeChat>,
    pub title: SharedString,
}

impl Render for DraggedClaudeChat {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        div()
            .flex()
            .items_center()
            .gap_1p5()
            .h(px(28.))
            .pl_2()
            .pr_2p5()
            .rounded(px(RADIUS_MD))
            // Opaque: the label floats over the tabs, and their text must not show through.
            .bg(UiColors::tint(ui.elevated, 1.))
            .border_1()
            .border_color(ui.elevated_border)
            .shadow(ui::popover_shadow(ui))
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_MD))
            .text_color(ui.foreground)
            .child(icon(IconName::Claude, ui.accent_text).size(px(13.)))
            .child(shorten(&self.title, TAB_LABEL_MAX_CHARS))
    }
}

/// The width handle being dragged.
#[derive(Clone)]
struct DraggedEdge;

impl Render for DraggedEdge {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// A menu of the window (a pill's, the ⋯ one) and its subscriptions.
struct Menu {
    menu: Entity<ContextMenu>,
    position: Point<Pixels>,
    _subscriptions: [Subscription; 2],
}

pub struct ClaudePanel {
    store: Entity<ClaudeStore>,
    /// The chats shown here (not moved to the editor), in pill order.
    chats: Vec<Entity<ClaudeChat>>,
    active: usize,
    width: ui::RightIslandWidth,
    focus_handle: FocusHandle,
    visible: bool,
    tab_scroll: ScrollHandle,
    /// Where a dragged pill would land.
    drop_index: Option<usize>,
    /// Sessions that finished or ask something while their chat wasn't on screen: a mark on the
    /// pill until it is shown.
    unseen: HashSet<EntityId>,
    menu: Option<Menu>,
    resizing: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ClaudePanelEvent> for ClaudePanel {}

impl ClaudePanel {
    pub fn new(
        store: Entity<ClaudeStore>,
        width: ui::RightIslandWidth,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![cx.subscribe(&store, |_, _, _: &ClaudeStoreEvent, cx| cx.notify())];
        Self {
            store,
            chats: Vec::new(),
            active: 0,
            width,
            focus_handle: cx.focus_handle(),
            visible: false,
            tab_scroll: ScrollHandle::new(),
            drop_index: None,
            unseen: HashSet::new(),
            menu: None,
            resizing: false,
            _subscriptions: subscriptions,
        }
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        if visible && let Some(chat) = self.chats.get(self.active) {
            self.unseen.remove(&chat.entity_id());
        }
        if !visible {
            self.menu = None;
        }
        cx.notify();
    }

    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.contains_focused(window, cx)
            || self
                .menu
                .as_ref()
                .is_some_and(|menu| menu.menu.focus_handle(cx).contains_focused(window, cx))
    }

    pub fn chats(&self) -> &[Entity<ClaudeChat>] {
        &self.chats
    }

    pub fn active_chat(&self) -> Option<&Entity<ClaudeChat>> {
        self.chats.get(self.active)
    }

    /// The chat is the one shown here (the window is visible and its pill is active).
    pub fn shows(&self, chat: &Entity<ClaudeChat>) -> bool {
        self.visible && self.chats.get(self.active) == Some(chat)
    }

    /// Marks a chat's pill: something happened in it while it wasn't on screen.
    pub fn mark_unseen(&mut self, chat: &Entity<ClaudeChat>, cx: &mut Context<Self>) {
        if !self.shows(chat) && self.chats.contains(chat) {
            self.unseen.insert(chat.entity_id());
            cx.notify();
        }
    }

    /// Adds a chat (a new session, or one coming back from the editor) at `index` (the end by
    /// default) and makes it active.
    pub fn add_chat(
        &mut self,
        chat: Entity<ClaudeChat>,
        index: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let index = index.unwrap_or(self.chats.len()).min(self.chats.len());
        self.chats.insert(index, chat);
        self.active = index;
        self.tab_scroll.scroll_to_item(index);
        cx.notify();
    }

    /// Takes a chat out (it moves to the editor, its session ended); `false` — it isn't here.
    pub fn remove_chat(&mut self, chat: &Entity<ClaudeChat>, cx: &mut Context<Self>) -> bool {
        let Some(index) = self.chats.iter().position(|known| known == chat) else {
            return false;
        };
        self.chats.remove(index);
        self.unseen.remove(&chat.entity_id());
        if index < self.active || self.active >= self.chats.len() {
            self.active = self.active.saturating_sub(1);
        }
        cx.notify();
        true
    }

    pub fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.chats.get(index) else {
            return;
        };
        self.unseen.remove(&chat.entity_id());
        self.active = index;
        self.tab_scroll.scroll_to_item(index);
        self.focus(window, cx);
        cx.notify();
    }

    /// Makes the chat active, if it is here.
    pub fn activate_chat(
        &mut self,
        chat: &Entity<ClaudeChat>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match self.chats.iter().position(|known| known == chat) {
            Some(index) => {
                self.activate(index, window, cx);
                true
            }
            None => false,
        }
    }

    /// Focus to the active chat's message field, or to the panel itself.
    pub fn focus(&self, window: &mut Window, cx: &App) {
        match self.chats.get(self.active) {
            Some(chat) => chat.read(cx).focus_composer(window, cx),
            None => window.focus(&self.focus_handle),
        }
    }

    fn index_of(&self, chat: &Entity<ClaudeChat>) -> Option<usize> {
        self.chats.iter().position(|known| known == chat)
    }

    // --- Menus ---

    /// The menu of a pill (right click): it acts on that session.
    fn secondary_click(
        &mut self,
        index: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate(index, window, cx);
        let menu = cx.new(|cx| {
            ContextMenu::new(window, cx)
                .entry(tr("Rename…"), claude::RenameSession)
                .entry(tr("Move to Editor"), claude::MoveToEditor)
                .entry(tr("Open in Terminal"), claude::OpenInTerminal)
                .separator()
                .entry(tr("Close Session"), claude::CloseSession)
        });
        self.show_menu(menu, position, window, cx);
    }

    /// ⋯ in the header: the sessions and the settings.
    fn open_more_menu(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        // The actions go to the active session, wherever the focus was.
        self.focus(window, cx);
        let has_chat = !self.chats.is_empty();
        let ready = self.store.read(cx).is_ready();
        let menu = cx.new(|cx| {
            let mut menu = ContextMenu::new(window, cx)
                .entry_if(ready, tr("New Session"), claude::NewSession);
            if has_chat {
                menu = menu
                    .separator()
                    .entry(tr("Rename…"), claude::RenameSession)
                    .entry(tr("Move to Editor"), claude::MoveToEditor)
                    .entry(tr("Open in Terminal"), claude::OpenInTerminal)
                    .separator()
                    .entry(tr("Close Session"), claude::CloseSession);
            }
            if !ready {
                menu = menu.separator().entry(tr("Check Again"), claude::CheckAgain);
            }
            menu.separator()
                .entry(tr("Claude Code Settings…"), claude::OpenSettings)
        });
        self.show_menu(menu, position, window, cx);
    }

    /// ⌄ next to the pills: every session with its status, to go to one that doesn't fit.
    fn open_sessions_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus(window, cx);
        let items: Vec<(usize, String)> = self
            .chats
            .iter()
            .enumerate()
            .map(|(index, chat)| {
                let session = chat.read(cx).session().read(cx);
                let status = claude::status_label(&session.model().status);
                let marker = if index == self.active { "● " } else { "" };
                (index, format!("{marker}{} · {status}", session.title()))
            })
            .collect();
        let menu = cx.new(|cx| {
            items
                .into_iter()
                .fold(ContextMenu::new(window, cx), |menu, (index, label)| {
                    menu.entry(label, ActivateSession(index))
                })
        });
        self.show_menu(menu, position, window, cx);
    }

    fn show_menu(
        &mut self,
        menu: Entity<ContextMenu>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    /// Closes the menu (if it is still the same one); Esc in it returns focus to the chat.
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

    // --- Dragging pills ---

    /// A pill is dragged over the pill at `index`: it would land before it, or after it in the
    /// pill's right half.
    fn drag_over_tab(
        &mut self,
        index: usize,
        event: &DragMoveEvent<DraggedClaudeChat>,
        cx: &mut Context<Self>,
    ) {
        let position = event.event.position;
        if !event.bounds.contains(&position) {
            return;
        }
        let after = position.x > event.bounds.center().x;
        self.set_drop_index(Some(index + usize::from(after)), cx);
    }

    fn drag_over_header(&mut self, event: &DragMoveEvent<DraggedClaudeChat>, cx: &mut Context<Self>) {
        if !event.bounds.contains(&event.event.position) {
            self.set_drop_index(None, cx);
        }
    }

    /// Over the free space after the pills: at the end.
    fn drag_over_tail(&mut self, event: &DragMoveEvent<DraggedClaudeChat>, cx: &mut Context<Self>) {
        if event.bounds.contains(&event.event.position) {
            self.set_drop_index(Some(self.chats.len()), cx);
        }
    }

    fn set_drop_index(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        if self.drop_index != index {
            self.drop_index = index;
            cx.notify();
        }
    }

    /// A pill dropped on the header: one of ours moves to its new place; a chat from the editor's
    /// tabs is handed over by the window ([`ClaudePanelEvent::MoveToPanel`]).
    fn drop_tab(&mut self, dragged: &DraggedClaudeChat, window: &mut Window, cx: &mut Context<Self>) {
        let index = self.drop_index.take().unwrap_or(self.chats.len());
        cx.notify();
        let chat = dragged.chat.clone();
        let Some(from) = self.index_of(&chat) else {
            return cx.emit(ClaudePanelEvent::MoveToPanel { chat, index });
        };
        // Taking the pill out shifts the places after it by one.
        let to = if from < index { index - 1 } else { index };
        let chat = self.chats.remove(from);
        let to = to.min(self.chats.len());
        self.chats.insert(to, chat);
        self.activate(to, window, cx);
    }

    // --- Drawing ---

    fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let drop_index = self.drop_index.filter(|_| cx.has_active_drag());
        // Pills that don't fit the island leave only the active one; ⌄ lists them all.
        let all_fit = self.pills_fit(cx);
        let tabs: Vec<AnyElement> = self
            .chats
            .iter()
            .enumerate()
            .filter(|(index, _)| all_fit || *index == self.active)
            .map(|(index, chat)| self.render_tab(index, chat, drop_index, cx))
            .collect();
        let ready = self.store.read(cx).is_ready();
        let has_chats = !self.chats.is_empty();
        // Without sessions, the window's name; with them, their pills (they scroll when they don't
        // fit; "+" follows them, the free space after it also takes a dropped pill).
        let title = (!has_chats).then(|| {
            div()
                .flex_none()
                .pl(px(GAP + 6.))
                .flex()
                .items_center()
                .gap_1p5()
                .child(icon(IconName::Claude, ui.accent_text).size(px(13.)))
                .child(ui::section_label(tr("Claude"), ui))
        });
        let strip = has_chats.then(|| {
            div()
                .id("claude-tabs")
                .flex_initial()
                .min_w_0()
                .h_full()
                .pl_1p5()
                .pr_0p5()
                .flex()
                .items_center()
                .gap_1()
                .overflow_x_scroll()
                .track_scroll(&self.tab_scroll)
                .children(tabs)
        });
        let sessions = (self.chats.len() > 1).then(|| {
            ui::icon_button("claude-sessions", IconName::ChevronDown, ui)
                .tooltip(ui::tooltip(tr("All Sessions"), None))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.open_sessions_menu(event.position, window, cx)
                    }),
                )
        });
        let new_session = (ready && has_chats).then(|| {
            ui::icon_button("claude-new-session", IconName::Plus, ui)
                .tooltip(ui::tooltip(tr("New Session"), None))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(claude::NewSession), cx))
        });
        let tail = div()
            .id("claude-tabs-tail")
            .flex_1()
            .h_full()
            .on_drag_move(cx.listener(
                |this, event: &DragMoveEvent<DraggedClaudeChat>, _, cx| this.drag_over_tail(event, cx),
            ));
        // The menu opens at the button, as the filter of Notifications.
        let more = ui::icon_button("claude-more", IconName::More, ui)
            .tooltip(ui::tooltip(tr("More"), None))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_more_menu(event.position, window, cx)
                }),
            );
        let hide = ui::icon_button("claude-hide", IconName::ChevronRight, ui)
            .tooltip(ui::tooltip(
                tr("Hide"),
                ui::shortcut_in(&notifications_panel::Hide, &self.focus_handle, window),
            ))
            .on_click(|_, window, cx| {
                window.dispatch_action(Box::new(notifications_panel::Hide), cx)
            });
        div()
            .id("claude-header")
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .pr(px(6.))
            .flex()
            .items_center()
            .gap_0p5()
            .on_drag_move(cx.listener(
                |this, event: &DragMoveEvent<DraggedClaudeChat>, _, cx| {
                    this.drag_over_header(event, cx)
                },
            ))
            .on_drop(cx.listener(|this, dragged: &DraggedClaudeChat, window, cx| {
                this.drop_tab(dragged, window, cx)
            }))
            .children(title)
            .children(strip)
            .children(sessions)
            .children(new_session)
            .child(tail)
            .child(more)
            .child(hide)
    }

    /// Whether every pill fits next to the header's buttons (an estimate by the titles' lengths, so
    /// that the strip doesn't flip between two layouts).
    fn pills_fit(&self, cx: &App) -> bool {
        const BUTTONS: f32 = 140.;
        const PILL_CHROME: f32 = 46.;
        const CHAR_WIDTH: f32 = 7.6;
        let needed: f32 = self
            .chats
            .iter()
            .enumerate()
            .map(|(index, chat)| {
                let max = if index == self.active {
                    TAB_LABEL_MAX_CHARS
                } else {
                    INACTIVE_TAB_LABEL_MAX_CHARS
                };
                let chars = chat.read(cx).session().read(cx).title().chars().count().min(max);
                PILL_CHROME + chars as f32 * CHAR_WIDTH
            })
            .sum();
        needed <= self.width.get() - BUTTONS
    }

    fn render_tab(
        &self,
        index: usize,
        chat: &Entity<ClaudeChat>,
        drop_index: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let active = index == self.active;
        let session = chat.read(cx).session().read(cx);
        let title = session.title();
        let status = session.model().status.clone();
        let unseen = self.unseen.contains(&chat.entity_id());
        let tooltip: SharedString = format!("{title} — {}", claude::status_label(&status)).into();
        let dragged = DraggedClaudeChat {
            chat: chat.clone(),
            title: title.clone(),
        };
        let close = {
            let chat = chat.clone();
            div()
                .id("close")
                .group("claude-tab-close")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(ui::RADIUS_XS))
                .hover(move |style| style.bg(ui.pressed))
                .when(!active, |close| {
                    close
                        .invisible()
                        .group_hover(TAB_GROUP, |style| style.visible())
                })
                // Clicking × must not activate the pill or start dragging it.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    cx.emit(ClaudePanelEvent::CloseChat(chat.clone()))
                }))
                .child(
                    icon(IconName::Close, ui.dim)
                        .size(px(12.))
                        .group_hover("claude-tab-close", move |style| {
                            style.text_color(ui.foreground)
                        }),
                )
        };
        let activate = chat.clone();
        let middle_close = chat.clone();
        div()
            .id(("claude-tab", chat.entity_id()))
            .group(TAB_GROUP)
            .relative()
            .flex_none()
            .h(px(TAB_HEIGHT))
            .pl_2()
            .pr_1()
            .flex()
            .items_center()
            .gap_1p5()
            .rounded(px(RADIUS_MD))
            .border_1()
            .text_color(if active { ui.foreground } else { ui.text_muted })
            .when(active, |tab| {
                tab.bg(ui.pressed)
                    .border_color(ui.island_border)
                    .font_weight(FontWeight::MEDIUM)
            })
            .when(!active, |tab| {
                tab.border_color(gpui::transparent_black())
                    .hover(|style| style.bg(ui.hover).text_color(ui.foreground))
            })
            .tooltip(ui::tooltip(tooltip, None))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    if let Some(index) = this.index_of(&activate) {
                        this.activate(index, window, cx)
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.secondary_click(index, event.position, window, cx)
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |_, _: &MouseUpEvent, _, cx| {
                    cx.emit(ClaudePanelEvent::CloseChat(middle_close.clone()))
                }),
            )
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .on_drag_move(cx.listener(
                move |this, event: &DragMoveEvent<DraggedClaudeChat>, _, cx| {
                    this.drag_over_tab(index, event, cx)
                },
            ))
            .when(drop_index == Some(index), |tab| tab.child(drop_marker(false, ui)))
            .when(
                drop_index == Some(self.chats.len()) && index + 1 == self.chats.len(),
                |tab| tab.child(drop_marker(true, ui)),
            )
            .child(status_dot(&status, unseen, ("claude-tab-dot", chat.entity_id()), &ui))
            .child(div().whitespace_nowrap().child(shorten(
                &title,
                if active {
                    TAB_LABEL_MAX_CHARS
                } else {
                    INACTIVE_TAB_LABEL_MAX_CHARS
                },
            )))
            .child(div().relative().flex_none().size(px(18.)).child(close))
            .into_any_element()
    }

    /// The window's content without a chat: what `claude` needs, or how to start.
    fn render_onboarding(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let store = self.store.read(cx);
        let project = store
            .root()
            .and_then(|root| root.file_name())
            .map(|name| name.to_string_lossy().into_owned());
        let column = div()
            .w_full()
            .max_w(px(ONBOARDING_WIDTH))
            .flex()
            .flex_col()
            .items_center()
            .gap_3()
            .text_center();
        let tile = |name: IconName, color: gpui::Hsla| {
            div()
                .size(px(44.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(ui::RADIUS_LG))
                .bg(UiColors::tint(color, 0.14))
                .border_1()
                .border_color(UiColors::tint(color, 0.3))
                .child(icon(name, color).size(px(22.)))
        };
        let heading = |text: SharedString| {
            div()
                .text_size(px(theme::TEXT_LG))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(ui.foreground)
                .child(text)
        };
        let note = |text: SharedString| {
            div()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.text_muted)
                .child(text)
        };
        let action_button = |id: &'static str, label: &'static str, primary: bool, action: Box<dyn Action>| {
            let button = if primary {
                ui::primary_button(id, label, true, ui)
            } else {
                ui::text_button(id, label, false, ui)
            };
            button.on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
        };
        let content = match store.cli() {
            CliState::Checking => column
                .child(spinner_dot(ui))
                .child(note(tr("Looking for Claude Code…").into())),
            CliState::Missing => column
                .child(tile(IconName::Claude, ui.accent_text))
                .child(heading(tr("Claude Code isn't installed").into()))
                .child(note(
                    tr("Flux runs the Claude Code CLI with your Claude subscription. Install it, then sign in.").into(),
                ))
                .child(
                    div()
                        .w_full()
                        .mt_1()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(claude::INSTALL_COMMANDS.iter().enumerate().map(
                            |(index, (title, command))| install_option(index, tr(title), command, ui),
                        )),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(action_button(
                            "claude-check-again",
                            tr("Check Again"),
                            true,
                            Box::new(claude::CheckAgain),
                        ))
                        .child(action_button(
                            "claude-open-settings",
                            tr("Set Path…"),
                            false,
                            Box::new(claude::OpenSettings),
                        )),
                ),
            CliState::SignedOut { .. } => column
                .child(tile(IconName::Claude, ui.accent_text))
                .child(heading(tr("Sign in to Claude").into()))
                .child(note(
                    tr("Claude Code works with your Claude account: a Pro, Max, Team or Enterprise subscription. Signing in opens the browser.").into(),
                ))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(action_button(
                            "claude-sign-in",
                            tr("Sign In"),
                            true,
                            Box::new(claude::SignIn),
                        ))
                        .child(action_button(
                            "claude-check-again",
                            tr("Check Again"),
                            false,
                            Box::new(claude::CheckAgain),
                        )),
                ),
            CliState::Failed(error) => column
                .child(tile(IconName::Warning, ui.warning))
                .child(heading(tr("Claude Code doesn't respond").into()))
                .child(
                    div()
                        .w_full()
                        .p_2()
                        .rounded(px(RADIUS_SM))
                        .bg(ui.input_background)
                        .border_1()
                        .border_color(ui.input_border)
                        .text_left()
                        .font_family(theme::code_font())
                        .text_size(px(theme::TEXT_XS))
                        .text_color(ui.text_muted)
                        .child(error.clone()),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(action_button(
                            "claude-check-again",
                            tr("Check Again"),
                            true,
                            Box::new(claude::CheckAgain),
                        ))
                        .child(action_button(
                            "claude-open-settings",
                            tr("Settings…"),
                            false,
                            Box::new(claude::OpenSettings),
                        )),
                ),
            CliState::Ready { account, .. } => {
                let keys = ui::shortcut_for(&claude::ToggleClaude, window);
                let mention_keys = ui::shortcut_for(&claude::AddSelectionToClaude, window);
                column
                    .child(tile(IconName::Claude, ui.accent_text))
                    .child(heading(tr("Start a session").into()))
                    .child(note(match &project {
                        Some(project) => trf(
                            "Claude works in {0}: ask it to explain, fix or build something.",
                            &[project],
                        )
                        .into(),
                        None => tr("Ask Claude to explain, fix or build something.").into(),
                    }))
                    .child(action_button(
                        "claude-start",
                        tr("New Session"),
                        true,
                        Box::new(claude::NewSession),
                    ))
                    .child(
                        div()
                            .mt_2()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_1p5()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .children(keys.map(|keys| hint(tr("Open Claude from anywhere"), &keys, ui)))
                            .children(mention_keys.map(|keys| {
                                hint(tr("Mention the selection in the message"), &keys, ui)
                            })),
                    )
                    .children(account.clone().map(|account| {
                        div()
                            .mt_1()
                            .text_size(px(theme::TEXT_XS))
                            .text_color(ui.dim)
                            .child(trf("Signed in as {0}", &[&account]))
                    }))
            }
        };
        div()
            .id("claude-onboarding")
            .size_full()
            .px_5()
            .py_6()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .child(content)
            .into_any_element()
    }
}

/// An install option: its name, the command (code font), and "Type in Terminal".
fn install_option(index: usize, title: &'static str, command: &'static str, ui: UiColors) -> impl IntoElement {
    div()
        .w_full()
        .p_2p5()
        .flex()
        .flex_col()
        .gap_1p5()
        .rounded(px(RADIUS_MD))
        .border_1()
        .border_color(ui.island_border)
        .text_left()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                // A long name wraps; the button keeps its size.
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(theme::TEXT_SM))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(ui.foreground)
                        .child(title),
                )
                .child(
                    ui::text_button(("claude-install", index), tr("Type in Terminal"), false, ui)
                        .on_click(move |_, window, cx| {
                            window.dispatch_action(
                                Box::new(claude::TypeInTerminal(command.to_string())),
                                cx,
                            )
                        }),
                ),
        )
        .child(
            div()
                .px_2()
                .py_1()
                .rounded(px(RADIUS_SM))
                .bg(ui.input_background)
                .font_family(theme::code_font())
                .text_size(px(theme::TEXT_XS))
                .text_color(ui.text_muted)
                .child(command),
        )
}

/// "Open Claude from anywhere   ⌘ Esc".
fn hint(label: &'static str, keys: &str, ui: UiColors) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(label)
        .child(ui::keys(keys, ui))
}

/// A session's status: a dot (pulsing while it works), a hollow ring when ready; a session that
/// finished out of sight gets the success color until it is shown.
pub fn status_dot(
    status: &Status,
    unseen: bool,
    id: impl Into<gpui::ElementId>,
    ui: &UiColors,
) -> AnyElement {
    let dot = div().flex_none().size(px(7.)).rounded(px(4.));
    match claude::status_color(status, ui) {
        Some(color) if status.is_working() && *status != Status::WaitingForUser => dot
            .bg(color)
            .with_animation(
                id,
                Animation::new(Duration::from_millis(1100)).repeat(),
                |dot, delta| dot.opacity(0.35 + 0.65 * (1. - (2. * delta - 1.).abs())),
            )
            .into_any_element(),
        Some(color) => dot.bg(color).into_any_element(),
        None if unseen => dot.bg(ui.success).into_any_element(),
        None => dot
            .border_1()
            .border_color(UiColors::tint(ui.text_muted, 0.6))
            .into_any_element(),
    }
}

/// A pulsing dot while `claude` is looked for.
fn spinner_dot(ui: UiColors) -> impl IntoElement {
    div()
        .size(px(9.))
        .rounded(px(5.))
        .bg(ui.accent)
        .with_animation(
            "claude-checking",
            Animation::new(Duration::from_millis(900)).repeat(),
            |dot, delta| dot.opacity(0.3 + 0.7 * (1. - (2. * delta - 1.).abs())),
        )
}

/// Where a dragged pill would land: a line at the pill's edge.
fn drop_marker(after: bool, ui: UiColors) -> impl IntoElement {
    div()
        .absolute()
        .top(px(3.))
        .bottom(px(3.))
        .when(after, |marker| marker.right(px(-3.)))
        .when(!after, |marker| marker.left(px(-3.)))
        .w(px(2.))
        .rounded(px(1.))
        .bg(ui.accent)
}

/// Width handle in the gap to the left of the island: an accent line on hover and while dragged.
fn resize_handle(resizing: bool, ui: UiColors) -> impl IntoElement {
    div()
        .id("claude-resize")
        .group("claude-resize")
        .absolute()
        .top_0()
        .bottom_0()
        .left(px(-(1. + RESIZE_HANDLE_WIDTH)))
        .w(px(RESIZE_HANDLE_WIDTH))
        .py(px(ui::RADIUS_LG))
        .flex()
        .justify_center()
        .cursor(CursorStyle::ResizeLeftRight)
        .on_drag(DraggedEdge, |_, _, _, cx| cx.new(|_| DraggedEdge))
        .child(
            div()
                .w(px(2.))
                .h_full()
                .rounded(px(1.))
                .bg(ui.focus_border)
                .when(!resizing, |line| {
                    line.invisible()
                        .group_hover("claude-resize", |style| style.visible())
                }),
        )
}

/// Shortens in the middle: "start…end".
pub fn shorten(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(count - tail).collect();
    format!("{start}…{end}")
}

impl Focusable for ClaudePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ClaudePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        self.resizing &= cx.has_active_drag();
        let header = self.render_header(window, cx);
        let body = match self.chats.get(self.active) {
            Some(chat) => chat.clone().into_any_element(),
            None => self.render_onboarding(window, cx),
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
            .on_action(cx.listener(|this, action: &ActivateSession, window, cx| {
                this.activate(action.0, window, cx)
            }))
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<DraggedEdge>, _, cx| {
                // The handle sits in the gap to the left of the edge.
                let width =
                    f32::from(event.bounds.right() - event.event.position.x) - RESIZE_HANDLE_OFFSET;
                this.width.set(width);
                this.resizing = true;
                cx.notify();
            }))
            .child(header)
            .child(ui::divider(ui).mx(px(GAP)))
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
            .child(resize_handle(self.resizing, ui))
            .children(
                self.menu
                    .as_ref()
                    .map(|menu| ContextMenu::overlay(&menu.menu, menu.position)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_titles_shorten_in_the_middle() {
        assert_eq!(shorten("fix tests", 24), "fix tests");
        let long = "Fix the failing lexer tests in the parser crate";
        let short = shorten(long, 24);
        assert_eq!(short.chars().count(), 24);
        assert!(short.starts_with("Fix the fail"));
        assert!(short.ends_with("arser crate"));
    }
}
