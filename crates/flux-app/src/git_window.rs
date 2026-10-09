//! The Git window: an island under the editor in place of the terminals (⌘9 shows and hides it, as
//! the Git tool window of JetBrains IDEs), with tabs — the Log and the histories of files and lines
//! (`git_log`). A tab can be dragged onto the editor's tab strip (or moved there from its menu) and
//! back. The island's height is shared with the terminal panel (`ui::BottomIslandHeight`).

use std::path::Path;

use gpui::{
    AnyElement, App, ClickEvent, Context, CursorStyle, DismissEvent, DragMoveEvent, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, KeyBinding, MouseButton, MouseDownEvent,
    MouseUpEvent, Pixels, Point, Render, SharedString, Subscription, Window, actions, div,
    prelude::*, px,
};

use crate::context_menu::ContextMenu;
use crate::git::GitStore;
use crate::git_log::{GitLogView, LogScope, relative_in_repo};
use crate::i18n::tr;
use crate::icons::{IconName, icon};
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, GAP, RADIUS_LG, RADIUS_MD};
use crate::workspace::{NextTab, PrevTab};

actions!(
    git_window,
    [HideWindow, MoveToEditor, MoveToPanel, CloseTab]
);

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "shift-escape",
        HideWindow,
        Some("GitWindow"),
    )]);
}

const HEADER_HEIGHT: f32 = 38.;
const TAB_HEIGHT: f32 = 26.;
const TAB_GROUP: &str = "git-window-tab";
const RESIZE_GROUP: &str = "git-window-resize";
const RESIZE_HANDLE_HEIGHT: f32 = GAP;
const RESIZE_HANDLE_OFFSET: f32 = 1. + RESIZE_HANDLE_HEIGHT / 2.;

/// A tab of the Git window being dragged: also the label next to the pointer.
#[derive(Clone)]
pub struct DraggedLogTab {
    pub view: Entity<GitLogView>,
    pub title: SharedString,
}

impl Render for DraggedLogTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        ui::popover(ui)
            .h(px(TAB_HEIGHT))
            .px_2()
            .flex()
            .items_center()
            .gap_1p5()
            .text_size(px(theme::TEXT_SM))
            .child(icon(IconName::History, ui.accent).size(px(13.)))
            .child(self.title.clone())
    }
}

#[derive(Debug, Clone)]
pub enum GitWindowEvent {
    /// A tab from the editor area was dropped on the strip at `index`: the window takes it out of
    /// the editor tabs and gives it to [`GitWindow::add_view`].
    MoveToPanel {
        view: Entity<GitLogView>,
        index: usize,
    },
    /// The last tab left: the window hides the island.
    Empty,
}

struct WindowTab {
    view: Entity<GitLogView>,
    _subscription: Subscription,
}

struct Menu {
    menu: Entity<ContextMenu>,
    position: Point<Pixels>,
    _subscriptions: [Subscription; 2],
}

#[derive(Debug, Clone, Copy)]
struct DraggedEdge;

impl Render for DraggedEdge {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

pub struct GitWindow {
    git: Entity<GitStore>,
    tabs: Vec<WindowTab>,
    active: usize,
    height: ui::BottomIslandHeight,
    resizing: bool,
    focus_handle: FocusHandle,
    drop_index: Option<usize>,
    menu: Option<Menu>,
}

impl EventEmitter<GitWindowEvent> for GitWindow {}

impl GitWindow {
    pub fn new(
        git: Entity<GitStore>,
        height: ui::BottomIslandHeight,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            git,
            tabs: Vec::new(),
            active: 0,
            height,
            resizing: false,
            focus_handle: cx.focus_handle(),
            drop_index: None,
            menu: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn active_view(&self) -> Option<Entity<GitLogView>> {
        self.tabs.get(self.active).map(|tab| tab.view.clone())
    }

    /// The repository log's tab, if it is here.
    pub fn log_view(&self, cx: &App) -> Option<Entity<GitLogView>> {
        self.tabs
            .iter()
            .map(|tab| tab.view.clone())
            .find(|view| *view.read(cx).scope() == LogScope::All)
    }

    fn index_of(&self, view: &Entity<GitLogView>) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.view == *view)
    }

    /// A new log of the repository (the Log tab) at the start.
    pub fn new_log(
        &mut self,
        repo: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<GitLogView> {
        let git = self.git.clone();
        let view = cx.new(|cx| GitLogView::new(git, repo, LogScope::All, window, cx));
        self.add_view(view.clone(), Some(0), window, cx);
        view
    }

    /// A history tab of a file (absolute path) or its lines; an open one for the same is reused.
    /// `None` — the file isn't in a repository.
    pub fn show_history(
        &mut self,
        path: &Path,
        lines: Option<(u32, u32)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<GitLogView>> {
        let (repo, relative) = relative_in_repo(self.git.read(cx), path)?;
        let scope = match lines {
            Some((start, end)) => LogScope::Lines {
                path: relative,
                start: start.min(end),
                end: start.max(end),
            },
            None => LogScope::File { path: relative },
        };
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| *tab.view.read(cx).scope() == scope && tab.view.read(cx).repo() == repo)
        {
            self.activate(index, window, cx);
            return Some(self.tabs[index].view.clone());
        }
        let git = self.git.clone();
        let view = cx.new(|cx| GitLogView::new(git, repo, scope, window, cx));
        self.add_view(view.clone(), None, window, cx);
        Some(view)
    }

    /// Adds a tab (a new one, or one moved back from the editor area) at `index` (by default, at the
    /// end) and focuses it.
    pub fn add_view(
        &mut self,
        view: Entity<GitLogView>,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let subscription = cx.observe(&view, |_, _, cx| cx.notify());
        let index = index.unwrap_or(self.tabs.len()).min(self.tabs.len());
        self.tabs.insert(
            index,
            WindowTab {
                view,
                _subscription: subscription,
            },
        );
        self.activate(index, window, cx);
    }

    /// Takes a tab out (it moves to the editor area). `false` if it isn't here.
    pub fn remove_view(&mut self, view: &Entity<GitLogView>, cx: &mut Context<Self>) -> bool {
        let Some(index) = self.index_of(view) else {
            return false;
        };
        self.tabs.remove(index);
        if index < self.active || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        cx.notify();
        true
    }

    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_view() {
            Some(view) => window.focus(&view.focus_handle(cx)),
            None => window.focus(&self.focus_handle),
        }
    }

    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.contains_focused(window, cx)
            || self
                .tabs
                .iter()
                .any(|tab| tab.view.read(cx).contains_focus(window, cx))
            || self
                .menu
                .as_ref()
                .is_some_and(|menu| menu.menu.focus_handle(cx).is_focused(window))
    }

    pub fn activate_view(
        &mut self,
        view: &Entity<GitLogView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.index_of(view) {
            self.activate(index, window, cx);
        }
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        self.active = index;
        window.focus(&tab.view.focus_handle(cx));
        cx.notify();
    }

    fn cycle(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.tabs.len() as isize;
        if len > 0 {
            let index = (self.active as isize + step).rem_euclid(len) as usize;
            self.activate(index, window, cx);
        }
    }

    /// Closes a history tab (the Log tab stays: it has no ×, and ⌘W skips it).
    fn close_tab(
        &mut self,
        view: &Entity<GitLogView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if *view.read(cx).scope() == LogScope::All {
            return;
        }
        let had_focus = self.contains_focus(window, cx);
        self.remove_view(view, cx);
        if self.tabs.is_empty() {
            return cx.emit(GitWindowEvent::Empty);
        }
        if had_focus {
            self.activate(self.active, window, cx);
        }
    }

    fn secondary_click(
        &mut self,
        index: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate(index, window, cx);
        let closable = self
            .tabs
            .get(index)
            .is_some_and(|tab| *tab.view.read(cx).scope() != LogScope::All);
        let menu = cx.new(|cx| {
            ContextMenu::new(window, cx)
                .entry(tr("Move to Editor"), MoveToEditor)
                .separator()
                .entry_if(closable, tr("Close"), CloseTab)
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
            self.focus(window, cx);
        }
        cx.notify();
    }

    // --- Dragging tabs ---

    fn drag_over_tab(
        &mut self,
        index: usize,
        event: &DragMoveEvent<DraggedLogTab>,
        cx: &mut Context<Self>,
    ) {
        let position = event.event.position;
        if !event.bounds.contains(&position) {
            return;
        }
        let after = position.x > event.bounds.center().x;
        self.set_drop_index(Some(index + usize::from(after)), cx);
    }

    fn set_drop_index(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        if self.drop_index != index {
            self.drop_index = index;
            cx.notify();
        }
    }

    fn drop_tab(&mut self, dragged: &DraggedLogTab, window: &mut Window, cx: &mut Context<Self>) {
        let index = self.drop_index.take().unwrap_or(self.tabs.len());
        cx.notify();
        let view = dragged.view.clone();
        let Some(from) = self.index_of(&view) else {
            return cx.emit(GitWindowEvent::MoveToPanel { view, index });
        };
        let to = if from < index { index - 1 } else { index };
        let tab = self.tabs.remove(from);
        self.tabs.insert(to.min(self.tabs.len()), tab);
        self.activate(to.min(self.tabs.len() - 1), window, cx);
    }

    // --- Display ---

    fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let drop_index = self.drop_index.filter(|_| cx.has_active_drag());
        let tabs: Vec<AnyElement> = (0..self.tabs.len())
            .map(|index| self.render_tab(index, drop_index, cx))
            .collect();
        let tail = div()
            .id("git-window-tail")
            .flex_1()
            .h_full()
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedLogTab>, _, cx| {
                    if event.bounds.contains(&event.event.position) {
                        this.set_drop_index(Some(this.tabs.len()), cx);
                    }
                }),
            );
        div()
            .id("git-window-header")
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .pl_1p5()
            .pr(px(GAP - 2.))
            .flex()
            .items_center()
            .gap_1()
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedLogTab>, _, cx| {
                    if !event.bounds.contains(&event.event.position) {
                        this.set_drop_index(None, cx);
                    }
                }),
            )
            .on_drop(cx.listener(|this, dragged: &DraggedLogTab, window, cx| {
                this.drop_tab(dragged, window, cx)
            }))
            .child(
                div()
                    .flex_none()
                    .pl_1()
                    .pr_1()
                    .text_color(ui.text_muted)
                    .font_weight(FontWeight::MEDIUM)
                    .child(tr("Git")),
            )
            .children(tabs)
            .child(tail)
            .child(
                ui::icon_button("git-window-hide", IconName::ChevronDown, ui)
                    .tooltip(ui::tooltip(
                        tr("Hide"),
                        ui::shortcut_for(&crate::git::ToggleGitWindow, window),
                    ))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(HideWindow), cx)),
            )
    }

    fn render_tab(
        &self,
        index: usize,
        drop_index: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let active = index == self.active;
        let view = self.tabs[index].view.clone();
        let (title, tooltip, is_log) = {
            let view = view.read(cx);
            (view.title(), view.tooltip(), *view.scope() == LogScope::All)
        };
        let dragged = DraggedLogTab {
            view: view.clone(),
            title: title.clone(),
        };
        let close = (!is_log).then(|| {
            let view = view.clone();
            div()
                .id("close")
                .flex_none()
                .size(px(18.))
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
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.close_tab(&view, window, cx)
                }))
                .child(icon(IconName::Close, ui.dim).size(px(12.)))
        });
        let (activate, middle) = (view.clone(), view.clone());
        div()
            .id(("git-window-tab", view.entity_id()))
            .group(TAB_GROUP)
            .relative()
            .flex_none()
            .h(px(TAB_HEIGHT))
            .pl_2()
            .when(is_log, |tab| tab.pr_2())
            .when(!is_log, |tab| tab.pr_1())
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
            .when_some(tooltip, |tab, tooltip| {
                tab.tooltip(ui::tooltip(tooltip, None))
            })
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
                cx.listener(move |this, _: &MouseUpEvent, window, cx| {
                    this.close_tab(&middle, window, cx)
                }),
            )
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .on_drag_move(
                cx.listener(move |this, event: &DragMoveEvent<DraggedLogTab>, _, cx| {
                    this.drag_over_tab(index, event, cx)
                }),
            )
            .when(drop_index == Some(index), |tab| {
                tab.child(drop_marker(false, ui))
            })
            .when(
                drop_index == Some(self.tabs.len()) && index + 1 == self.tabs.len(),
                |tab| tab.child(drop_marker(true, ui)),
            )
            .child(
                icon(
                    if is_log {
                        IconName::Commit
                    } else {
                        IconName::History
                    },
                    ui.accent,
                )
                .size(px(13.)),
            )
            .child(div().whitespace_nowrap().child(title))
            .children(close)
            .into_any_element()
    }
}

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

fn resize_handle(resizing: bool, ui: UiColors) -> impl IntoElement {
    div()
        .id("git-window-resize")
        .group(RESIZE_GROUP)
        .absolute()
        .top(px(-(1. + RESIZE_HANDLE_HEIGHT)))
        .left_0()
        .right_0()
        .h(px(RESIZE_HANDLE_HEIGHT))
        .px(px(RADIUS_LG))
        .flex()
        .items_center()
        .cursor(CursorStyle::ResizeUpDown)
        .on_drag(DraggedEdge, |_, _, _, cx| cx.new(|_| DraggedEdge))
        .child(
            div()
                .w_full()
                .h(px(2.))
                .rounded(px(1.))
                .bg(ui.focus_border)
                .when(!resizing, |line| {
                    line.invisible()
                        .group_hover(RESIZE_GROUP, |style| style.visible())
                }),
        )
}

impl Focusable for GitWindow {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self.active_view() {
            Some(view) => view.focus_handle(cx),
            None => self.focus_handle.clone(),
        }
    }
}

impl Render for GitWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        self.resizing &= cx.has_active_drag();
        let max_height =
            f32::from(window.viewport_size().height) * ui::BottomIslandHeight::MAX_SHARE;
        let height = self.height.get_within(max_height);
        let height_cell = self.height.clone();
        div()
            .key_context("GitWindow")
            .track_focus(&self.focus_handle)
            .relative()
            .h(px(height))
            .flex()
            .flex_col()
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_SM))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle(1, window, cx)))
            .on_action(cx.listener(|this, _: &PrevTab, window, cx| this.cycle(-1, window, cx)))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if let Some(view) = this.active_view() {
                    this.close_tab(&view, window, cx)
                }
            }))
            .on_drag_move(
                cx.listener(move |this, event: &DragMoveEvent<DraggedEdge>, _, cx| {
                    let height = f32::from(event.bounds.bottom() - event.event.position.y)
                        - RESIZE_HANDLE_OFFSET;
                    height_cell.set_within(height, max_height);
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
                    .pb_1p5()
                    .children(self.active_view()),
            )
            .child(resize_handle(self.resizing, ui))
            .children(
                self.menu
                    .as_ref()
                    .map(|menu| ContextMenu::overlay(&menu.menu, menu.position)),
            )
    }
}
