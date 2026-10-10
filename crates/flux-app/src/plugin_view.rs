//! A plugin's tool window in the island on the right (stage 8, ADR-029): the plugin describes its
//! content as elements (`flux_plugin::api::ui::View`: a toolbar, a tree, text, Markdown, buttons,
//! fields, switches), and this view draws them in the design system — plugins look like Flux.
//! What the user does comes back to the plugin as [`PluginViewEvent::Input`]. The elements keep
//! their state between views by id: a field's text, the scroll, the expanded and selected rows.
//!
//! The window looks like the Notifications window: the title and the hide button in the header,
//! the width shared by the island's windows and dragged by its left edge. Text, fields and buttons
//! line up with the title; a tree spans the island like the file tree and takes the height left.
//!
//! Keys (context "PluginView"): ↑/↓, Home/End, PgUp/PgDn select rows of the tree, → expands a row
//! (or goes to its first child), ← collapses it (or goes to its parent), ↵ opens it; ↵ in a field
//! submits it; Esc returns to the editor, ⇧Esc hides the window.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::time::{Duration, Instant};

use flux_plugin::api::events::{UiEvent, UiInput};
use flux_plugin::api::ui::{
    ButtonSpec, Element, ElementKind, Span, TextFieldSpec, ToggleSpec, ToggleStyle, Tone, TreeRow,
    TreeSpec, View,
};
use gpui::{
    AnyElement, App, ClickEvent, Context, CursorStyle, DragMoveEvent, ElementId, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, KeyBinding, MouseButton,
    MouseDownEvent, Render, ScrollHandle, ScrollStrategy, SharedString, StyledText, Subscription,
    Task, TextRun, UniformListScrollHandle, Window, actions, div, font, prelude::*, px, relative,
    uniform_list,
};

use crate::i18n::tr;
use crate::icons::{IconName, icon, plugin_icon};
use crate::input::{InputEvent, TextInput};
use crate::markdown::{self, Block};
use crate::notifications::{Progress, progress_bar};
use crate::notifications_panel::{FocusEditor, Hide};
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, CheckState, GAP, RADIUS_SM};

actions!(
    plugin_view,
    [
        SelectNext,
        SelectPrevious,
        SelectFirst,
        SelectLast,
        SelectNextPage,
        SelectPreviousPage,
        /// →: expands the selected row, or goes to its first child.
        Expand,
        /// ←: collapses the selected row, or goes to its parent.
        Collapse,
        /// ↵: opens the selected row (`activated`).
        Activate,
        /// ↵ in a text field (`submitted`).
        SubmitField,
    ]
);

const CONTEXT: &str = "PluginView";
/// A text field of the view hands ↵ over to its element.
const FIELD_CONTEXT: &str = "PluginViewField";
const HEADER_HEIGHT: f32 = 40.;
/// Rows of a tree, as in the file tree: inset from the island's edges, the highlight is a rounded
/// box inside the row.
const ROW_HEIGHT: f32 = 26.;
const ROW_INSET: f32 = 6.;
const ROW_PADDING: f32 = 6.;
const INDENT: f32 = 14.;
const CHEVRON_WIDTH: f32 = 16.;
const CHEVRON_SIZE: f32 = 12.;
const ICON_GAP: f32 = 2.;
const LABEL_GAP: f32 = 6.;
const TEXT_SIZE: f32 = 13.;
/// Text, fields and buttons line up with the title.
const CONTENT_INSET: f32 = ROW_INSET + 8.;
/// The toolbar: a row of buttons under the title.
const TOOLBAR_HEIGHT: f32 = 32.;
/// Between the blocks of a column.
const BLOCK_GAP: f32 = 6.;
/// Elements nested deeper are not drawn: a broken view can't take the window down.
const MAX_DEPTH: usize = 32;
const RESIZE_HANDLE_WIDTH: f32 = GAP;
const RESIZE_HANDLE_OFFSET: f32 = 1. + RESIZE_HANDLE_WIDTH / 2.;
/// Row hover group: the box is highlighted when the mouse is over the row, including its insets.
const ROW_GROUP: &str = "plugin-tree-row";
const DEFAULT_PAGE_ROWS: usize = 20;

pub fn init(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("home", SelectFirst, context),
        KeyBinding::new("end", SelectLast, context),
        KeyBinding::new("pagedown", SelectNextPage, context),
        KeyBinding::new("pageup", SelectPreviousPage, context),
        KeyBinding::new("right", Expand, context),
        KeyBinding::new("left", Collapse, context),
        KeyBinding::new("enter", Activate, context),
        // The island on the right handles them for whichever window it shows.
        KeyBinding::new("escape", FocusEditor, context),
        KeyBinding::new("shift-escape", Hide, context),
    ]);
    cx.bind_keys([KeyBinding::new("enter", SubmitField, Some(FIELD_CONTEXT))]);
}

pub enum PluginViewEvent {
    /// The user did something with an element.
    Input(UiInput),
    /// The window's hide button, ⇧Esc.
    #[allow(dead_code)] // The island hides the window itself (`notifications_panel::Hide`).
    Hide,
}

pub struct PluginView {
    plugin: SharedString,
    window: SharedString,
    title: SharedString,
    /// The plugin's last view; the rows of its trees live in [`Self::trees`].
    view: Option<View>,
    width: ui::RightIslandWidth,
    visible: bool,
    focus_handle: FocusHandle,
    /// The width handle is being dragged.
    resizing: bool,
    /// Text fields by element id.
    fields: HashMap<String, Field>,
    /// Trees by element id.
    trees: HashMap<String, TreeState>,
    /// The tree the keys work on: the first of the view, or the one clicked last.
    active_tree: Option<String>,
    /// Markdown by element id, parsed (code blocks highlighted in the background).
    markdown: HashMap<String, Markdown>,
    /// Toggles the user flipped, until the plugin's next view says how they are.
    toggled: HashMap<String, bool>,
    /// Elements with a tree inside (by index in the view): they take the height left.
    holds_tree: Vec<bool>,
    /// The content without trees scrolls as a whole.
    scroll: ScrollHandle,
    /// When a click on a row without children last opened it: the plugin's answer
    /// (`editors.open`) leaves the keyboard here, as the file tree's single click does.
    click_opened: Option<Instant>,
}

/// How long after a click its `activated` counts as the reason of the plugin's `editors.open`.
const CLICK_OPEN_WINDOW: Duration = Duration::from_secs(2);

/// A text field of the view.
struct Field {
    input: Entity<TextInput>,
    /// The text the plugin last gave: a new view replaces what the user typed only when the
    /// plugin's text changes.
    plugin_text: String,
    /// The text the view itself put into the field: its `Changed` is not the user's.
    echo: Option<String>,
    _subscription: Subscription,
}

struct Markdown {
    source: String,
    blocks: Vec<Block>,
    _highlight: Task<()>,
}

impl EventEmitter<PluginViewEvent> for PluginView {}

impl PluginView {
    /// The tool window `window` of the plugin `plugin`, empty until the plugin sets a view.
    pub fn new(
        plugin: SharedString,
        window: SharedString,
        title: SharedString,
        width: ui::RightIslandWidth,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            plugin,
            window,
            title,
            view: None,
            width,
            visible: false,
            focus_handle: cx.focus_handle(),
            resizing: false,
            fields: HashMap::new(),
            trees: HashMap::new(),
            active_tree: None,
            markdown: HashMap::new(),
            toggled: HashMap::new(),
            holds_tree: Vec::new(),
            scroll: ScrollHandle::new(),
            click_opened: None,
        }
    }

    /// A click (not ↵, not a double click) has just opened a row: the file the plugin opens in
    /// answer keeps the keyboard in this window.
    pub fn opened_by_click(&self) -> bool {
        self.click_opened
            .is_some_and(|at| at.elapsed() < CLICK_OPEN_WINDOW)
    }

    /// The plugin's new content. Elements keep their state by id: what the user typed into a field
    /// (unless the plugin changes the field's text), the expanded and the selected rows of a tree,
    /// its scroll.
    pub fn set_view(&mut self, mut view: View, cx: &mut Context<Self>) {
        let mut present = HashSet::new();
        let mut first_tree = None;
        for element in &mut view.elements {
            present.insert(element.id.clone());
            match &mut element.kind {
                ElementKind::TextField(spec) => self.sync_field(&element.id, spec, cx),
                ElementKind::Tree(spec) => {
                    // The rows move into the tree's state: the stored view stays light.
                    let spec = std::mem::replace(
                        spec,
                        TreeSpec {
                            rows: Vec::new(),
                            empty_text: spec.empty_text.clone(),
                        },
                    );
                    self.trees
                        .entry(element.id.clone())
                        .or_insert_with(TreeState::new)
                        .update(spec);
                    first_tree.get_or_insert_with(|| element.id.clone());
                }
                ElementKind::Markdown(source) => self.sync_markdown(&element.id, source, cx),
                _ => {}
            }
        }
        // What the user flipped, the plugin now says.
        self.toggled.clear();
        self.fields.retain(|id, _| present.contains(id));
        self.trees.retain(|id, _| present.contains(id));
        self.markdown.retain(|id, _| present.contains(id));
        if !self
            .active_tree
            .as_ref()
            .is_some_and(|id| self.trees.contains_key(id))
        {
            self.active_tree = first_tree;
        }
        self.holds_tree = holds_tree(&view.elements);
        self.view = Some(view);
        cx.notify();
    }

    /// Shown in the island or hidden.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        cx.notify();
    }

    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.contains_focused(window, cx)
    }

    fn emit(&self, element: &str, event: UiEvent, cx: &mut Context<Self>) {
        cx.emit(PluginViewEvent::Input(UiInput {
            window: self.window.to_string(),
            element: element.to_string(),
            event,
        }));
    }

    // --- Fields ---

    fn sync_field(&mut self, id: &str, spec: &TextFieldSpec, cx: &mut Context<Self>) {
        if let Some(field) = self.fields.get_mut(id) {
            if field.plugin_text != spec.text {
                field.plugin_text = spec.text.clone();
                field.echo = Some(spec.text.clone());
                field
                    .input
                    .update(cx, |input, cx| input.set_text(&spec.text, cx));
            }
            return;
        }
        let placeholder: SharedString = spec.placeholder.clone().unwrap_or_default().into();
        let code = spec.code;
        let input = cx.new(|cx| {
            let input = TextInput::new(placeholder, cx);
            if code { input.code() } else { input }
        });
        input.update(cx, |input, cx| input.set_text(&spec.text, cx));
        let element = id.to_string();
        let subscription = cx.subscribe(&input, move |this, input, _: &InputEvent, cx| {
            let text = input.read(cx).text();
            let Some(field) = this.fields.get_mut(&element) else {
                return;
            };
            if field.echo.take().is_some_and(|echo| echo == text) {
                return;
            }
            this.emit(&element, UiEvent::Changed(text), cx);
        });
        self.fields.insert(
            id.to_string(),
            Field {
                input,
                plugin_text: spec.text.clone(),
                echo: Some(spec.text.clone()),
                _subscription: subscription,
            },
        );
    }

    fn submit_field(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(field) = self.fields.get(id) {
            let text = field.input.read(cx).text();
            self.emit(id, UiEvent::Submitted(text), cx);
        }
    }

    // --- Markdown ---

    fn sync_markdown(&mut self, id: &str, source: &str, cx: &mut Context<Self>) {
        if self
            .markdown
            .get(id)
            .is_some_and(|markdown| markdown.source == source)
        {
            return;
        }
        // Drawn at once as plain Markdown; code blocks get their colors in the background.
        let blocks = markdown::parse(source);
        let scopes = markdown::scopes(cx);
        let (element, text) = (id.to_string(), source.to_string());
        let mut highlighted = blocks.clone();
        let highlight = cx.spawn(async move |this, cx| {
            let blocks = cx
                .background_executor()
                .spawn(async move {
                    markdown::highlight(&mut highlighted, None, &scopes);
                    highlighted
                })
                .await;
            this.update(cx, |this, cx| {
                if let Some(markdown) = this.markdown.get_mut(&element)
                    && markdown.source == text
                {
                    markdown.blocks = blocks;
                    cx.notify();
                }
            })
            .ok();
        });
        self.markdown.insert(
            id.to_string(),
            Markdown {
                source: source.to_string(),
                blocks,
                _highlight: highlight,
            },
        );
    }

    // --- Trees ---

    fn active_tree_mut(&mut self) -> Option<(String, &mut TreeState)> {
        let id = self.active_tree.clone()?;
        let tree = self.trees.get_mut(&id)?;
        Some((id, tree))
    }

    /// Moves the selection of the active tree; down, the row ends up at the bottom edge, up, at
    /// the top one.
    fn move_selection(
        &mut self,
        to: impl FnOnce(Option<usize>, usize) -> usize,
        cx: &mut Context<Self>,
    ) {
        let Some((id, tree)) = self.active_tree_mut() else {
            return;
        };
        if tree.visible.is_empty() {
            return;
        }
        let current = tree.selected_index();
        let index = to(current, tree.visible.len()).min(tree.visible.len() - 1);
        let strategy = match current {
            Some(current) if index < current => ScrollStrategy::Top,
            _ => ScrollStrategy::Bottom,
        };
        let selected = tree.select(index);
        tree.scroll.scroll_to_item(index, strategy);
        if let Some(key) = selected {
            self.emit(&id, UiEvent::Selected(key), cx);
        }
        cx.notify();
    }

    fn page_rows(&self) -> usize {
        let Some(tree) = self.active_tree.as_ref().and_then(|id| self.trees.get(id)) else {
            return DEFAULT_PAGE_ROWS;
        };
        let height = f32::from(tree.scroll.0.borrow().base_handle.bounds().size.height);
        if height <= 0. {
            return DEFAULT_PAGE_ROWS;
        }
        ((height / ROW_HEIGHT) as usize).saturating_sub(1).max(1)
    }

    fn expand(&mut self, _: &Expand, _: &mut Window, cx: &mut Context<Self>) {
        let Some((id, tree)) = self.active_tree_mut() else {
            return;
        };
        match tree.expand_selected() {
            TreeMove::Toggled(key, expanded) => {
                self.emit(&id, UiEvent::Expanded((key, expanded)), cx)
            }
            TreeMove::Selected(index, key) => {
                tree.scroll.scroll_to_item(index, ScrollStrategy::Bottom);
                self.emit(&id, UiEvent::Selected(key), cx)
            }
            TreeMove::None => return,
        }
        cx.notify();
    }

    fn collapse(&mut self, _: &Collapse, _: &mut Window, cx: &mut Context<Self>) {
        let Some((id, tree)) = self.active_tree_mut() else {
            return;
        };
        match tree.collapse_selected() {
            TreeMove::Toggled(key, expanded) => {
                self.emit(&id, UiEvent::Expanded((key, expanded)), cx)
            }
            TreeMove::Selected(index, key) => {
                tree.scroll.scroll_to_item(index, ScrollStrategy::Top);
                self.emit(&id, UiEvent::Selected(key), cx)
            }
            TreeMove::None => return,
        }
        cx.notify();
    }

    fn activate(&mut self, _: &Activate, _: &mut Window, cx: &mut Context<Self>) {
        let Some((id, tree)) = self.active_tree_mut() else {
            return;
        };
        if let Some(key) = tree.selected.clone() {
            self.click_opened = None;
            self.emit(&id, UiEvent::Activated(key), cx);
        }
    }

    /// A click on a row: selects it; a row without children opens with the first click, a row
    /// with children expands or collapses with a double click.
    fn click_row(
        &mut self,
        tree_id: &str,
        index: usize,
        click_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.active_tree = Some(tree_id.to_string());
        let Some(tree) = self.trees.get_mut(tree_id) else {
            return;
        };
        let Some(visible) = tree.visible.get(index).copied() else {
            return;
        };
        let key = tree.rows[visible.row].key.clone();
        let selected = tree.select(index);
        let toggled = (click_count >= 2 && visible.has_children).then(|| tree.toggle(visible.row));
        if let Some(key) = selected {
            self.emit(tree_id, UiEvent::Selected(key), cx);
        }
        if let Some(expanded) = toggled {
            self.emit(tree_id, UiEvent::Expanded((key, expanded)), cx);
        } else if click_count == 1 && !visible.has_children {
            self.click_opened = Some(Instant::now());
            self.emit(tree_id, UiEvent::Activated(key), cx);
        }
        cx.notify();
    }

    /// The chevron: expands or collapses the row, without selecting it.
    fn click_chevron(
        &mut self,
        tree_id: &str,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.active_tree = Some(tree_id.to_string());
        let Some(tree) = self.trees.get_mut(tree_id) else {
            return;
        };
        let Some(visible) = tree.visible.get(index).copied() else {
            return;
        };
        let key = tree.rows[visible.row].key.clone();
        let expanded = tree.toggle(visible.row);
        self.emit(tree_id, UiEvent::Expanded((key, expanded)), cx);
        cx.notify();
    }

    fn render_tree_rows(
        &mut self,
        tree_id: &str,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let ui = Theme::ui(cx);
        let focused =
            self.focus_handle.is_focused(window) && self.active_tree.as_deref() == Some(tree_id);
        let Some(tree) = self.trees.get(tree_id) else {
            return Vec::new();
        };
        range
            .filter_map(|index| {
                let visible = tree.visible.get(index).copied()?;
                let row = &tree.rows[visible.row];
                let selected = tree.selected.as_ref() == Some(&row.key);
                let expanded = visible.has_children && tree.is_expanded(visible.row);
                Some(self.render_tree_row(
                    tree_id, index, visible, row, selected, expanded, focused, ui, cx,
                ))
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_tree_row(
        &self,
        tree_id: &str,
        index: usize,
        visible: Visible,
        row: &TreeRow,
        selected: bool,
        expanded: bool,
        focused: bool,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row_icon = row
            .icon
            .as_deref()
            .and_then(|name| plugin_icon(&self.plugin, name, &ui));
        let chevron = {
            let tree = tree_id.to_string();
            let glyph = visible.has_children.then_some(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            });
            div()
                .flex_none()
                .w(px(CHEVRON_WIDTH))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .when(visible.has_children, |chevron| {
                    chevron.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.click_chevron(&tree, index, window, cx)
                        }),
                    )
                })
                .children(glyph.map(|glyph| icon(glyph, ui.dim).size(px(CHEVRON_SIZE))))
        };
        let body = div()
            .size_full()
            .flex()
            .items_center()
            .pr_2()
            .rounded(px(RADIUS_SM))
            .child(indent_guides(visible.depth, ui))
            .child(chevron)
            .children(row_icon.map(|row_icon| row_icon.render(ui.text_muted).ml(px(ICON_GAP))))
            .child(
                div()
                    .ml(px(LABEL_GAP))
                    .min_w_0()
                    .flex_shrink()
                    .truncate()
                    .child(styled(&row.label, ui, selected, true)),
            )
            // A line number keeps its width; a long path gives way, but not past half of the row.
            .children(row.detail.clone().map(|detail| {
                div()
                    .ml(px(LABEL_GAP))
                    .flex_none()
                    .max_w(relative(0.5))
                    .truncate()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(if selected { ui.text_muted } else { ui.dim })
                    .child(single_line(&detail))
            }))
            .child(div().flex_1().min_w(px(LABEL_GAP)))
            .children(
                row.badge
                    .clone()
                    .map(|badge| ui::badge(single_line(&badge), ui.text_muted)),
            );
        let body = if selected {
            body.bg(if focused {
                ui.list_selected
            } else {
                ui.list_selected_inactive
            })
        } else {
            body.group_hover(ROW_GROUP, move |style| style.bg(ui.hover))
        };
        let tree = tree_id.to_string();
        div()
            .id(("plugin-tree-row", index))
            .group(ROW_GROUP)
            .h(px(ROW_HEIGHT))
            .w_full()
            .px(px(ROW_INSET))
            .whitespace_nowrap()
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.click_row(&tree, index, event.click_count(), window, cx)
            }))
            .child(body)
            .into_any_element()
    }

    // --- Toggles and buttons ---

    fn flip(&mut self, id: &str, on: bool, cx: &mut Context<Self>) {
        self.toggled.insert(id.to_string(), on);
        self.emit(id, UiEvent::Toggled(on), cx);
        cx.notify();
    }

    // --- Drawing ---

    fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        div()
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .pl(px(CONTENT_INSET))
            .pr(px(ROW_INSET))
            .flex()
            .items_center()
            .gap_0p5()
            .child(
                ui::section_label(self.title.clone(), ui)
                    .flex_1()
                    .min_w_0()
                    .truncate(),
            )
            .child(
                ui::icon_button("plugin-view-hide", IconName::ChevronRight, ui)
                    .tooltip(ui::tooltip(
                        tr("Hide"),
                        ui::shortcut_in(&Hide, &self.focus_handle, window),
                    ))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(Hide), cx)),
            )
    }

    /// The view's content: the root and what it holds.
    fn render_content(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let Some(view) = &self.view else {
            return empty_state(tr("Nothing to show"), ui).into_any_element();
        };
        if view.elements.is_empty() {
            return empty_state(tr("Nothing to show"), ui).into_any_element();
        }
        let mut drawn = HashSet::new();
        let root = self.render_element(&view.elements, 0, Parent::Root, 0, &mut drawn, cx);
        if self.holds_tree.first().copied().unwrap_or(false) {
            // A tree scrolls by itself and takes the height left.
            div()
                .size_full()
                .flex()
                .flex_col()
                .pt_1()
                .child(root)
                .into_any_element()
        } else {
            div()
                .id("plugin-view-content")
                .size_full()
                .flex()
                .flex_col()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .pt_1()
                .pb_3()
                .child(root)
                .into_any_element()
        }
    }

    /// An element and its children. Each element is drawn once and no deeper than
    /// [`MAX_DEPTH`]: a view with a cycle in it stays harmless.
    fn render_element(
        &self,
        elements: &[Element],
        index: usize,
        parent: Parent,
        depth: usize,
        drawn: &mut HashSet<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let Some(element) = elements.get(index) else {
            return div().into_any_element();
        };
        if depth > MAX_DEPTH || !drawn.insert(index) {
            return div().into_any_element();
        }
        let holds_tree = self.holds_tree.get(index).copied().unwrap_or(false);
        let mut children = |cx: &mut Context<Self>, parent: Parent| -> Vec<AnyElement> {
            element
                .children
                .iter()
                .map(|&child| {
                    self.render_element(elements, child as usize, parent, depth + 1, drawn, cx)
                })
                .collect()
        };
        let id = element.id.as_str();
        let drawn_element = match &element.kind {
            ElementKind::Column => div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(BLOCK_GAP))
                .when(holds_tree, |column| column.flex_1().min_h_0())
                .children(children(cx, Parent::Column))
                .into_any_element(),
            ElementKind::Row => div()
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .children(children(cx, Parent::Row))
                .into_any_element(),
            ElementKind::Toolbar => div()
                .flex_none()
                .w_full()
                .h(px(TOOLBAR_HEIGHT))
                .px(px(ROW_INSET))
                .flex()
                .items_center()
                .gap_0p5()
                .children(children(cx, Parent::Row))
                .into_any_element(),
            ElementKind::Text(spans) => div()
                .min_w_0()
                .text_size(px(TEXT_SIZE))
                .when(parent == Parent::Row, |text| text.truncate())
                .child(styled(spans, ui, false, parent == Parent::Row))
                .into_any_element(),
            ElementKind::Markdown(_) => match self.markdown.get(id) {
                Some(markdown) => div()
                    .min_w_0()
                    .text_size(px(TEXT_SIZE))
                    .child(markdown::render(
                        &markdown.blocks,
                        ui.foreground,
                        Theme::get(cx),
                    ))
                    .into_any_element(),
                None => div().into_any_element(),
            },
            ElementKind::Button(spec) => self.render_button(id, spec, cx),
            ElementKind::TextField(_) => match self.fields.get(id) {
                Some(field) => {
                    let element = id.to_string();
                    // In a row, as the project search query: the field takes the row's width.
                    div()
                        .key_context(FIELD_CONTEXT)
                        .w_full()
                        .flex()
                        .items_center()
                        .on_action(cx.listener(move |this, _: &SubmitField, _, cx| {
                            this.submit_field(&element, cx)
                        }))
                        .child(div().flex_1().min_w_0().flex().child(field.input.clone()))
                        .into_any_element()
                }
                None => div().into_any_element(),
            },
            ElementKind::Toggle(spec) => self.render_toggle(id, spec, parent, cx),
            ElementKind::Tree(spec) => self.render_tree(id, spec, cx),
            ElementKind::Divider => match parent {
                Parent::Row => div()
                    .flex_none()
                    .w(px(1.))
                    .h(px(16.))
                    .mx_1()
                    .bg(ui.divider)
                    .into_any_element(),
                _ => ui::divider(ui).mx(px(GAP)).into_any_element(),
            },
            ElementKind::Spacer => div().flex_1().into_any_element(),
            ElementKind::Progress(fraction) => div()
                .w_full()
                .flex()
                .flex_col()
                .child(progress_bar(
                    match fraction {
                        Some(done) => Progress::Fraction(*done),
                        None => Progress::Indeterminate,
                    },
                    ui,
                ))
                .into_any_element(),
        };
        // In a column, text, fields and buttons line up with the title; a tree, a toolbar, a
        // divider span the island as in the file tree.
        let inset = parent != Parent::Row
            && matches!(
                element.kind,
                ElementKind::Text(_)
                    | ElementKind::Markdown(_)
                    | ElementKind::Button(_)
                    | ElementKind::TextField(_)
                    | ElementKind::Toggle(_)
                    | ElementKind::Progress(_)
                    | ElementKind::Row
            );
        let grows = matches!(element.kind, ElementKind::Tree(_) | ElementKind::Spacer)
            || (element.kind == ElementKind::Column && holds_tree);
        let block = div().when(inset, |block| block.px(px(CONTENT_INSET)));
        let block = if parent == Parent::Row {
            match element.kind {
                // A field takes the room left in a row; text gives way and truncates.
                _ if grows => block.flex_1(),
                ElementKind::TextField(_) => block.flex_1().min_w_0().flex().flex_col(),
                ElementKind::Text(_) | ElementKind::Markdown(_) => block.min_w_0().flex_shrink(),
                _ => block.flex_none(),
            }
        } else {
            // A flex column: the element stretches across (percentages inside a plain block
            // resolve to nothing); buttons and checkboxes keep their own width.
            let keeps_width = matches!(
                &element.kind,
                ElementKind::Button(_)
                    | ElementKind::Toggle(ToggleSpec {
                        style: ToggleStyle::Checkbox,
                        ..
                    })
            );
            let block = block
                .w_full()
                .flex()
                .flex_col()
                .when(keeps_width, |block| block.items_start());
            if grows {
                block.flex_1().min_h_0()
            } else {
                block.flex_none()
            }
        };
        block.child(drawn_element).into_any_element()
    }

    fn render_button(&self, id: &str, spec: &ButtonSpec, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let element_id = ElementId::Name(format!("plugin-button-{id}").into());
        let button_icon = spec
            .icon
            .as_deref()
            .and_then(|name| plugin_icon(&self.plugin, name, &ui));
        let label = spec.label.as_deref().map(single_line);
        let button = match (label, button_icon) {
            (Some(label), _) if spec.primary => {
                ui::primary_button(element_id, label, spec.enabled, ui)
            }
            (Some(label), Some(button_icon)) => text_button_with_icon(
                element_id,
                button_icon.render(ui.text_muted).size(px(14.)),
                label,
                ui,
            ),
            (Some(label), None) => ui::text_button(element_id, label, false, ui),
            (None, Some(button_icon)) => ui::icon_button_at(element_id, button_icon.path(), ui),
            (None, None) => ui::icon_button(element_id, IconName::More, ui),
        };
        let button = match spec.tooltip.as_deref() {
            Some(tooltip) => button.tooltip(ui::tooltip(single_line(tooltip), None)),
            None => button,
        };
        if !spec.enabled {
            return button
                .opacity(0.45)
                .cursor(CursorStyle::Arrow)
                .into_any_element();
        }
        let element = id.to_string();
        button
            .on_click(cx.listener(move |this, _, _, cx| this.emit(&element, UiEvent::Clicked, cx)))
            .into_any_element()
    }

    fn render_toggle(
        &self,
        id: &str,
        spec: &ToggleSpec,
        parent: Parent,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let on = self.toggled.get(id).copied().unwrap_or(spec.on);
        let element = id.to_string();
        let label = div()
            .min_w_0()
            .truncate()
            .text_size(px(TEXT_SIZE))
            .text_color(ui.foreground)
            .child(single_line(&spec.label));
        let row = div()
            .id(ElementId::Name(format!("plugin-toggle-{id}").into()))
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.flip(&element, !on, cx)));
        let id = ElementId::Name(format!("plugin-toggle-control-{id}").into());
        match spec.style {
            // A switch is a setting: the label on the left, the switch at the end of the line.
            ToggleStyle::Switch => row
                .when(parent != Parent::Row, |row| row.w_full())
                .child(label.flex_1())
                .child(ui::switch(id, on, ui)),
            ToggleStyle::Checkbox => row
                .child(ui::checkbox(id, CheckState::from_bool(on), ui))
                .child(label),
        }
        .into_any_element()
    }

    fn render_tree(&self, id: &str, spec: &TreeSpec, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let Some(tree) = self.trees.get(id) else {
            return div().into_any_element();
        };
        if tree.visible.is_empty() {
            let text = spec.empty_text.clone().unwrap_or_default();
            return empty_state(text, ui).into_any_element();
        }
        let tree_id = id.to_string();
        let list = uniform_list(
            ElementId::Name(format!("plugin-tree-{id}").into()),
            tree.visible.len(),
            cx.processor(move |this, range: Range<usize>, window, cx| {
                this.render_tree_rows(&tree_id, range, window, cx)
            }),
        )
        .track_scroll(tree.scroll.clone())
        .size_full();
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .pb_2()
            .child(list)
            .into_any_element()
    }
}

impl Focusable for PluginView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PluginView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        // The handle was released anywhere: the drag is over and the highlight goes away.
        self.resizing &= cx.has_active_drag();
        let content = self.render_content(cx);
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
            .text_size(px(TEXT_SIZE))
            .text_color(ui.foreground)
            // A click anywhere activates the window, as in JetBrains IDEs (the header's buttons then
            // dispatch through it). Before the children: a click into a field then focuses the
            // field itself.
            .capture_any_mouse_down(
                cx.listener(|this, _, window, _| window.focus(&this.focus_handle)),
            )
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                this.move_selection(|current, _| current.map_or(0, |i| i + 1), cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                this.move_selection(|current, _| current.map_or(0, |i| i.saturating_sub(1)), cx)
            }))
            .on_action(
                cx.listener(|this, _: &SelectFirst, _, cx| this.move_selection(|_, _| 0, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectLast, _, cx| {
                    this.move_selection(|_, len| len - 1, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &SelectNextPage, _, cx| {
                let page = this.page_rows();
                this.move_selection(|current, _| current.map_or(0, |i| i + page), cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPreviousPage, _, cx| {
                let page = this.page_rows();
                this.move_selection(
                    |current, _| current.map_or(0, |i| i.saturating_sub(page)),
                    cx,
                )
            }))
            .on_action(cx.listener(Self::expand))
            .on_action(cx.listener(Self::collapse))
            .on_action(cx.listener(Self::activate))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedEdge>, _, cx| {
                    // The handle sits in the gap to the left of the edge.
                    let width = f32::from(event.bounds.right() - event.event.position.x)
                        - RESIZE_HANDLE_OFFSET;
                    this.width.set(width);
                    this.resizing = true;
                    cx.notify();
                }),
            )
            .child(self.render_header(window, cx))
            .child(ui::divider(ui).mx(px(GAP)))
            .child(div().flex_1().min_h_0().flex().flex_col().child(content))
            .child(resize_handle(self.resizing, ui))
    }
}

#[derive(Clone)]
struct DraggedEdge;

impl Render for DraggedEdge {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// Width handle in the gap to the left of the island: an accent line on hover and while dragged.
fn resize_handle(resizing: bool, ui: UiColors) -> impl IntoElement {
    div()
        .id("plugin-view-resize")
        .group("plugin-view-resize")
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
                        .group_hover("plugin-view-resize", |style| style.visible())
                }),
        )
}

/// Where an element sits: it decides its insets and how it takes space.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Parent {
    Root,
    Column,
    Row,
}

/// The text of the middle of an empty view or tree.
fn empty_state(text: impl Into<SharedString>, ui: UiColors) -> impl IntoElement {
    div()
        .size_full()
        .p_4()
        .flex()
        .items_center()
        .justify_center()
        .text_center()
        .text_size(px(theme::TEXT_SM))
        .text_color(ui.dim)
        .child(text.into())
}

/// Nesting indent with thin guides, as in the file tree.
fn indent_guides(depth: usize, ui: UiColors) -> impl IntoElement {
    div()
        .flex_none()
        .h_full()
        .pl(px(ROW_PADDING))
        .flex()
        .children((0..depth).map(move |_| {
            div()
                .flex_none()
                .w(px(INDENT))
                .h_full()
                .pl(px(CHEVRON_WIDTH / 2. - 0.5))
                .child(div().w(px(1.)).h_full().bg(ui.divider))
        }))
}

/// [`ui::text_button`] with an icon before the label.
fn text_button_with_icon(
    id: ElementId,
    button_icon: gpui::Div,
    label: String,
    ui: UiColors,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(ui::ICON_BUTTON_SIZE))
        .pl(px(8.))
        .pr(px(10.))
        .flex()
        .items_center()
        .gap_1p5()
        .rounded(px(RADIUS_SM))
        .border_1()
        .border_color(ui.input_border)
        .text_size(px(theme::TEXT_SM))
        .text_color(ui.foreground)
        .cursor_pointer()
        .hover(move |style| style.bg(ui.hover))
        .active(move |style| style.bg(ui.pressed))
        .child(button_icon)
        .child(label)
}

/// The color of a tone.
fn tone_color(tone: Tone, ui: UiColors) -> gpui::Hsla {
    match tone {
        Tone::Normal => ui.foreground,
        Tone::Muted => ui.text_muted,
        Tone::Dim => ui.dim,
        Tone::Accent => ui.accent_text,
        Tone::Success => ui.success,
        Tone::Warning => ui.warning,
        Tone::Error => ui.error,
    }
}

/// Spans as one styled text. Highlighted spans get the background of search matches (the current
/// match's in a selected row); `one_line` turns line breaks into spaces (a row, a tree label).
fn styled(spans: &[Span], ui: UiColors, selected: bool, one_line: bool) -> StyledText {
    let mut text = String::new();
    let mut runs = Vec::with_capacity(spans.len());
    for span in spans.iter().filter(|span| !span.text.is_empty()) {
        let mut run_font = font(if span.code {
            theme::code_font()
        } else {
            theme::UI_FONT
        });
        if span.bold {
            run_font.weight = FontWeight::SEMIBOLD;
        }
        let piece = if one_line {
            single_line(&span.text)
        } else {
            span.text.clone()
        };
        runs.push(TextRun {
            len: piece.len(),
            font: run_font,
            color: tone_color(span.tone, ui),
            background_color: span.highlight.then_some(if selected {
                ui.search_match_active
            } else {
                ui.search_match
            }),
            underline: None,
            strikethrough: None,
        });
        text.push_str(&piece);
    }
    StyledText::new(text).with_runs(runs)
}

/// Line breaks and tabs as spaces: the byte length stays the same.
fn single_line(text: &str) -> String {
    text.replace(['\n', '\r', '\t'], " ")
}

/// Which elements have a tree inside them (by index), following children as drawn: each element
/// once, no deeper than [`MAX_DEPTH`].
fn holds_tree(elements: &[Element]) -> Vec<bool> {
    fn visit(
        elements: &[Element],
        index: usize,
        depth: usize,
        out: &mut Vec<Option<bool>>,
    ) -> bool {
        let Some(element) = elements.get(index) else {
            return false;
        };
        if let Some(known) = out[index] {
            return known;
        }
        if depth > MAX_DEPTH {
            return false;
        }
        // A cycle back here: not a tree on that path.
        out[index] = Some(false);
        let mut holds = matches!(element.kind, ElementKind::Tree(_));
        for &child in &element.children {
            holds |= visit(elements, child as usize, depth + 1, out);
        }
        out[index] = Some(holds);
        holds
    }
    let mut out = vec![None; elements.len()];
    if !elements.is_empty() {
        visit(elements, 0, 0, &mut out);
    }
    out.into_iter()
        .map(|known| known.unwrap_or(false))
        .collect()
}

// --- The state of a tree ---

/// A drawn row: its index in the plugin's rows, its depth, whether it has children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Visible {
    row: usize,
    depth: usize,
    has_children: bool,
}

/// What a key did to a tree.
#[derive(Debug, PartialEq, Eq)]
enum TreeMove {
    /// A row was expanded (`true`) or collapsed.
    Toggled(String, bool),
    /// The selection moved to the visible row at the index.
    Selected(usize, String),
    None,
}

/// A tree's rows and what the user did to them: expanded rows and the selection by key, the
/// scroll.
struct TreeState {
    rows: Vec<TreeRow>,
    /// Children of each row, in the plugin's order.
    children: Vec<Vec<usize>>,
    roots: Vec<usize>,
    /// The parent of each row.
    parents: Vec<Option<usize>>,
    /// What is drawn: the rows under expanded parents, depth first.
    visible: Vec<Visible>,
    /// Expanded or not, by key: the plugin's `expanded` when the key is first seen, then the
    /// user's choice.
    expanded: HashMap<String, bool>,
    selected: Option<String>,
    scroll: UniformListScrollHandle,
}

impl TreeState {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            children: Vec::new(),
            roots: Vec::new(),
            parents: Vec::new(),
            visible: Vec::new(),
            expanded: HashMap::new(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
        }
    }

    /// The plugin's new rows. A parent must come before its children; a row pointing elsewhere
    /// goes to the top level.
    fn update(&mut self, spec: TreeSpec) {
        self.rows = spec.rows;
        self.children = vec![Vec::new(); self.rows.len()];
        self.roots.clear();
        self.parents = vec![None; self.rows.len()];
        for (index, row) in self.rows.iter().enumerate() {
            match row.parent.map(|parent| parent as usize) {
                Some(parent) if parent < index => {
                    self.children[parent].push(index);
                    self.parents[index] = Some(parent);
                }
                _ => self.roots.push(index),
            }
        }
        let keys: HashSet<&str> = self.rows.iter().map(|row| row.key.as_str()).collect();
        self.expanded.retain(|key, _| keys.contains(key.as_str()));
        for row in &self.rows {
            self.expanded.entry(row.key.clone()).or_insert(row.expanded);
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|key| !keys.contains(key.as_str()))
        {
            self.selected = None;
        }
        self.recompute();
    }

    fn is_expanded(&self, row: usize) -> bool {
        self.expanded
            .get(&self.rows[row].key)
            .copied()
            .unwrap_or(self.rows[row].expanded)
    }

    /// The drawn rows, after the expansion changed.
    fn recompute(&mut self) {
        let mut visible = Vec::with_capacity(self.rows.len());
        let mut stack: Vec<(usize, usize)> = self.roots.iter().rev().map(|&row| (row, 0)).collect();
        while let Some((row, depth)) = stack.pop() {
            let has_children = !self.children[row].is_empty();
            visible.push(Visible {
                row,
                depth,
                has_children,
            });
            if has_children && self.is_expanded(row) {
                stack.extend(
                    self.children[row]
                        .iter()
                        .rev()
                        .map(|&child| (child, depth + 1)),
                );
            }
        }
        self.visible = visible;
        // A selected row hidden inside a collapsed parent: the nearest visible ancestor.
        if let Some(key) = &self.selected
            && !self.visible.iter().any(|v| &self.rows[v.row].key == key)
        {
            let mut row = self.rows.iter().position(|r| &r.key == key);
            while let Some(index) = row {
                if self.visible.iter().any(|v| v.row == index) {
                    break;
                }
                row = self.parents[index];
            }
            self.selected = row.map(|index| self.rows[index].key.clone());
        }
    }

    /// The selected row's index among the drawn ones.
    fn selected_index(&self) -> Option<usize> {
        let key = self.selected.as_ref()?;
        self.visible
            .iter()
            .position(|v| &self.rows[v.row].key == key)
    }

    /// Selects the drawn row at `index`; returns its key if the selection changed.
    fn select(&mut self, index: usize) -> Option<String> {
        let key = self.rows[self.visible.get(index)?.row].key.clone();
        if self.selected.as_ref() == Some(&key) {
            return None;
        }
        self.selected = Some(key.clone());
        Some(key)
    }

    /// Expands or collapses a row; returns whether it is expanded now.
    fn toggle(&mut self, row: usize) -> bool {
        let expanded = !self.is_expanded(row);
        self.expanded.insert(self.rows[row].key.clone(), expanded);
        self.recompute();
        expanded
    }

    /// →: a collapsed row with children expands; an expanded one hands the selection to its
    /// first child.
    fn expand_selected(&mut self) -> TreeMove {
        let Some(index) = self.selected_index() else {
            return TreeMove::None;
        };
        let visible = self.visible[index];
        if !visible.has_children {
            return TreeMove::None;
        }
        if !self.is_expanded(visible.row) {
            let key = self.rows[visible.row].key.clone();
            return TreeMove::Toggled(key, self.toggle(visible.row));
        }
        match self.select(index + 1) {
            Some(key) => TreeMove::Selected(index + 1, key),
            None => TreeMove::None,
        }
    }

    /// ←: an expanded row collapses; otherwise the selection goes to the parent.
    fn collapse_selected(&mut self) -> TreeMove {
        let Some(index) = self.selected_index() else {
            return TreeMove::None;
        };
        let visible = self.visible[index];
        if visible.has_children && self.is_expanded(visible.row) {
            let key = self.rows[visible.row].key.clone();
            return TreeMove::Toggled(key, self.toggle(visible.row));
        }
        let Some(parent) = self.parents[visible.row] else {
            return TreeMove::None;
        };
        let Some(at) = self.visible.iter().position(|v| v.row == parent) else {
            return TreeMove::None;
        };
        match self.select(at) {
            Some(key) => TreeMove::Selected(at, key),
            None => TreeMove::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_plugin::api::ui::Span;

    fn row(key: &str, parent: Option<u32>, expanded: bool) -> TreeRow {
        TreeRow {
            key: key.to_string(),
            parent,
            icon: None,
            label: vec![Span {
                text: key.to_string(),
                tone: Tone::Normal,
                bold: false,
                code: false,
                highlight: false,
            }],
            detail: None,
            badge: None,
            expanded,
        }
    }

    /// Two files with two items each; the first file expanded.
    fn spec() -> TreeSpec {
        TreeSpec {
            rows: vec![
                row("a.rs", None, true),
                row("a.rs:1", Some(0), false),
                row("a.rs:2", Some(0), false),
                row("b.rs", None, false),
                row("b.rs:7", Some(3), false),
            ],
            empty_text: None,
        }
    }

    fn keys(tree: &TreeState) -> Vec<&str> {
        tree.visible
            .iter()
            .map(|v| tree.rows[v.row].key.as_str())
            .collect()
    }

    #[test]
    fn rows_under_collapsed_parents_are_hidden() {
        let mut tree = TreeState::new();
        tree.update(spec());
        assert_eq!(keys(&tree), ["a.rs", "a.rs:1", "a.rs:2", "b.rs"]);
        assert_eq!(tree.visible[1].depth, 1);
        assert!(tree.visible[0].has_children && !tree.visible[1].has_children);
    }

    #[test]
    fn the_users_expansion_wins_over_the_plugins() {
        let mut tree = TreeState::new();
        tree.update(spec());
        assert!(!tree.toggle(0));
        assert!(tree.toggle(3));
        // The plugin sends the same rows again, with its own idea of what is expanded.
        tree.update(spec());
        assert_eq!(keys(&tree), ["a.rs", "b.rs", "b.rs:7"]);
        // A key gone and back is new again: the plugin's choice.
        let mut without_b = spec();
        without_b.rows.truncate(3);
        tree.update(without_b);
        tree.update(spec());
        assert_eq!(keys(&tree), ["a.rs", "b.rs"]);
    }

    #[test]
    fn keys_move_the_selection_and_expand() {
        let mut tree = TreeState::new();
        tree.update(spec());
        assert_eq!(tree.select(3), Some("b.rs".to_string()));
        assert_eq!(tree.select(3), None);
        assert_eq!(
            tree.expand_selected(),
            TreeMove::Toggled("b.rs".to_string(), true)
        );
        assert_eq!(
            tree.expand_selected(),
            TreeMove::Selected(4, "b.rs:7".to_string())
        );
        assert_eq!(tree.expand_selected(), TreeMove::None);
        assert_eq!(
            tree.collapse_selected(),
            TreeMove::Selected(3, "b.rs".to_string())
        );
        assert_eq!(
            tree.collapse_selected(),
            TreeMove::Toggled("b.rs".to_string(), false)
        );
        assert_eq!(tree.collapse_selected(), TreeMove::None);
    }

    #[test]
    fn a_hidden_selection_goes_to_the_visible_parent() {
        let mut tree = TreeState::new();
        tree.update(spec());
        tree.select(2);
        tree.toggle(0);
        assert_eq!(tree.selected.as_deref(), Some("a.rs"));
        // A selected row the plugin no longer sends is no longer selected.
        tree.select(1);
        tree.update(TreeSpec {
            rows: vec![row("c.rs", None, false)],
            empty_text: None,
        });
        assert_eq!(tree.selected, None);
    }

    #[test]
    fn bad_parents_go_to_the_top_level() {
        let mut tree = TreeState::new();
        tree.update(TreeSpec {
            rows: vec![
                row("self", Some(0), true),
                row("later", Some(2), true),
                row("far", Some(99), true),
            ],
            empty_text: None,
        });
        assert_eq!(keys(&tree), ["self", "later", "far"]);
        assert!(tree.visible.iter().all(|v| v.depth == 0));
    }

    fn element(id: &str, kind: ElementKind, children: Vec<u32>) -> Element {
        Element {
            id: id.to_string(),
            kind,
            children,
        }
    }

    #[test]
    fn columns_with_a_tree_take_the_height_and_cycles_are_harmless() {
        let tree = ElementKind::Tree(TreeSpec {
            rows: Vec::new(),
            empty_text: None,
        });
        let elements = vec![
            element("root", ElementKind::Column, vec![1, 2]),
            element("bar", ElementKind::Toolbar, vec![]),
            element("inner", ElementKind::Column, vec![3, 0]),
            element("items", tree, vec![]),
        ];
        assert_eq!(holds_tree(&elements), [true, false, true, true]);
        let looped = vec![element("a", ElementKind::Column, vec![0, 5])];
        assert_eq!(holds_tree(&looped), [false]);
    }

    #[test]
    fn single_line_keeps_byte_lengths() {
        let text = "a\tb\nc\r";
        assert_eq!(single_line(text), "a b c ");
        assert_eq!(single_line(text).len(), text.len());
    }
}
