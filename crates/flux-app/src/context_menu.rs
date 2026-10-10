//! Context menu (right mouse button): a list of items over the window at the mouse cursor.
//!
//! [`ContextMenu`] is a view with its own focus: ↑/↓, ↵, ⎋ work right away. The owner draws it on
//! top of everything at the click point ([`ContextMenu::overlay`]) and closes it on
//! [`DismissEvent`]: an item is chosen, Esc is pressed, or the user clicks outside. An item is a
//! gpui action: before dispatching it, focus returns to where it was before the menu
//! (`dispatch_action` takes the focus at the moment of the call, as in the command palette), so the
//! action is received by whoever opened the menu. The shortcuts on the right come from the keymap
//! of that same place.
//!
//! Submenus ([`ContextMenu::submenu`]) work as in JetBrains IDEs: an item with ▸ opens its child
//! menu to the right (to the left at the window's edge) after the pointer rests on it, on a click,
//! → or ↵; ← and Esc close the child, its parent item stays highlighted while it is open. The child
//! is drawn by the same view, so the focus (and the owner's focus-out handling) stays with the
//! menu.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Action, AnyElement, App, Bounds, ClickEvent, Context, DismissEvent, Entity, EventEmitter,
    FocusHandle, Focusable, KeyBinding, MouseDownEvent, Pixels, Point, Render, SharedString, Task,
    Window, actions, anchored, canvas, deferred, div, point, prelude::*, px,
};

use crate::command_palette::keystroke_label;
use crate::icons::{IconName, icon};
use crate::popup;
use crate::theme::Theme;
use crate::ui::{self, RADIUS_SM};

pub const MENU_MIN_WIDTH: f32 = 240.;
const ITEM_HEIGHT: f32 = 28.;
/// Inset of the items from the menu edge; item corner radius = menu corner radius − inset.
const MENU_PADDING: f32 = 5.;
/// A submenu sits this far from its menu.
const SUBMENU_GAP: f32 = 2.;
/// The pointer rests on an item with ▸ this long before its submenu opens…
const HOVER_OPEN_DELAY: Duration = Duration::from_millis(150);
/// …and this long on another item before an open submenu gives way: the pointer crosses items on
/// its way into the submenu.
const HOVER_SWITCH_DELAY: Duration = Duration::from_millis(300);

actions!(
    context_menu,
    [
        SelectNext,
        SelectPrevious,
        Confirm,
        Cancel,
        /// →: into the submenu of the selected item.
        SelectChild,
        /// ←: out of the submenu.
        SelectParent,
    ]
);

pub fn init(cx: &mut App) {
    let context = Some("ContextMenu");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("enter", Confirm, context),
        KeyBinding::new("escape", Cancel, context),
        KeyBinding::new("right", SelectChild, context),
        KeyBinding::new("left", SelectParent, context),
    ]);
}

enum Item {
    Entry {
        label: SharedString,
        action: Box<dyn Action>,
        enabled: bool,
    },
    Separator,
    Submenu {
        label: SharedString,
        items: Vec<Item>,
    },
}

impl Item {
    /// What ↑/↓ stop at: an available entry, a submenu.
    fn selectable(&self) -> bool {
        matches!(self, Item::Entry { enabled: true, .. } | Item::Submenu { .. })
    }
}

/// The items of a submenu: [`ContextMenu::submenu`] fills it.
#[derive(Default)]
pub struct Submenu {
    items: Vec<Item>,
}

impl Submenu {
    pub fn entry(self, label: impl Into<SharedString>, action: impl Action) -> Self {
        self.entry_if(true, label, action)
    }

    /// An item that is visible but unavailable when `!enabled`.
    pub fn entry_if(
        self,
        enabled: bool,
        label: impl Into<SharedString>,
        action: impl Action,
    ) -> Self {
        self.boxed_entry_if(enabled, label, Box::new(action))
    }

    /// [`Self::entry_if`] with an action chosen at run time.
    pub fn boxed_entry_if(
        mut self,
        enabled: bool,
        label: impl Into<SharedString>,
        action: Box<dyn Action>,
    ) -> Self {
        self.items.push(Item::Entry {
            label: label.into(),
            action,
            enabled,
        });
        self
    }

    /// A separator; not drawn at the start, at the end, or twice in a row.
    pub fn separator(mut self) -> Self {
        push_separator(&mut self.items);
        self
    }
}

fn push_separator(items: &mut Vec<Item>) {
    if !items.is_empty() && !matches!(items.last(), Some(Item::Separator)) {
        items.push(Item::Separator);
    }
}

/// The submenu that is open: the item it belongs to, and its item selected with the keyboard.
struct Open {
    index: usize,
    selected: Option<usize>,
}

/// Where the menu was drawn in the last frame (window coordinates): the child menu is placed next
/// to its item, and a click inside the child is not a click outside the menu.
#[derive(Default)]
struct Geometry {
    panel: Cell<Option<Bounds<Pixels>>>,
    items: RefCell<HashMap<usize, Bounds<Pixels>>>,
    /// The open child menu: its item, and its bounds.
    child: Cell<Option<(usize, Bounds<Pixels>)>>,
}

pub struct ContextMenu {
    /// A title above the items (the menu of version control operations).
    title: Option<SharedString>,
    items: Vec<Item>,
    /// The item selected with the arrow keys; the mouse highlights its own item on its own.
    selected: Option<usize>,
    open: Option<Open>,
    /// The item under the pointer and what resting on it will do (open its submenu, close one).
    hovered: Option<usize>,
    hover_task: Option<Task<()>>,
    geometry: Rc<Geometry>,
    focus_handle: FocusHandle,
    /// Whoever was focused before the menu: the action is dispatched to it, and shortcuts are
    /// looked up from it.
    previous_focus: Option<FocusHandle>,
}

impl EventEmitter<DismissEvent> for ContextMenu {}

impl ContextMenu {
    /// An empty menu for whoever is currently in focus. Items: [`Self::entry`],
    /// [`Self::separator`], [`Self::submenu`].
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            title: None,
            items: Vec::new(),
            selected: None,
            open: None,
            hovered: None,
            hover_task: None,
            geometry: Rc::default(),
            focus_handle: cx.focus_handle(),
            previous_focus: window.focused(cx),
        }
    }

    /// A title above the items.
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn entry(self, label: impl Into<SharedString>, action: impl Action) -> Self {
        self.entry_if(true, label, action)
    }

    /// An item that is visible but unavailable when `!enabled` (for example, "Paste" with nothing
    /// copied).
    pub fn entry_if(
        mut self,
        enabled: bool,
        label: impl Into<SharedString>,
        action: impl Action,
    ) -> Self {
        self.items.push(Item::Entry {
            label: label.into(),
            action: Box::new(action),
            enabled,
        });
        self
    }

    /// A separator; not drawn at the start, at the end, or twice in a row.
    pub fn separator(mut self) -> Self {
        push_separator(&mut self.items);
        self
    }

    /// An item with ▸ and a child menu: `menu.submenu("Git", |git| git.entry(…).entry(…))`. A
    /// submenu without items is left out.
    pub fn submenu(
        mut self,
        label: impl Into<SharedString>,
        build: impl FnOnce(Submenu) -> Submenu,
    ) -> Self {
        let mut items = build(Submenu::default()).items;
        if matches!(items.last(), Some(Item::Separator)) {
            items.pop();
        }
        if items.iter().any(|item| matches!(item, Item::Entry { .. })) {
            self.items.push(Item::Submenu {
                label: label.into(),
                items,
            });
        }
        self
    }

    /// The menu over the window at the point `position` (window coordinates); near the window edge
    /// it is shifted inward, keeping the margin every popup keeps.
    pub fn overlay(menu: &Entity<Self>, position: Point<Pixels>) -> AnyElement {
        deferred(
            anchored()
                .position(position)
                .snap_to_window_with_margin(px(popup::WINDOW_MARGIN))
                .child(menu.clone()),
        )
        .with_priority(1)
        .into_any_element()
    }

    /// The items of the open submenu.
    fn child_items(&self) -> Option<&[Item]> {
        let open = self.open.as_ref()?;
        match self.items.get(open.index) {
            Some(Item::Submenu { items, .. }) => Some(items),
            _ => None,
        }
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.step(true, cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.step(false, cx);
    }

    /// ↓ / ↑: within the submenu the keyboard went into, else within the menu (an open submenu
    /// closes).
    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.hover_task = None;
        if let Some(items) = self.child_items()
            && let Some(selected) = self.open.as_ref().and_then(|open| open.selected)
        {
            let next = step(items, Some(selected), forward);
            if let Some(open) = &mut self.open {
                open.selected = next;
            }
        } else {
            self.open = None;
            self.selected = step(&self.items, self.selected, forward);
        }
        cx.notify();
    }

    /// →: into the submenu (it opens if it isn't).
    fn select_child(&mut self, _: &SelectChild, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(open) = &self.open
            && open.selected.is_some()
        {
            return;
        }
        let index = self
            .open
            .as_ref()
            .map(|open| open.index)
            .or(self.selected);
        if let Some(index) = index {
            self.open_submenu(index, true, cx);
        }
    }

    /// ←: the submenu closes; its item stays selected.
    fn select_parent(&mut self, _: &SelectParent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(open) = self.open.take() {
            self.selected = Some(open.index);
            self.hover_task = None;
            cx.notify();
        }
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(open) = &self.open
            && let Some(child) = open.selected
        {
            let index = open.index;
            return self.run_child(index, child, window, cx);
        }
        if let Some(index) = self.selected {
            if matches!(self.items.get(index), Some(Item::Submenu { .. })) {
                return self.open_submenu(index, true, cx);
            }
            self.run(index, window, cx);
        }
    }

    /// Esc: an open submenu closes first, then the menu.
    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(open) = self.open.take() {
            self.selected = Some(open.index);
            cx.notify();
        } else {
            cx.emit(DismissEvent);
        }
    }

    /// Opens the submenu of the item at `index`; `select_first` — the keyboard goes into it.
    fn open_submenu(&mut self, index: usize, select_first: bool, cx: &mut Context<Self>) {
        let Some(Item::Submenu { items, .. }) = self.items.get(index) else {
            return;
        };
        let selected = select_first
            .then(|| items.iter().position(Item::selectable))
            .flatten();
        self.hover_task = None;
        self.selected = Some(index);
        self.open = Some(Open { index, selected });
        cx.notify();
    }

    /// The pointer came onto an item (`hovered`) or left it. An item with ▸ opens its submenu
    /// after a short rest; with a submenu open, another item takes over only after a longer one —
    /// the pointer may just be crossing it on the way into the submenu.
    fn hover_item(&mut self, index: usize, hovered: bool, cx: &mut Context<Self>) {
        if !hovered {
            if self.hovered == Some(index) {
                self.hovered = None;
            }
            return;
        }
        self.hovered = Some(index);
        if self.open.as_ref().is_some_and(|open| open.index == index) {
            self.hover_task = None;
            return;
        }
        let is_submenu = matches!(self.items.get(index), Some(Item::Submenu { .. }));
        let delay = if self.open.is_some() {
            HOVER_SWITCH_DELAY
        } else if is_submenu {
            HOVER_OPEN_DELAY
        } else {
            self.hover_task = None;
            return;
        };
        self.hover_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |this, cx| {
                if this.hovered != Some(index) {
                    return;
                }
                this.hover_task = None;
                if matches!(this.items.get(index), Some(Item::Submenu { .. })) {
                    this.open_submenu(index, false, cx);
                } else {
                    this.open = None;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// The pointer reached the submenu: a switch it started on the way is called off.
    fn hover_child(&mut self, hovered: bool) {
        if hovered {
            self.hover_task = None;
            self.hovered = None;
        }
    }

    /// Executes the item: focus goes back, the action is dispatched to the same place, and the menu
    /// closes.
    fn run(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Item::Entry {
            action,
            enabled: true,
            ..
        }) = self.items.get(index)
        else {
            return;
        };
        let action = action.boxed_clone();
        self.dispatch(action, window, cx);
    }

    fn run_child(&mut self, index: usize, child: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Item::Submenu { items, .. }) = self.items.get(index) else {
            return;
        };
        let Some(Item::Entry {
            action,
            enabled: true,
            ..
        }) = items.get(child)
        else {
            return;
        };
        let action = action.boxed_clone();
        self.dispatch(action, window, cx);
    }

    fn dispatch(&mut self, action: Box<dyn Action>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(focus) = &self.previous_focus {
            window.focus(focus);
        }
        cx.emit(DismissEvent);
        window.dispatch_action(action, cx);
    }

    /// The action's shortcut as macOS symbols, in the key context of whoever opened the menu.
    fn keys_for(&self, action: &dyn Action, window: &Window) -> Option<SharedString> {
        let bindings = match &self.previous_focus {
            Some(focus) => window.bindings_for_action_in(action, focus),
            None => window.bindings_for_action(action),
        };
        let binding = bindings.first()?;
        let keys: Vec<String> = binding
            .keystrokes()
            .iter()
            .map(|keystroke| keystroke_label(keystroke.modifiers(), keystroke.key()))
            .collect();
        Some(keys.join(" ").into())
    }

    /// An entry in its row ([`item_row`]): the label and the shortcut.
    fn render_entry(
        &self,
        row: gpui::Stateful<gpui::Div>,
        label: &SharedString,
        action: &dyn Action,
        enabled: bool,
        window: &Window,
        cx: &App,
    ) -> gpui::Stateful<gpui::Div> {
        let ui = Theme::ui(cx);
        let keys = self.keys_for(action, window);
        row.child(label.clone())
            .children(keys.map(|keys| ui::keys(&keys, ui).when(!enabled, |keys| keys.opacity(0.5))))
    }

    fn render_child(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.open.as_ref()?;
        let items = self.child_items()?;
        let geometry = &self.geometry;
        let panel = geometry.panel.get()?;
        let item = geometry.items.borrow().get(&open.index).copied()?;
        let ui = Theme::ui(cx);
        let last = items.len().saturating_sub(1);
        let index = open.index;
        let rows: Vec<AnyElement> = items
            .iter()
            .enumerate()
            .filter(|(child, item)| !(matches!(item, Item::Separator) && *child == last))
            .map(|(child, item)| match item {
                Item::Entry {
                    label,
                    action,
                    enabled,
                } => self
                    .render_entry(
                        item_row(
                            ("context-submenu-item", child),
                            *enabled,
                            open.selected == Some(child),
                            cx,
                        ),
                        label,
                        action.as_ref(),
                        *enabled,
                        window,
                        cx,
                    )
                    .when(*enabled, |row| {
                        row.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.run_child(index, child, window, cx)
                        }))
                    })
                    .into_any_element(),
                // One level: a submenu inside a submenu is not offered.
                Item::Separator | Item::Submenu { .. } => {
                    ui::divider(ui).mx_1p5().my_1().into_any_element()
                }
            })
            .collect();
        // The child's width as drawn last time (the same submenu), else as its labels suggest.
        let width = match geometry.child.get() {
            Some((drawn, bounds)) if drawn == index => bounds.size.width,
            _ => px(estimated_width(items)),
        };
        let origin = child_origin(panel, item, width, window.viewport_size().width);
        let recorded = self.geometry.clone();
        Some(
            anchored()
                .position(origin)
                .snap_to_window_with_margin(px(popup::WINDOW_MARGIN))
                .child(
                    popup::panel(ui)
                        .id("context-submenu")
                        .occlude()
                        .min_w(px(MENU_MIN_WIDTH))
                        .p(px(MENU_PADDING))
                        .flex()
                        .flex_col()
                        .on_hover(cx.listener(|this, hovered: &bool, _, _| this.hover_child(*hovered)))
                        .child(
                            canvas(
                                move |bounds, _, _| recorded.child.set(Some((index, bounds))),
                                |_, _, _, _| {},
                            )
                            .absolute()
                            .size_full(),
                        )
                        .children(rows),
                )
                .into_any_element(),
        )
    }
}

/// The next (`forward`) or previous selectable item after `from`, around the end.
fn step(items: &[Item], from: Option<usize>, forward: bool) -> Option<usize> {
    let selectable: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.selectable())
        .map(|(index, _)| index)
        .collect();
    if forward {
        selectable
            .iter()
            .find(|&&index| from.is_none_or(|from| index > from))
            .or(selectable.first())
            .copied()
    } else {
        selectable
            .iter()
            .rev()
            .find(|&&index| from.is_none_or(|from| index < from))
            .or(selectable.last())
            .copied()
    }
}

/// Where a child menu `width` wide goes: to the right of the menu, level with its item (its first
/// item on the same line); to the left when the right side has no room in a window `window_width`
/// wide.
fn child_origin(
    panel: Bounds<Pixels>,
    item: Bounds<Pixels>,
    width: Pixels,
    window_width: Pixels,
) -> Point<Pixels> {
    let y = item.top() - px(MENU_PADDING) - px(1.);
    let right = panel.right() + px(SUBMENU_GAP);
    let left = panel.left() - px(SUBMENU_GAP) - width;
    let margin = px(popup::WINDOW_MARGIN);
    let x = if right + width + margin <= window_width || left < margin {
        right
    } else {
        left
    };
    point(x, y)
}

/// A child menu's width as its labels suggest (before it was drawn): the widest label and its
/// shortcut, at least the menu's minimum.
fn estimated_width(items: &[Item]) -> f32 {
    let widest = items
        .iter()
        .filter_map(|item| match item {
            Item::Entry { label, .. } => Some(label.chars().count() as f32 * 7.2 + 90.),
            _ => None,
        })
        .fold(0., f32::max);
    widest.max(MENU_MIN_WIDTH)
}

/// The row of an item: highlighted when selected (the keyboard, an open submenu), else under the
/// pointer.
fn item_row(
    id: (&'static str, usize),
    enabled: bool,
    selected: bool,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    let ui = Theme::ui(cx);
    div()
        .id(id)
        .relative()
        .h(px(ITEM_HEIGHT))
        .px_2p5()
        .flex()
        .items_center()
        .justify_between()
        .gap_6()
        .rounded(px(RADIUS_SM))
        .whitespace_nowrap()
        .text_color(if enabled {
            ui.foreground
        } else {
            ui.text_disabled
        })
        .when(enabled && selected, |item| item.bg(ui.list_selected))
        .when(enabled && !selected, |item| item.hover(|style| style.bg(ui.hover)))
        .when(enabled, |item| item.cursor_pointer())
}

impl Focusable for ContextMenu {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ContextMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let last = self.items.len().saturating_sub(1);
        let open = self.open.as_ref().map(|open| open.index);
        let items: Vec<AnyElement> = self
            .items
            .iter()
            .enumerate()
            .filter(|(index, item)| !(matches!(item, Item::Separator) && *index == last))
            .map(|(index, item)| match item {
                Item::Separator => ui::divider(ui).mx_1p5().my_1().into_any_element(),
                Item::Entry {
                    label,
                    action,
                    enabled,
                } => self
                    .render_entry(
                        item_row(
                            ("context-item", index),
                            *enabled,
                            self.selected == Some(index),
                            cx,
                        ),
                        label,
                        action.as_ref(),
                        *enabled,
                        window,
                        cx,
                    )
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        this.hover_item(index, *hovered, cx)
                    }))
                    .when(*enabled, |item| {
                        item.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.run(index, window, cx)
                        }))
                    })
                    .into_any_element(),
                Item::Submenu { label, .. } => {
                    let recorded = self.geometry.clone();
                    let highlighted = open == Some(index) || self.selected == Some(index);
                    item_row(("context-item", index), true, highlighted, cx)
                        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            this.hover_item(index, *hovered, cx)
                        }))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if this.open.as_ref().is_none_or(|open| open.index != index) {
                                this.open_submenu(index, false, cx);
                            }
                        }))
                        .child(
                            canvas(
                                move |bounds, _, _| {
                                    recorded.items.borrow_mut().insert(index, bounds);
                                },
                                |_, _, _, _| {},
                            )
                            .absolute()
                            .size_full(),
                        )
                        .child(label.clone())
                        .child(icon(IconName::ChevronRight, ui.dim).size(px(12.)))
                        .into_any_element()
                }
            })
            .collect();
        let recorded = self.geometry.clone();
        let child = self.render_child(window, cx);
        if child.is_none() {
            self.geometry.child.set(None);
        }
        let geometry = self.geometry.clone();
        div()
            .child(
                popup::panel(ui)
                    .key_context("ContextMenu")
                    .track_focus(&self.focus_handle)
                    .occlude()
                    .min_w(px(MENU_MIN_WIDTH))
                    .p(px(MENU_PADDING))
                    .flex()
                    .flex_col()
                    .on_action(cx.listener(Self::select_next))
                    .on_action(cx.listener(Self::select_previous))
                    .on_action(cx.listener(Self::select_child))
                    .on_action(cx.listener(Self::select_parent))
                    .on_action(cx.listener(Self::confirm))
                    .on_action(cx.listener(Self::cancel))
                    .on_mouse_down_out(cx.listener(move |_, event: &MouseDownEvent, _, cx| {
                        // A click in the open submenu is a click in the menu.
                        let in_child = geometry
                            .child
                            .get()
                            .is_some_and(|(_, bounds)| bounds.contains(&event.position));
                        if !in_child {
                            cx.emit(DismissEvent);
                        }
                    }))
                    .child(
                        canvas(
                            move |bounds, _, _| recorded.panel.set(Some(bounds)),
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .children(self.title.clone().map(|title| {
                        div()
                            .px_2p5()
                            .pt_1()
                            .pb_1p5()
                            .child(ui::section_label(title, ui))
                    }))
                    .children(items),
            )
            .children(child)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(labels: &[Option<&str>]) -> Vec<Item> {
        labels
            .iter()
            .map(|label| match label {
                Some(label) => Item::Entry {
                    label: SharedString::from(label.to_string()),
                    action: Box::new(Cancel),
                    enabled: !label.starts_with('-'),
                },
                None => Item::Separator,
            })
            .collect()
    }

    #[test]
    fn arrows_skip_separators_and_unavailable_items_around_the_end() {
        let items = entries(&[Some("a"), None, Some("-b"), Some("c")]);
        assert_eq!(step(&items, None, true), Some(0));
        assert_eq!(step(&items, Some(0), true), Some(3));
        assert_eq!(step(&items, Some(3), true), Some(0));
        assert_eq!(step(&items, Some(0), false), Some(3));
        assert_eq!(step(&items, None, false), Some(3));
    }

    #[test]
    fn a_child_menu_goes_right_unless_the_window_ends() {
        let panel = Bounds::new(point(px(100.), px(50.)), gpui::size(px(250.), px(300.)));
        let item = Bounds::new(point(px(105.), px(120.)), gpui::size(px(240.), px(28.)));
        let right = child_origin(panel, item, px(240.), px(1200.));
        assert_eq!(right, point(px(352.), px(114.)));
        // No room on the right: to the left of the menu.
        let far = Bounds::new(point(px(300.), px(50.)), gpui::size(px(250.), px(300.)));
        let left = child_origin(far, item, px(240.), px(560.));
        assert_eq!(left, point(px(300. - 2. - 240.), px(114.)));
        // No room on either side: right, the window shifts it in.
        let narrow = Bounds::new(point(px(20.), px(50.)), gpui::size(px(250.), px(300.)));
        assert_eq!(
            child_origin(narrow, item, px(240.), px(400.)).x,
            px(20. + 250. + 2.)
        );
    }

    #[test]
    fn empty_submenus_are_left_out_and_trailing_separators_dropped() {
        let built = Submenu::default().entry("a", Cancel).separator();
        assert_eq!(built.items.len(), 2);
        let estimated = estimated_width(&entries(&[Some("Show History for Selection")]));
        assert!(estimated >= MENU_MIN_WIDTH);
    }
}
