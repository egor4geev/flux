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
//!
//! Motion (`crate::motion`): the menu grows out of its click — the point itself, wherever the
//! window's edges pushed the menu — and a submenu out of its item's edge. A submenu is laid out
//! unseen the frame it opens, so its side is chosen by its real width and it grows right where it
//! stays; when it closes, it fades out toward that edge, taking no input. An owner that draws
//! [`ContextMenu::slot`] in every frame, open or not, also gets the menu's exit: the menu it
//! dropped fades out toward its click, taking no input.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::panic::Location;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Action, AnyElement, App, Bounds, ClickEvent, Context, DismissEvent, Element, ElementId, Entity,
    EventEmitter, FocusHandle, Focusable, GlobalElementId, InspectorElementId, KeyBinding,
    LayoutId, MouseDownEvent, Pixels, Point, Render, SharedString, Task, Window, actions, anchored,
    canvas, deferred, div, point, prelude::*, px,
};

use crate::icons::{IconName, icon};
use crate::motion::{self, Appear, Leaving};
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
        /// A switch's state, shown with a check mark (the editor's Soft-Wrap); `None` — not a
        /// switch.
        checked: Option<bool>,
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
        matches!(
            self,
            Item::Entry { enabled: true, .. } | Item::Submenu { .. }
        )
    }
}

/// Whether the items have a switch: then every entry leaves room for a check mark, so the labels
/// line up (as macOS menus do).
fn has_switches(items: &[Item]) -> bool {
    items.iter().any(|item| {
        matches!(
            item,
            Item::Entry {
                checked: Some(_),
                ..
            }
        )
    })
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
            checked: None,
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

/// A submenu as it was drawn — its item, its row selected with the keyboard, its place and the
/// point it grew out of: what its exit draws once it has closed.
#[derive(Clone, Copy)]
struct ShownChild {
    index: usize,
    selected: Option<usize>,
    origin: Point<Pixels>,
    anchor: Point<Pixels>,
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
    /// The submenu drawn in the last frame…
    shown_child: Option<ShownChild>,
    /// …and the one that closed, fading out.
    leaving_child: Option<Leaving<ShownChild>>,
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
            shown_child: None,
            leaving_child: None,
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
            checked: None,
        });
        self
    }

    /// A switch: the label with a check mark while `checked` (the editor's Soft-Wrap).
    pub fn check_entry(
        mut self,
        label: impl Into<SharedString>,
        checked: bool,
        action: impl Action,
    ) -> Self {
        self.items.push(Item::Entry {
            label: label.into(),
            action: Box::new(action),
            enabled: true,
            checked: Some(checked),
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
    /// it is shifted inward, keeping the margin every popup keeps. It grows out of the point.
    pub fn overlay(menu: &Entity<Self>, position: Point<Pixels>) -> AnyElement {
        let view = Appear::pop((0., 0.)).apply_from(
            ("context-menu", menu.entity_id().as_u64()),
            position,
            div().child(menu.clone()),
        );
        place(position, view)
    }

    /// [`Self::overlay`] for an owner that draws it in every frame, open or not —
    /// `ContextMenu::slot("tree-menu", self.menu.as_ref().map(|open| (&open.menu, open.position)))`:
    /// the menu it dropped (closed, or replaced by another one) fades out toward its click, taking
    /// no input, while the owner has already let it go with its subscriptions.
    pub fn slot(
        id: impl Into<ElementId>,
        open: Option<(&Entity<Self>, Point<Pixels>)>,
    ) -> AnyElement {
        MenuSlot {
            id: id.into(),
            open: open.map(|(menu, position)| (menu.clone(), position)),
        }
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
        let index = self.open.as_ref().map(|open| open.index).or(self.selected);
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

    fn run_child(
        &mut self,
        index: usize,
        child: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    /// The action's shortcut as macOS symbols, in the key context of whoever opened the menu
    /// (without one, the window's own keys — [`ui::action_binding`]).
    fn keys_for(&self, action: &dyn Action, window: &Window, cx: &App) -> Option<SharedString> {
        let binding = match &self.previous_focus {
            Some(focus) => window
                .bindings_for_action_in(action, focus)
                .first()
                .cloned(),
            None => ui::action_binding(action, cx),
        };
        binding.map(|binding| ui::shortcut_label(&binding))
    }

    /// An entry in its row ([`item_row`]): the label and the shortcut. In a menu with switches
    /// (`check_room`), the label has room for a check mark before it.
    #[allow(clippy::too_many_arguments)]
    fn render_entry(
        &self,
        row: gpui::Stateful<gpui::Div>,
        label: &SharedString,
        action: &dyn Action,
        enabled: bool,
        checked: Option<bool>,
        check_room: bool,
        window: &Window,
        cx: &App,
    ) -> gpui::Stateful<gpui::Div> {
        let ui = Theme::ui(cx);
        let keys = self.keys_for(action, window, cx);
        let label = if check_room {
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(
                    div()
                        .flex_none()
                        .w(px(14.))
                        .when(checked == Some(true), |slot| {
                            slot.child(icon(IconName::Check, ui.accent_text).size(px(14.)))
                        }),
                )
                .child(label.clone())
                .into_any_element()
        } else {
            label.clone().into_any_element()
        };
        row.child(label)
            .children(keys.map(|keys| ui::keys(&keys, ui).when(!enabled, |keys| keys.opacity(0.5))))
    }

    /// The rows of the submenu of item `index`, `selected` the row the keyboard is on; `live` —
    /// the open one, taking clicks, else one fading out (rows of its own, so the two never share
    /// an element's state).
    fn child_rows(
        &self,
        index: usize,
        selected: Option<usize>,
        live: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(Item::Submenu { items, .. }) = self.items.get(index) else {
            return Vec::new();
        };
        let ui = Theme::ui(cx);
        let last = items.len().saturating_sub(1);
        let check_room = has_switches(items);
        let id = if live {
            "context-submenu-item"
        } else {
            "context-submenu-leaving-item"
        };
        items
            .iter()
            .enumerate()
            .filter(|(child, item)| !(matches!(item, Item::Separator) && *child == last))
            .map(|(child, item)| match item {
                Item::Entry {
                    label,
                    action,
                    enabled,
                    checked,
                } => self
                    .render_entry(
                        item_row((id, child), *enabled, selected == Some(child), cx),
                        label,
                        action.as_ref(),
                        *enabled,
                        *checked,
                        check_room,
                        window,
                        cx,
                    )
                    .when(live && *enabled, |row| {
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
            .collect()
    }

    /// The panel of the submenu of item `index` with its rows ([`Self::child_rows`]).
    fn child_panel(
        &self,
        index: usize,
        selected: Option<usize>,
        live: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        popup::panel(Theme::ui(cx))
            .id(if live {
                "context-submenu"
            } else {
                "context-submenu-leaving"
            })
            .min_w(px(MENU_MIN_WIDTH))
            .p(px(MENU_PADDING))
            .flex()
            .flex_col()
            .children(self.child_rows(index, selected, live, window, cx))
    }

    /// The open submenu, next to its item: to the right of the menu, or to its left when the
    /// window has no room there. The side needs its real width: the frame it opens it is laid out
    /// unseen, taking no input, to be measured; the next frame it grows out of its item's edge
    /// right where it stays (as it was, it was placed by a guessed width and moved, or even
    /// changed sides, a frame into its pop).
    fn render_child(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.open.as_ref()?;
        let (index, selected) = (open.index, open.selected);
        self.child_items()?;
        let geometry = self.geometry.clone();
        let panel = geometry.panel.get()?;
        let item = geometry.items.borrow().get(&index).copied()?;
        let width = match geometry.child.get() {
            Some((drawn, bounds)) if drawn == index => Some(bounds.size.width),
            _ => None,
        };
        let recorded = geometry.clone();
        let view = self
            .child_panel(index, selected, true, window, cx)
            .occlude()
            .on_hover(cx.listener(|this, hovered: &bool, _, _| this.hover_child(*hovered)))
            .child(
                canvas(
                    move |bounds, _, _| recorded.child.set(Some((index, bounds))),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            );
        let place = anchored().snap_to_window_with_margin(px(popup::WINDOW_MARGIN));
        let Some(width) = width else {
            // Measured this frame, shown the next.
            window.request_animation_frame();
            return Some(
                place
                    .position(point(panel.right(), item.top()))
                    .child(motion::inert(view.invisible()))
                    .into_any_element(),
            );
        };
        let origin = child_origin(panel, item, width, window.viewport_size().width);
        // It grows out of its item's edge: the menu's right edge, or its left one when the window
        // sent the child to the left.
        let edge = if origin.x >= panel.right() {
            panel.right()
        } else {
            panel.left()
        };
        let anchor = point(edge, item.center().y);
        self.shown_child = Some(ShownChild {
            index,
            selected,
            origin,
            anchor,
        });
        Some(
            place
                .position(origin)
                .child(Appear::pop((0., 0.)).apply_from(
                    ("context-submenu-pop", index),
                    anchor,
                    view,
                ))
                .into_any_element(),
        )
    }

    /// The submenu that closed (or gave way to another one), fading out toward the edge it grew
    /// out of, taking no input.
    fn render_leaving_child(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let open = self.open.as_ref().map(|open| open.index);
        if let Some(shown) = self.shown_child.take_if(|shown| Some(shown.index) != open) {
            self.leaving_child = motion::enabled().then(|| Leaving::new(shown));
        }
        if self.leaving_child.as_ref().is_some_and(Leaving::done) {
            self.leaving_child = None;
        }
        let leaving = self.leaving_child.as_ref()?;
        let (shown, progress) = (leaving.value, leaving.progress(window));
        let view = self.child_panel(shown.index, shown.selected, false, window, cx);
        Some(
            anchored()
                .position(shown.origin)
                .snap_to_window_with_margin(px(popup::WINDOW_MARGIN))
                .child(motion::inert(motion::leaving_from(
                    view,
                    progress,
                    shown.anchor,
                )))
                .into_any_element(),
        )
    }
}

/// A menu's place over the window: at `position`, shifted inward near the window's edges.
fn place(position: Point<Pixels>, view: impl IntoElement) -> AnyElement {
    deferred(
        anchored()
            .position(position)
            .snap_to_window_with_margin(px(popup::WINDOW_MARGIN))
            .child(view),
    )
    .with_priority(1)
    .into_any_element()
}

/// A menu and its click.
type Shown = (Entity<ContextMenu>, Point<Pixels>);

/// The element of [`ContextMenu::slot`]: the open menu, and the one that left fading out.
struct MenuSlot {
    id: ElementId,
    open: Option<Shown>,
}

/// What a slot remembers between frames: the menu it drew, and the one leaving.
#[derive(Default)]
struct SlotState {
    shown: Option<Shown>,
    leaving: Option<Leaving<Shown>>,
}

impl IntoElement for MenuSlot {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for MenuSlot {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, AnyElement) {
        let open = self.open.take();
        let leaving = window.with_element_state(
            global_id.expect("a menu slot has an id"),
            |state: Option<SlotState>, window| {
                let mut state = state.unwrap_or_default();
                let gone = state
                    .shown
                    .as_ref()
                    .is_some_and(|(shown, _)| open.as_ref().is_none_or(|(menu, _)| menu != shown));
                if gone {
                    let shown = state.shown.take();
                    state.leaving = shown.filter(|_| motion::enabled()).map(Leaving::new);
                }
                if state.leaving.as_ref().is_some_and(Leaving::done) {
                    state.leaving = None;
                }
                state.shown = open.clone();
                let leaving = state.leaving.as_ref().map(|leaving| {
                    let (menu, position) = leaving.value.clone();
                    (menu, position, leaving.progress(window))
                });
                (leaving, state)
            },
        );
        let leaving = leaving.map(|(menu, position, progress)| {
            let view = motion::leaving_from(div().child(menu), progress, position);
            place(position, motion::inert(view))
        });
        let open = open.map(|(menu, position)| ContextMenu::overlay(&menu, position));
        // Out of the owner's flow: the menus are drawn over the window anyway.
        let mut element = div()
            .absolute()
            .children(leaving)
            .children(open)
            .into_any_element();
        let layout_id = element.request_layout(window, cx);
        (layout_id, element)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        element: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        element: &mut AnyElement,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
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
        .when(enabled && !selected, |item| {
            item.hover(|style| style.bg(ui.hover))
        })
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
        let check_room = has_switches(&self.items);
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
                    checked,
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
                        *checked,
                        check_room,
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
                        .child(match check_room {
                            true => div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(div().flex_none().w(px(14.)))
                                .child(label.clone())
                                .into_any_element(),
                            false => label.clone().into_any_element(),
                        })
                        .child(icon(IconName::ChevronRight, ui.dim).size(px(12.)))
                        .into_any_element()
                }
            })
            .collect();
        let recorded = self.geometry.clone();
        let leaving_child = self.render_leaving_child(window, cx);
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
            .children(leaving_child)
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
                    checked: None,
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
    }
}
