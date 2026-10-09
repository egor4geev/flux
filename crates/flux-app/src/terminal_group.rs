//! A terminal tab: one or more terminals, split side by side or one above another. The same group
//! sits either in the terminal panel or among the editor tabs (a tab can be dragged between them);
//! it splits panes, closes them (asking first if a command is running), and moves focus between
//! them.
//!
//! The panes form a tree: a split holds terminals or further splits along one axis, each with its
//! share of the space. A divider between two neighbors can be dragged; closing a pane gives its
//! share to the rest, and a split left with one child dissolves into its parent.

use std::path::PathBuf;

use gpui::{
    AnyElement, App, AppContext, Axis, Context, CursorStyle, DragMoveEvent, ElementId, Entity,
    EntityId, EventEmitter, FocusHandle, Focusable, KeyBinding, Render, SharedString, Subscription,
    Task, Window, actions, div, prelude::*, px, relative,
};

use crate::dialog::Dialog;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::terminal_view::{TerminalLink, TerminalView, TerminalViewEvent};
use crate::theme::{self, Theme, UiColors};
use crate::ui;

actions!(
    terminal,
    [
        SplitRight,
        SplitDown,
        ClosePane,
        FocusNextPane,
        FocusPreviousPane,
    ]
);

pub fn init(cx: &mut App) {
    let context = Some("Terminal");
    cx.bind_keys([
        KeyBinding::new("cmd-d", SplitRight, context),
        KeyBinding::new("cmd-shift-d", SplitDown, context),
        KeyBinding::new("cmd-w", ClosePane, context),
        // As "Goto Next Splitter" in JetBrains IDEs.
        KeyBinding::new("alt-tab", FocusNextPane, context),
        KeyBinding::new("alt-shift-tab", FocusPreviousPane, context),
    ]);
}

/// Dragging a divider stops short of making a pane narrower or lower than this.
const MIN_PANE: f32 = 80.;
/// The grab area of a divider; the line runs through its middle.
const DIVIDER_HIT: f32 = 8.;
/// Hover group of a divider: its line lights up under the mouse.
const DIVIDER_GROUP: &str = "terminal-pane-divider";
/// The bar marking the active pane when there are several.
const ACTIVE_MARKER_WIDTH: f32 = 28.;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalGroupEvent {
    /// The tab label may have changed: a pane's process or directory, or the active pane.
    TitleChanged,
    /// The bell rang in one of the panes.
    Bell,
    /// ⌘-click on a link in one of the panes.
    OpenLink(TerminalLink),
    /// The last pane is gone (its shell exited, or it was closed): the tab goes away.
    Empty,
    /// A new pane's shell didn't start: the reason (also shown in the group).
    ShellFailed(SharedString),
}

/// A node of the pane tree: a terminal (generic only for the tests), or a split.
enum Pane<T> {
    Leaf(T),
    Split(Split<T>),
}

/// Panes along one axis: side by side (`Horizontal`) or one above another (`Vertical`).
struct Split<T> {
    axis: Axis,
    children: Vec<Pane<T>>,
    /// Each child's share of the split's length; they add up to 1.
    ratios: Vec<f32>,
}

impl<T: Clone + PartialEq> Pane<T> {
    /// The terminals in order: left to right, top to bottom.
    fn leaves(&self, out: &mut Vec<T>) {
        match self {
            Pane::Leaf(leaf) => out.push(leaf.clone()),
            Pane::Split(split) => split.children.iter().for_each(|child| child.leaves(out)),
        }
    }

    /// Puts `new` next to `target` along `axis`: into the split `target` already sits in when it
    /// runs the same way (taking half of `target`'s share), otherwise into a new split in place of
    /// `target`. `false` if `target` isn't in this tree.
    fn split(&mut self, target: &T, new: &T, axis: Axis) -> bool {
        match self {
            Pane::Leaf(leaf) if leaf == target => {
                let leaf = leaf.clone();
                *self = Pane::Split(Split {
                    axis,
                    children: vec![Pane::Leaf(leaf), Pane::Leaf(new.clone())],
                    ratios: vec![0.5, 0.5],
                });
                true
            }
            Pane::Leaf(_) => false,
            Pane::Split(split) => {
                let position = split
                    .children
                    .iter()
                    .position(|child| matches!(child, Pane::Leaf(leaf) if leaf == target));
                if split.axis == axis
                    && let Some(index) = position
                {
                    let share = split.ratios[index] / 2.;
                    split.ratios[index] = share;
                    split.children.insert(index + 1, Pane::Leaf(new.clone()));
                    split.ratios.insert(index + 1, share);
                    return true;
                }
                split
                    .children
                    .iter_mut()
                    .any(|child| child.split(target, new, axis))
            }
        }
    }

    /// The tree without `view`; `None` if nothing is left. The neighbors share the freed space in
    /// proportion; a split left with one child is replaced by it, and a child split running the
    /// same way as its parent merges into it.
    fn without(self, target: &T) -> Option<Pane<T>> {
        let Pane::Split(Split {
            axis,
            children,
            ratios,
        }) = self
        else {
            return match self {
                Pane::Leaf(leaf) if leaf == *target => None,
                pane => Some(pane),
            };
        };
        let mut kept = Vec::with_capacity(children.len());
        let mut kept_ratios = Vec::with_capacity(children.len());
        for (child, ratio) in children.into_iter().zip(ratios) {
            match child.without(target) {
                Some(Pane::Split(inner)) if inner.axis == axis => {
                    kept_ratios.extend(inner.ratios.iter().map(|share| share * ratio));
                    kept.extend(inner.children);
                }
                Some(child) => {
                    kept.push(child);
                    kept_ratios.push(ratio);
                }
                None => {}
            }
        }
        match kept.len() {
            0 => None,
            1 => kept.pop(),
            _ => {
                let total: f32 = kept_ratios.iter().sum();
                for share in &mut kept_ratios {
                    *share /= total;
                }
                Some(Pane::Split(Split {
                    axis,
                    children: kept,
                    ratios: kept_ratios,
                }))
            }
        }
    }

    /// The split at a path of child indices from the root.
    fn split_at_mut(&mut self, path: &[usize]) -> Option<&mut Split<T>> {
        let Pane::Split(split) = self else {
            return None;
        };
        match path.split_first() {
            None => Some(split),
            Some((&index, rest)) => split.children.get_mut(index)?.split_at_mut(rest),
        }
    }
}

/// A divider being dragged: which group, which split (the path from the root), and which
/// boundary (between children `index` and `index + 1`).
#[derive(Clone)]
struct DraggedDivider {
    group: EntityId,
    path: Vec<usize>,
    index: usize,
}

impl Render for DraggedDivider {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// One terminal tab.
pub struct TerminalGroup {
    /// The pane tree; `None` once the last pane is gone (the tab is about to be dropped).
    panes: Option<Pane<Entity<TerminalView>>>,
    /// The pane that has or last had focus: the tab is named after it.
    active: Entity<TerminalView>,
    /// The project root: new panes look up relative paths there.
    root: Option<PathBuf>,
    /// The bell rang in a pane that didn't have focus.
    bell: bool,
    /// A divider is being dragged: its line stays lit.
    resizing: bool,
    /// Why the last split couldn't start its shell; cleared by the next split.
    error: Option<SharedString>,
    /// Subscriptions to the panes, dropped with their pane.
    subscriptions: Vec<(EntityId, Subscription)>,
}

impl EventEmitter<TerminalGroupEvent> for TerminalGroup {}

impl TerminalGroup {
    /// A tab with one terminal started in `cwd`; `root` is the project.
    pub fn spawn(
        cwd: Option<PathBuf>,
        root: Option<PathBuf>,
        window: &mut Window,
        cx: &mut App,
    ) -> std::io::Result<Entity<Self>> {
        let view = TerminalView::spawn(cwd, root.clone(), window, cx)?;
        Ok(cx.new(|cx| {
            let subscription = Self::subscribe(&view, window, cx);
            Self {
                panes: Some(Pane::Leaf(view.clone())),
                active: view.clone(),
                root,
                bell: false,
                resizing: false,
                error: None,
                subscriptions: vec![(view.entity_id(), subscription)],
            }
        }))
    }

    fn subscribe(
        view: &Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(view, window, |this, view, event, window, cx| match event {
            TerminalViewEvent::TitleChanged => cx.emit(TerminalGroupEvent::TitleChanged),
            TerminalViewEvent::Bell => {
                if !view.focus_handle(cx).is_focused(window) {
                    this.bell = true;
                    cx.notify();
                }
                cx.emit(TerminalGroupEvent::Bell);
            }
            TerminalViewEvent::OpenLink(link) => {
                cx.emit(TerminalGroupEvent::OpenLink(link.clone()))
            }
            TerminalViewEvent::Exited => this.remove_pane(view, window, cx),
            TerminalViewEvent::Focused => {
                this.active = view.clone();
                this.bell = false;
                cx.emit(TerminalGroupEvent::TitleChanged);
                cx.notify();
            }
        })
    }

    /// The pane that has or last had focus.
    pub fn active_view(&self) -> Entity<TerminalView> {
        self.active.clone()
    }

    /// The panes in order: left to right, top to bottom.
    pub fn views(&self) -> Vec<Entity<TerminalView>> {
        let mut views = Vec::new();
        if let Some(panes) = &self.panes {
            panes.leaves(&mut views);
        }
        views
    }

    /// The tab label: the active pane's process.
    pub fn title(&self, cx: &App) -> SharedString {
        self.active_view().read(cx).label()
    }

    /// The active pane's directory, as last read (the tab's tooltip, the detail of tabs with the
    /// same name).
    pub fn cwd(&self, cx: &App) -> Option<PathBuf> {
        self.active.read(cx).cwd.clone()
    }

    /// Whether focus is in one of the panes.
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.views()
            .iter()
            .any(|view| view.focus_handle(cx).contains_focused(window, cx))
    }

    /// Commands running in the panes (closing the tab would kill them).
    pub fn running_processes(&self, cx: &App) -> Vec<String> {
        self.views()
            .iter()
            .filter_map(|pane| pane.read(cx).running_process())
            .collect()
    }

    /// The bell rang in a pane since the tab was last looked at: the tab shows a mark.
    pub fn has_bell(&self) -> bool {
        self.bell
    }

    /// The tab was activated: the bell mark goes away.
    pub fn clear_bell(&mut self, cx: &mut Context<Self>) {
        if self.bell {
            self.bell = false;
            cx.notify();
        }
    }

    /// Asks before closing the whole tab if a command runs in it; `true` — close.
    pub fn confirm_close(&self, window: &mut Window, cx: &mut App) -> Task<bool> {
        let running = self.running_processes(cx);
        if running.is_empty() {
            return Task::ready(true);
        }
        let dialog = match running.as_slice() {
            [name] => Dialog::warning(trf("Terminate “{0}”?", &[name]))
                .message(tr("The process is still running in this terminal.")),
            names => Dialog::warning(tr("Terminate running processes?"))
                .message(trf("Still running in this tab: {0}.", &[&names.join(", ")])),
        };
        confirm_terminate(dialog, window, cx)
    }

    /// Splits the active pane: a new terminal to its right or below it, in its directory.
    pub fn split(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.active_view();
        let cwd = target.read(cx).cwd();
        let view = match TerminalView::spawn(cwd, self.root.clone(), window, cx) {
            Ok(view) => view,
            Err(err) => {
                self.error = Some(trf("Couldn't start the shell: {0}", &[&err]).into());
                cx.emit(TerminalGroupEvent::ShellFailed(err.to_string().into()));
                return cx.notify();
            }
        };
        let Some(panes) = self.panes.as_mut() else {
            return;
        };
        if !panes.split(&target, &view, axis) {
            return;
        }
        self.error = None;
        let subscription = Self::subscribe(&view, window, cx);
        self.subscriptions.push((view.entity_id(), subscription));
        self.active = view.clone();
        window.focus(&view.focus_handle(cx));
        cx.emit(TerminalGroupEvent::TitleChanged);
        cx.notify();
    }

    /// Closes a pane (⌘W), asking first if a command runs in it.
    pub fn close_pane(
        &mut self,
        view: &Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = view.read(cx).running_process() else {
            return self.remove_pane(view, window, cx);
        };
        let answer = confirm_terminate(
            Dialog::warning(trf("Terminate “{0}”?", &[&name]))
                .message(tr("The process is still running in this terminal.")),
            window,
            cx,
        );
        let view = view.clone();
        cx.spawn_in(window, async move |this, cx| {
            if answer.await {
                this.update_in(cx, |this, window, cx| this.remove_pane(&view, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Removes a pane without asking (its shell exited, or closing was confirmed); its terminal is
    /// dropped with it, which hangs up its shell. Focus moves to the neighbor that comes next in
    /// order (the previous one for the last pane) if the pane had it; the last pane gone empties
    /// the tab.
    fn remove_pane(
        &mut self,
        view: &Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let views = self.views();
        let Some(index) = views.iter().position(|pane| pane == view) else {
            return;
        };
        let had_focus = view.focus_handle(cx).contains_focused(window, cx);
        self.panes = self.panes.take().and_then(|panes| panes.without(view));
        self.subscriptions.retain(|(id, _)| *id != view.entity_id());
        let remaining = self.views();
        let Some(neighbor) = remaining.get(index.min(remaining.len().saturating_sub(1))) else {
            return cx.emit(TerminalGroupEvent::Empty);
        };
        if self.active == *view {
            self.active = neighbor.clone();
        }
        if had_focus {
            window.focus(&self.active.focus_handle(cx));
        }
        cx.emit(TerminalGroupEvent::TitleChanged);
        cx.notify();
    }

    /// ⌥⇥ / ⌥⇧⇥: focus to the next or the previous pane, around the end.
    fn focus_neighbor(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let views = self.views();
        let Some(index) = views.iter().position(|view| *view == self.active) else {
            return;
        };
        let next = (index as isize + step).rem_euclid(views.len() as isize) as usize;
        window.focus(&views[next].focus_handle(cx));
    }

    /// A divider of the split at `path` is being dragged: its two neighbors share their length
    /// anew, neither shorter than [`MIN_PANE`].
    fn drag_divider(
        &mut self,
        path: &[usize],
        event: &DragMoveEvent<DraggedDivider>,
        cx: &mut Context<Self>,
    ) {
        let drag = event.drag(cx);
        if drag.group != cx.entity_id() || drag.path != path {
            return;
        }
        let index = drag.index;
        let Some(split) = self
            .panes
            .as_mut()
            .and_then(|panes| panes.split_at_mut(path))
        else {
            return;
        };
        let (position, start, length) = match split.axis {
            Axis::Horizontal => (
                event.event.position.x,
                event.bounds.left(),
                event.bounds.size.width,
            ),
            Axis::Vertical => (
                event.event.position.y,
                event.bounds.top(),
                event.bounds.size.height,
            ),
        };
        let length = f32::from(length);
        if length <= 0. || index + 1 >= split.ratios.len() {
            return;
        }
        let at = f32::from(position - start) / length;
        let before: f32 = split.ratios[..index].iter().sum();
        let pair = split.ratios[index] + split.ratios[index + 1];
        let min = (MIN_PANE / length).min(pair / 2.);
        let first = (at - before).clamp(min, pair - min);
        split.ratios[index] = first;
        split.ratios[index + 1] = pair - first;
        self.resizing = true;
        cx.notify();
    }

    fn render_pane(
        &self,
        pane: &Pane<Entity<TerminalView>>,
        path: &[usize],
        several: bool,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        match pane {
            Pane::Leaf(view) => {
                // With several panes, the one that has (or last had) focus is marked by a short
                // bar at its top left corner, like the open tool in the launchpad.
                let marker = (several && *view == self.active).then(|| {
                    div()
                        .absolute()
                        .top(px(1.))
                        .left(px(ui::GAP))
                        .w(px(ACTIVE_MARKER_WIDTH))
                        .h(px(2.))
                        .rounded(px(1.))
                        .bg(if focused { ui.accent } else { ui.text_disabled })
                });
                div()
                    .relative()
                    .size_full()
                    .child(view.clone())
                    .children(marker)
                    .into_any_element()
            }
            Pane::Split(split) => {
                let horizontal = split.axis == Axis::Horizontal;
                let children: Vec<AnyElement> = split
                    .children
                    .iter()
                    .zip(&split.ratios)
                    .enumerate()
                    .map(|(index, (child, ratio))| {
                        let mut child_path = path.to_vec();
                        child_path.push(index);
                        let element = self.render_pane(child, &child_path, several, focused, cx);
                        div()
                            .flex_none()
                            .relative()
                            .overflow_hidden()
                            .when(horizontal, |cell| cell.h_full().w(relative(*ratio)))
                            .when(!horizontal, |cell| cell.w_full().h(relative(*ratio)))
                            .child(element)
                            .into_any_element()
                    })
                    .collect();
                let mut boundary = 0.;
                let dividers: Vec<AnyElement> = split.ratios[..split.ratios.len() - 1]
                    .iter()
                    .enumerate()
                    .map(|(index, ratio)| {
                        boundary += ratio;
                        self.render_divider(split.axis, path, index, boundary, cx)
                    })
                    .collect();
                let split_path = path.to_vec();
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .when(horizontal, |split| split.flex_row())
                    .when(!horizontal, |split| split.flex_col())
                    .on_drag_move(cx.listener(
                        move |this, event: &DragMoveEvent<DraggedDivider>, _, cx| {
                            this.drag_divider(&split_path, event, cx)
                        },
                    ))
                    .children(children)
                    .children(dividers)
                    .into_any_element()
            }
        }
    }

    /// The divider at `boundary` (a share of the split's length): a thin line in the middle of a
    /// wider grab area, lit on hover and while dragged.
    fn render_divider(
        &self,
        axis: Axis,
        path: &[usize],
        index: usize,
        boundary: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let horizontal = axis == Axis::Horizontal;
        let dragged = DraggedDivider {
            group: cx.entity_id(),
            path: path.to_vec(),
            index,
        };
        let id = ElementId::Name(format!("terminal-divider-{path:?}-{index}").into());
        let lit = self.resizing && cx.has_active_drag();
        let line = div()
            .flex_none()
            .rounded(px(1.))
            .when(horizontal, |line| line.w(px(1.)).h_full())
            .when(!horizontal, |line| line.h(px(1.)).w_full())
            .bg(if lit { ui.focus_border } else { ui.divider })
            .when(!lit, |line| {
                line.group_hover(DIVIDER_GROUP, move |style| style.bg(ui.focus_border))
            });
        div()
            .id(id)
            .group(DIVIDER_GROUP)
            .absolute()
            .flex()
            .items_center()
            .justify_center()
            .when(horizontal, |divider| {
                divider
                    .top_0()
                    .bottom_0()
                    .left(relative(boundary))
                    .ml(px(-DIVIDER_HIT / 2.))
                    .w(px(DIVIDER_HIT))
                    .cursor(CursorStyle::ResizeLeftRight)
            })
            .when(!horizontal, |divider| {
                divider
                    .left_0()
                    .right_0()
                    .top(relative(boundary))
                    .mt(px(-DIVIDER_HIT / 2.))
                    .h(px(DIVIDER_HIT))
                    .cursor(CursorStyle::ResizeUpDown)
            })
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .child(line)
            .into_any_element()
    }
}

impl Focusable for TerminalGroup {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.active_view().focus_handle(cx)
    }
}

impl Render for TerminalGroup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        self.resizing &= cx.has_active_drag();
        let several = self.views().len() > 1;
        let focused = several && self.contains_focus(window, cx);
        let panes = self
            .panes
            .as_ref()
            .map(|panes| self.render_pane(panes, &[], several, focused, cx));
        let error = self.error.clone().map(|error| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1p5()
                .px_2()
                .py_1()
                .font_family(theme::UI_FONT)
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.error)
                .child(icon(IconName::Warning, ui.error).size(px(13.)))
                .child(error)
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .on_action(cx.listener(|this, _: &ClosePane, window, cx| {
                let view = this.active_view();
                this.close_pane(&view, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SplitRight, window, cx| {
                this.split(Axis::Horizontal, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &SplitDown, window, cx| {
                    this.split(Axis::Vertical, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &FocusNextPane, window, cx| {
                    this.focus_neighbor(1, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &FocusPreviousPane, window, cx| {
                this.focus_neighbor(-1, window, cx)
            }))
            .children(error)
            .child(div().flex_1().min_h_0().children(panes))
    }
}

/// A terminal tab being dragged: between the panel's tab strip and the editor's. It is also the
/// label next to the pointer.
#[derive(Clone)]
pub struct DraggedTerminal {
    pub group: Entity<TerminalGroup>,
    pub title: SharedString,
}

impl Render for DraggedTerminal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        div()
            .flex()
            .items_center()
            .gap_1p5()
            .h(px(28.))
            .pl_2()
            .pr_2p5()
            .rounded(px(ui::RADIUS_MD))
            // Opaque: the label floats over the tabs, and their text must not show through.
            .bg(UiColors::tint(ui.elevated, 1.))
            .border_1()
            .border_color(ui.elevated_border)
            .shadow(ui::popover_shadow(ui))
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_MD))
            .text_color(ui.foreground)
            .child(icon(IconName::Terminal, ui.green).size(px(14.)))
            .child(self.title.clone())
    }
}

/// Asks whether to terminate what runs in a terminal being closed (`dialog` — the question): Terminate
/// or Cancel, with "Don't ask again" (Settings: `terminal.confirm_terminate`). `true` — terminate.
pub(crate) fn confirm_terminate(dialog: Dialog, window: &mut Window, cx: &mut App) -> Task<bool> {
    if !crate::settings::confirm_terminate(cx) {
        return Task::ready(true);
    }
    let answer = crate::dialog::ask(
        dialog
            .danger(tr("Terminate"))
            .cancel(tr("Cancel"))
            .dont_ask_again(),
        window,
        cx,
    );
    cx.spawn(async move |cx| {
        let Some(answer) = answer.await.filter(|answer| answer.button == 0) else {
            return false;
        };
        if answer.dont_ask_again {
            cx.update(|cx| crate::settings::set_confirm_terminate(false, cx))
                .ok();
        }
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tree as text: `[a | [b / c]]` — `|` side by side, `/` one above another.
    fn shape(pane: &Pane<char>) -> String {
        match pane {
            Pane::Leaf(leaf) => leaf.to_string(),
            Pane::Split(split) => {
                let separator = match split.axis {
                    Axis::Horizontal => " | ",
                    Axis::Vertical => " / ",
                };
                let children: Vec<String> = split.children.iter().map(shape).collect();
                format!("[{}]", children.join(separator))
            }
        }
    }

    fn ratios(pane: &Pane<char>) -> Vec<f32> {
        match pane {
            Pane::Split(split) => split.ratios.clone(),
            Pane::Leaf(_) => Vec::new(),
        }
    }

    #[test]
    fn splits_along_the_same_axis_add_a_sibling() {
        let mut tree = Pane::Leaf('a');
        assert!(tree.split(&'a', &'b', Axis::Horizontal));
        assert!(tree.split(&'a', &'c', Axis::Horizontal));
        assert_eq!(shape(&tree), "[a | c | b]");
        assert_eq!(ratios(&tree), vec![0.25, 0.25, 0.5]);
        assert!(!tree.split(&'x', &'d', Axis::Horizontal));
    }

    #[test]
    fn splits_across_nest_a_new_split() {
        let mut tree = Pane::Leaf('a');
        tree.split(&'a', &'b', Axis::Horizontal);
        tree.split(&'b', &'c', Axis::Vertical);
        assert_eq!(shape(&tree), "[a | [b / c]]");
        let mut leaves = Vec::new();
        tree.leaves(&mut leaves);
        assert_eq!(leaves, vec!['a', 'b', 'c']);
        assert!(
            tree.split_at_mut(&[1])
                .is_some_and(|split| split.axis == Axis::Vertical)
        );
        assert!(tree.split_at_mut(&[0]).is_none());
    }

    #[test]
    fn removing_a_pane_gives_its_share_to_the_rest() {
        let mut tree = Pane::Leaf('a');
        tree.split(&'a', &'b', Axis::Horizontal);
        tree.split(&'a', &'c', Axis::Horizontal);
        let tree = tree.without(&'c').unwrap();
        assert_eq!(shape(&tree), "[a | b]");
        assert_eq!(ratios(&tree), vec![1. / 3., 2. / 3.]);
    }

    #[test]
    fn a_split_left_with_one_child_dissolves_into_its_parent() {
        let mut tree = Pane::Leaf('a');
        tree.split(&'a', &'b', Axis::Horizontal);
        tree.split(&'b', &'c', Axis::Vertical);
        tree.split(&'c', &'d', Axis::Horizontal);
        assert_eq!(shape(&tree), "[a | [b / [c | d]]]");
        // Without b, the vertical split holds only [c | d], which runs the same way as the root:
        // c and d join the root, sharing b's former half.
        let tree = tree.without(&'b').unwrap();
        assert_eq!(shape(&tree), "[a | c | d]");
        assert_eq!(ratios(&tree), vec![0.5, 0.25, 0.25]);
        let tree = tree.without(&'a').unwrap().without(&'c').unwrap();
        assert_eq!(shape(&tree), "d");
        assert!(tree.without(&'d').is_none());
    }
}
