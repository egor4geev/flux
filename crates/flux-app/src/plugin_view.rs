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
//! submits it — in a multi-line one (API 0.3) ↵ starts a line and ⌘↵ submits; ⇥ goes to the next
//! field; Esc returns to the editor, ⇧Esc hides the window.
//!
//! API 0.3 (part 10.3): a badge — a colored label, with ▾ a button (a status that offers its
//! transitions); where each element was drawn, for a plugin's picker opened under it. A window of
//! the manifest's `location = "editor"` is drawn as a tab among the editor's ([`Chrome::Tab`]):
//! the full width of the editor's island, no header, no resize handle — the tab names it.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use flux_plugin::api::events::{UiEvent, UiInput};
use flux_plugin::api::ui::{
    BadgeSpec, ButtonSpec, Element, ElementKind, Property, Span, TextFieldSpec, ToggleSpec,
    ToggleStyle, Tone, TreeRow, TreeSpec, View,
};
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, CursorStyle, DragMoveEvent, ElementId, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, KeyBinding, MouseButton,
    MouseDownEvent, Pixels, Render, ScrollHandle, ScrollStrategy, SharedString, StyledText,
    Subscription, Task, TextRun, UniformListScrollHandle, Window, actions, div, font, prelude::*,
    px, relative, uniform_list,
};

use crate::i18n::tr;
use crate::icons::{self, IconName, icon, plugin_icon};
use crate::input::{InputEvent, TextInput};
use crate::markdown::{self, Block};
use crate::motion::{Disclosure, unfold_style};
use crate::notifications::{Progress, progress_bar};
use crate::notifications_panel::{FocusEditor, Hide};
use crate::prompt_input::{PromptInput, PromptInputEvent};
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
        /// ↵ in a text field (`submitted`); ⌘↵ in a multi-line one.
        SubmitField,
        /// ↵ in a multi-line text field: a line break.
        NewLine,
        /// ⇥: the next text field of the view takes the keyboard (the first after the last).
        FocusNextField,
        /// ⌘↵: the first primary button of the selected row (a card's "Take to Work"):
        /// `row-action`.
        RunRowAction,
    ]
);

const CONTEXT: &str = "PluginView";
/// A text field of the view hands ↵ over to its element.
const FIELD_CONTEXT: &str = "PluginViewField";
/// A multi-line text field: ↵ is a line break, ⌘↵ submits.
const LINES_CONTEXT: &str = "PluginViewLines";
/// A multi-line field grows with its text up to this many rows, then scrolls.
const MAX_FIELD_ROWS: usize = 6;
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
/// A row's label keeps this much while there is room: the detail gives way before it.
const LABEL_MIN_WIDTH: f32 = 48.;
/// How much faster than the label a row's detail shrinks in a narrow row.
const DETAIL_SHRINK: f32 = 1000.;
const TEXT_SIZE: f32 = 13.;
/// Text, fields and buttons line up with the title.
const CONTENT_INSET: f32 = ROW_INSET + 8.;
/// The toolbar: a row of buttons under the title.
const TOOLBAR_HEIGHT: f32 = 34.;
/// Between the items of a toolbar: a dropdown, a checkbox and a button don't touch.
const TOOLBAR_GAP: f32 = 6.;
/// Between the blocks of a column, as the paragraphs of the Notifications window and the chat.
const BLOCK_GAP: f32 = 10.;
/// Between the items of a row; a row that doesn't fit wraps, its lines this far apart.
const ROW_GAP: f32 = 8.;
const ROW_LINE_GAP: f32 = 6.;
/// Around a divider in a column: the blocks' gap and this more on each side — a new section.
const DIVIDER_MARGIN: f32 = 4.;
/// A property's row (API 0.3): its least height, the vertical padding, the gap between the
/// label's column and the value, and between rows.
const PROPERTY_ROW_HEIGHT: f32 = 28.;
const PROPERTY_PADDING_Y: f32 = 4.;
const PROPERTY_COLUMN_GAP: f32 = 12.;
const PROPERTY_ROW_GAP: f32 = 1.;
/// The labels' column takes at most this share of the list's width; a longer label truncates.
const PROPERTY_LABEL_SHARE: f32 = 0.4;
/// The labels' column is at least this wide (a list of short labels still lines its values up
/// with the other lists of the window).
const PROPERTY_LABEL_MIN_WIDTH: f32 = 72.;
/// A tab's content keeps this far from the editor island's edges.
const TAB_PADDING: f32 = GAP;
/// Elements nested deeper are not drawn: a broken view can't take the window down.
const MAX_DEPTH: usize = 32;
const RESIZE_HANDLE_WIDTH: f32 = GAP;
/// Row hover group: the box is highlighted when the mouse is over the row, including its insets.
const ROW_GROUP: &str = "plugin-tree-row";
/// A card of a tree of cards (a board's column): its height with the gap under it — fixed, so the
/// list stays virtual — and its parts: the label line, two subtitle lines, the detail line.
const CARD_GAP: f32 = 8.;
const CARD_PADDING_X: f32 = 12.;
const CARD_PADDING_Y: f32 = 10.;
const CARD_LINE_HEIGHT: f32 = 20.;
const CARD_SUBTITLE_LINE: f32 = 18.;
const CARD_DETAIL_LINE: f32 = 16.;
/// A card's line of tags (`tree-row.tags`) under the subtitle — in a tree that has any.
const CARD_TAGS_GAP: f32 = 6.;
const CARD_TAGS_LINE: f32 = 18.;
const TAG_GAP: f32 = 4.;
/// A tag's text and its padding (`ui::badge`: `px_1p5`).
const TAG_TEXT_SIZE: f32 = theme::TEXT_XS;
const TAG_PADDING: f32 = 12.;
/// The buttons at a card's foot (`tree-row.actions`) — in a tree that has any — and at a list
/// row's end.
const ROW_ACTION_HEIGHT: f32 = 24.;
/// A list row shows this many tags, then "+N".
const LIST_TAGS: usize = 2;
const ROW_ACTION_GAP: f32 = 4.;

/// A card's height with the gap under it: the padding, the label line, two subtitle lines, the
/// line of tags when the tree has tags, and the foot — the detail, as tall as a button when the
/// tree has buttons. One height per tree, so the list stays virtual.
const fn card_height(tags: bool, actions: bool) -> f32 {
    let tags = if tags {
        CARD_TAGS_GAP + CARD_TAGS_LINE
    } else {
        0.
    };
    let foot = if actions {
        ROW_ACTION_HEIGHT
    } else {
        CARD_DETAIL_LINE
    };
    CARD_PADDING_Y * 2.
        + CARD_LINE_HEIGHT
        + 4.
        + CARD_SUBTITLE_LINE * 2.
        + tags
        + 6.
        + foot
        + 2.
        + CARD_GAP
}
/// A board's column (a column holding a tree of cards, in a row): a lane under its cards.
const LANE_PADDING: f32 = 6.;
/// A column of cards in a row of them is never narrower: the row scrolls sideways instead.
const CARD_COLUMN_MIN_WIDTH: f32 = 220.;
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
        KeyBinding::new("cmd-enter", RunRowAction, context),
        KeyBinding::new("tab", FocusNextField, context),
        // The island on the right handles them for whichever window it shows.
        KeyBinding::new("escape", FocusEditor, context),
        KeyBinding::new("shift-escape", Hide, context),
    ]);
    cx.bind_keys([
        KeyBinding::new("enter", SubmitField, Some(FIELD_CONTEXT)),
        KeyBinding::new("enter", NewLine, Some(LINES_CONTEXT)),
        KeyBinding::new("cmd-enter", SubmitField, Some(LINES_CONTEXT)),
    ]);
}

pub enum PluginViewEvent {
    /// The user did something with an element.
    Input(UiInput),
    /// The window's hide button, ⇧Esc.
    #[allow(dead_code)] // The island hides the window itself (`notifications_panel::Hide`).
    Hide,
}

/// How a plugin's window is framed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chrome {
    /// The island on the right: the title and Hide in the header, the island's width.
    Island,
    /// A tab among the editor's tabs (API 0.3): the editor's width, the tab shows the title.
    Tab,
}

pub struct PluginView {
    plugin: SharedString,
    window: SharedString,
    title: SharedString,
    chrome: Chrome,
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
    /// Which elements have a tree of cards inside them: a row of them scrolls sideways.
    holds_cards: Vec<bool>,
    /// The width of each properties list's labels column (by element id), measured when the view
    /// is drawn: the longest label, within [`PROPERTY_LABEL_SHARE`] of the list.
    label_widths: HashMap<String, f32>,
    /// Each property's label and value as they fit their columns, "…" at the end of a longer
    /// one (by "<list>/<property>"): gpui's own ellipsis keeps the first measure of a line that
    /// doesn't wrap, made before the width was known.
    property_texts: HashMap<String, (String, Vec<Span>)>,
    /// The content without trees scrolls as a whole.
    scroll: ScrollHandle,
    /// When a click on a row without children last opened it: the plugin's answer
    /// (`editors.open`) leaves the keyboard here, as the file tree's single click does.
    click_opened: Option<Instant>,
    /// Where each element was drawn last (by id): a plugin's picker opens under one.
    element_bounds: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
    /// The content's visible area, last drawn: an element scrolled out of it isn't an anchor.
    content_bounds: Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
    /// The field that had the keyboard when the view was last drawn (its focus ring).
    focused_field: Option<String>,
}

/// How long after a click its `activated` counts as the reason of the plugin's `editors.open`.
const CLICK_OPEN_WINDOW: Duration = Duration::from_secs(2);

/// A text field of the view.
struct Field {
    input: FieldInput,
    /// The text the plugin last gave: a new view replaces what the user typed only when the
    /// plugin's text changes.
    plugin_text: String,
    /// The text the view itself put into the field: its `Changed` is not the user's.
    echo: Option<String>,
    _subscription: Subscription,
}

/// A field's input: a line, or several lines (API 0.3).
enum FieldInput {
    Line(Entity<TextInput>),
    Lines(Entity<PromptInput>),
}

impl FieldInput {
    fn text(&self, cx: &App) -> String {
        match self {
            FieldInput::Line(input) => input.read(cx).text(),
            FieldInput::Lines(input) => input.read(cx).text(),
        }
    }

    fn set_text(&self, text: &str, cx: &mut App) {
        match self {
            FieldInput::Line(input) => input.update(cx, |input, cx| input.set_text(text, cx)),
            FieldInput::Lines(input) => input.update(cx, |input, cx| input.set_text(text, cx)),
        }
    }

    fn multiline(&self) -> bool {
        matches!(self, FieldInput::Lines(_))
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            FieldInput::Line(input) => input.focus_handle(cx),
            FieldInput::Lines(input) => input.focus_handle(cx),
        }
    }
}

struct Markdown {
    source: String,
    blocks: Vec<Block>,
    _highlight: Task<()>,
}

impl EventEmitter<PluginViewEvent> for PluginView {}

impl PluginView {
    /// The window's title (`ui.set-title`): its header; its tab, in the editor.
    pub fn set_title(&mut self, title: SharedString, cx: &mut Context<Self>) {
        self.title = title;
        cx.notify();
    }

    /// The window's title: its header, its tab.
    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    /// The tool window `window` of the plugin `plugin`, empty until the plugin sets a view.
    pub fn new(
        plugin: SharedString,
        window: SharedString,
        title: SharedString,
        width: ui::RightIslandWidth,
        chrome: Chrome,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            plugin,
            window,
            title,
            chrome,
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
            holds_cards: Vec::new(),
            label_widths: HashMap::new(),
            property_texts: HashMap::new(),
            scroll: ScrollHandle::new(),
            click_opened: None,
            element_bounds: Rc::new(RefCell::new(HashMap::new())),
            content_bounds: Rc::new(std::cell::Cell::new(None)),
            focused_field: None,
        }
    }

    /// Where the element `element` was drawn last; none while the window is hidden, the element
    /// isn't in the view or is scrolled out of sight (part 10.3: a plugin's picker under it — the
    /// window's anchor then).
    ///
    /// A property of a `properties` list is named "<list>/<property>" (API 0.3): its row, or the
    /// list when the row hasn't been drawn yet.
    pub fn element_bounds(&self, element: &str) -> Option<Bounds<Pixels>> {
        if !self.visible {
            return None;
        }
        let drawn = self.element_bounds.borrow();
        let bounds = drawn.get(element).copied().or_else(|| {
            let (list, _) = element.rsplit_once('/')?;
            drawn.get(list).copied()
        })?;
        let area = self.content_bounds.get()?;
        in_sight(bounds, area).then_some(bounds)
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
                            cards: spec.cards,
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
        self.element_bounds.borrow_mut().retain(|id, _| {
            present.contains(id)
                || id
                    .rsplit_once('/')
                    .is_some_and(|(list, _)| present.contains(list))
        });
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
        self.holds_cards = holds_cards(&view.elements);
        self.view = Some(view);
        cx.notify();
    }

    /// Shown in the island or hidden.
    /// Drawn now: the island's window shown, the tab the active one.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

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
        // A field that became multi-line (or single-line) is made anew.
        if self
            .fields
            .get(id)
            .is_some_and(|field| field.input.multiline() != spec.multiline)
        {
            self.fields.remove(id);
        }
        if let Some(field) = self.fields.get_mut(id) {
            if field.plugin_text != spec.text {
                field.plugin_text = spec.text.clone();
                field.echo = Some(spec.text.clone());
                field.input.set_text(&spec.text, cx);
            }
            return;
        }
        let placeholder: SharedString = spec.placeholder.clone().unwrap_or_default().into();
        let element = id.to_string();
        let (input, subscription) = if spec.multiline {
            let input = cx.new(|cx| PromptInput::new(placeholder, cx).max_rows(MAX_FIELD_ROWS));
            input.update(cx, |input, cx| input.set_text(&spec.text, cx));
            let subscription = cx.subscribe(&input, move |this, input, event, cx| {
                if matches!(event, PromptInputEvent::Changed) {
                    let text = input.read(cx).text();
                    this.field_changed(&element, text, cx);
                }
            });
            (FieldInput::Lines(input), subscription)
        } else {
            let code = spec.code;
            let input = cx.new(|cx| {
                let input = TextInput::new(placeholder, cx);
                if code { input.code() } else { input }
            });
            input.update(cx, |input, cx| input.set_text(&spec.text, cx));
            let subscription = cx.subscribe(&input, move |this, input, _: &InputEvent, cx| {
                let text = input.read(cx).text();
                this.field_changed(&element, text, cx);
            });
            (FieldInput::Line(input), subscription)
        };
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

    /// The text of a field changed: the plugin hears it, unless the view itself put it there.
    fn field_changed(&mut self, element: &str, text: String, cx: &mut Context<Self>) {
        let Some(field) = self.fields.get_mut(element) else {
            return;
        };
        if field.echo.take().is_some_and(|echo| echo == text) {
            return;
        }
        self.emit(element, UiEvent::Changed(text), cx);
    }

    fn submit_field(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(field) = self.fields.get(id) {
            let text = field.input.text(cx);
            self.emit(id, UiEvent::Submitted(text), cx);
        }
    }

    /// ⇥: the next field of the view, in the order of the elements, takes the keyboard.
    fn focus_next_field(
        &mut self,
        _: &FocusNextField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = &self.view else {
            return;
        };
        let order: Vec<&Field> = view
            .elements
            .iter()
            .filter_map(|element| self.fields.get(&element.id))
            .collect();
        if order.is_empty() {
            return;
        }
        let current = order
            .iter()
            .position(|field| field.input.focus_handle(cx).is_focused(window));
        let next = current.map_or(0, |index| (index + 1) % order.len());
        window.focus(&order[next].input.focus_handle(cx));
        cx.notify();
    }

    /// ↵ in a multi-line field: a line break at the caret.
    fn new_line(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(Field {
            input: FieldInput::Lines(input),
            ..
        }) = self.fields.get(id)
        {
            input.update(cx, |input, cx| input.insert("\n", cx));
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
        ((height / tree.row_height()) as usize)
            .saturating_sub(1)
            .max(1)
    }

    fn expand(&mut self, _: &Expand, _: &mut Window, cx: &mut Context<Self>) {
        let Some((id, tree)) = self.active_tree_mut() else {
            return;
        };
        match tree.expand_selected() {
            TreeMove::Toggled(key, expanded) => {
                tree.user_toggled(key.clone(), expanded);
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
                tree.user_toggled(key.clone(), expanded);
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

    /// ⌘↵: the first primary button of the selected row — a card's "Take to Work" — as if pressed.
    fn run_row_action(&mut self, _: &RunRowAction, _: &mut Window, cx: &mut Context<Self>) {
        let Some((id, tree)) = self.active_tree_mut() else {
            return;
        };
        let Some(key) = tree.selected.clone() else {
            return;
        };
        let action = tree
            .rows
            .iter()
            .find(|row| row.key == key)
            .and_then(|row| row.actions.iter().find(|action| action.primary))
            .map(|action| action.id.clone());
        if let Some(action) = action {
            self.emit(&id, UiEvent::RowAction((key, action)), cx);
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
        let toggled = (click_count >= 2 && visible.has_children).then(|| {
            let expanded = tree.toggle(visible.row);
            tree.user_toggled(key.clone(), expanded);
            expanded
        });
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
        tree.user_toggled(key.clone(), expanded);
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
        // A card's line of tags has the list's width less the insets — as of the last frame;
        // before the first one every tag is drawn (and clipped).
        let list_width = f32::from(tree.scroll.0.borrow().base_handle.bounds().size.width);
        let look = RowsLook {
            height: tree.row_height(),
            actions: tree.has_actions,
            tags_width: (list_width > 0.)
                .then_some(list_width - 2. * (ROW_INSET + CARD_PADDING_X + 1.)),
        };
        range
            .filter_map(|index| {
                let visible = tree.visible.get(index).copied()?;
                let row = &tree.rows[visible.row];
                let selected = tree.selected.as_ref() == Some(&row.key);
                let expanded = visible.has_children && tree.is_expanded(visible.row);
                let motion = RowMotion {
                    chevron: tree.disclosure.chevron(&row.key, expanded, window),
                    unfold: tree.disclosure.unfold(index, window),
                };
                Some(if tree.cards {
                    self.render_tree_card(
                        tree_id, index, visible, row, selected, motion, focused, look, window, ui,
                        cx,
                    )
                } else {
                    self.render_tree_row(
                        tree_id, index, visible, row, selected, motion, focused, ui, cx,
                    )
                })
            })
            .collect()
    }

    /// A row's buttons (`tree-row.actions`): compact, the primary one in the accent. A press is
    /// `row-action`; it doesn't reach the row (no selection, no opening).
    fn render_row_actions(
        &self,
        tree_id: &str,
        row: &TreeRow,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(ROW_ACTION_GAP))
            .children(row.actions.iter().enumerate().map(|(position, action)| {
                let id = ElementId::Name(
                    format!("plugin-row-action-{tree_id}-{}-{position}", row.key).into(),
                );
                let action_icon = action
                    .icon
                    .as_deref()
                    .and_then(|name| plugin_icon(&self.plugin, name, &ui));
                let label = action.label.as_deref().map(single_line);
                let button = match (label, action_icon) {
                    (Some(label), action_icon) if action.primary => ui::primary_button(
                        id, label, true, ui,
                    )
                    .when_some(action_icon, |button, action_icon| {
                        button
                            .pl(px(7.))
                            .gap_1()
                            .flex_row_reverse()
                            .justify_end()
                            .child(action_icon.render(ui.accent_text).size(px(13.)))
                    }),
                    (Some(label), Some(action_icon)) => text_button_with_icon(
                        id,
                        action_icon.render(ui.text_muted).size(px(13.)),
                        label,
                        ui,
                    ),
                    (Some(label), None) => ui::text_button(id, label, false, ui),
                    (None, Some(action_icon)) => {
                        ui::icon_button_at(id, action_icon.path(), ui).size(px(ROW_ACTION_HEIGHT))
                    }
                    (None, None) => {
                        ui::icon_button(id, IconName::More, ui).size(px(ROW_ACTION_HEIGHT))
                    }
                };
                // A labelled button is a notch smaller than a toolbar's: it sits on a card.
                let button = if action.label.is_some() {
                    button
                        .h(px(ROW_ACTION_HEIGHT))
                        .px(px(8.))
                        .text_size(px(theme::TEXT_SM))
                } else {
                    button
                };
                let button = match action.tooltip.as_deref() {
                    Some(tooltip) => button.tooltip(ui::tooltip(single_line(tooltip), None)),
                    None => button,
                };
                let tree = tree_id.to_string();
                let key = row.key.clone();
                let action = action.id.clone();
                button
                    // The press is the button's: the row under it neither selects nor opens.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.emit(&tree, UiEvent::RowAction((key.clone(), action.clone())), cx);
                    }))
            }))
    }

    #[allow(clippy::too_many_arguments)]
    fn render_tree_row(
        &self,
        tree_id: &str,
        index: usize,
        visible: Visible,
        row: &TreeRow,
        selected: bool,
        motion: RowMotion,
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
                .children(
                    visible
                        .has_children
                        .then(|| icons::chevron(motion.chevron, ui.dim).size(px(CHEVRON_SIZE))),
                )
        };
        let body = div()
            .relative()
            .size_full()
            .overflow_hidden()
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
                    .min_w(px(LABEL_MIN_WIDTH))
                    .flex_shrink()
                    .truncate()
                    .flex()
                    .gap(px(LABEL_GAP))
                    .child(styled(&row.label, ui, selected, true))
                    .children(
                        row.subtitle
                            .as_ref()
                            .map(|subtitle| styled(subtitle, ui, selected, true)),
                    ),
            )
            // Tags after the label, whole: the first few, then "+N"; the label truncates first.
            .when(!row.tags.is_empty(), |body| {
                let hidden = row.tags.len().saturating_sub(LIST_TAGS);
                body.child(
                    div()
                        .ml(px(LABEL_GAP))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(TAG_GAP))
                        .children(
                            row.tags
                                .iter()
                                .take(LIST_TAGS)
                                .map(|tag| tag_badge(tag, ui)),
                        )
                        .when(hidden > 0, |tags| {
                            tags.child(ui::badge(format!("+{hidden}"), ui.text_muted))
                        }),
                )
            })
            // In a narrow row the detail gives way first (a line number keeps its width when there
            // is room), then the label; never wider than half of the row.
            .children(row.detail.clone().map(|detail| {
                let mut detail = div()
                    .ml(px(LABEL_GAP))
                    .min_w_0()
                    .max_w(relative(0.5))
                    .truncate()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(if selected { ui.text_muted } else { ui.dim })
                    .child(single_line(&detail));
                detail.style().flex_shrink = Some(DETAIL_SHRINK);
                detail
            }))
            .child(div().flex_1().min_w(px(LABEL_GAP)))
            .children(
                row.badge
                    .clone()
                    .map(|badge| ui::badge(single_line(&badge), ui.text_muted)),
            )
            // The row's buttons: at its end while it is selected; while it is only hovered, over
            // its end — so they take no room from the label in the other rows.
            .when(!row.actions.is_empty(), |body| {
                let actions = self.render_row_actions(tree_id, row, ui, cx);
                if selected {
                    body.child(div().ml(px(LABEL_GAP)).child(actions))
                } else {
                    body.child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .right_0()
                            .flex()
                            .items_center()
                            .invisible()
                            .group_hover(ROW_GROUP, |style| style.visible())
                            .child(
                                div()
                                    .h_full()
                                    .pl_1()
                                    .pr_1()
                                    .flex()
                                    .items_center()
                                    .rounded_r(px(RADIUS_SM))
                                    .bg(ui.island)
                                    .child(actions),
                            ),
                    )
                }
            });
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
        let element = div()
            .id(("plugin-tree-row", index))
            .group(ROW_GROUP)
            .h(px(ROW_HEIGHT))
            .w_full()
            .px(px(ROW_INSET))
            .whitespace_nowrap()
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.click_row(&tree, index, event.click_count(), window, cx)
            }))
            .child(body);
        unfold_style(element, motion.unfold).into_any_element()
    }

    /// A row of a tree of cards (a board's column): the icon, the label and the badge, the
    /// subtitle under them (two lines at most), the detail — on a card of a fixed height, with a gap
    /// under it. Selection, hover and clicks are those of a list row.
    #[allow(clippy::too_many_arguments)]
    fn render_tree_card(
        &self,
        tree_id: &str,
        index: usize,
        visible: Visible,
        row: &TreeRow,
        selected: bool,
        motion: RowMotion,
        focused: bool,
        look: RowsLook,
        window: &mut Window,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row_icon = row
            .icon
            .as_deref()
            .and_then(|name| plugin_icon(&self.plugin, name, &ui));
        let chevron = visible.has_children.then(|| {
            let tree = tree_id.to_string();
            div()
                .flex_none()
                .w(px(CHEVRON_WIDTH))
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.click_chevron(&tree, index, window, cx)
                    }),
                )
                .child(icons::chevron(motion.chevron, ui.dim).size(px(CHEVRON_SIZE)))
        });
        let first_line = div()
            .flex_none()
            .h(px(CARD_LINE_HEIGHT))
            .flex()
            .items_center()
            .gap(px(LABEL_GAP))
            .whitespace_nowrap()
            .children(chevron)
            .children(row_icon.map(|row_icon| row_icon.render(ui.text_muted)))
            .child(
                div()
                    .min_w_0()
                    .flex_shrink()
                    .truncate()
                    .child(styled(&row.label, ui, selected, true)),
            )
            .child(div().flex_1())
            .children(
                row.badge
                    .clone()
                    .map(|badge| ui::badge(single_line(&badge), ui.text_muted)),
            );
        let card = div()
            .size_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .px(px(CARD_PADDING_X))
            .py(px(CARD_PADDING_Y))
            .rounded(px(ui::RADIUS_MD))
            .border_1()
            .text_size(px(TEXT_SIZE))
            .child(first_line)
            // Two lines of the subtitle at most. No … on the second: gpui 0.2.2 panics truncating
            // a clamped multi-line text inside a multi-byte character (Cyrillic).
            .children(row.subtitle.as_ref().map(|subtitle| {
                div()
                    .flex_none()
                    .mt(px(4.))
                    .min_w_0()
                    .max_h(px(CARD_SUBTITLE_LINE * 2.))
                    .overflow_hidden()
                    .line_height(px(CARD_SUBTITLE_LINE))
                    .line_clamp(2)
                    .text_color(ui.foreground)
                    .child(styled(subtitle, ui, selected, false))
            }))
            // One line of tags: the ones that fit, then "+N" for the rest.
            .when(!row.tags.is_empty(), |card| {
                let shown = fitting_tags(&row.tags, look.tags_width, window);
                let hidden = row.tags.len() - shown;
                card.child(
                    div()
                        .flex_none()
                        .mt(px(CARD_TAGS_GAP))
                        .h(px(CARD_TAGS_LINE))
                        .flex()
                        .items_center()
                        .gap(px(TAG_GAP))
                        .overflow_hidden()
                        .children(row.tags[..shown].iter().map(|tag| tag_badge(tag, ui)))
                        .when(hidden > 0, |line| {
                            line.child(ui::badge(format!("+{hidden}"), ui.text_muted))
                        }),
                )
            })
            .child(div().flex_1().min_h(px(6.)))
            // The foot: the detail, and the row's buttons on the right.
            .when(row.detail.is_some() || !row.actions.is_empty(), |card| {
                card.child(
                    div()
                        .flex_none()
                        .h(px(if look.actions {
                            ROW_ACTION_HEIGHT
                        } else {
                            CARD_DETAIL_LINE
                        }))
                        .flex()
                        .items_center()
                        .gap(px(LABEL_GAP))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .line_height(px(CARD_DETAIL_LINE))
                                .truncate()
                                .text_size(px(theme::TEXT_SM))
                                .text_color(if selected { ui.text_muted } else { ui.dim })
                                .children(row.detail.as_deref().map(single_line)),
                        )
                        .when(!row.actions.is_empty(), |foot| {
                            foot.child(self.render_row_actions(tree_id, row, ui, cx))
                        }),
                )
            });
        let card = if selected {
            card.bg(if focused {
                ui.list_selected
            } else {
                ui.list_selected_inactive
            })
            .border_color(if focused {
                ui.focus_border
            } else {
                ui.elevated_border
            })
        } else {
            card.bg(ui.elevated)
                .border_color(ui.elevated_border)
                .group_hover(ROW_GROUP, move |style| style.bg(ui.hover))
        };
        let tree = tree_id.to_string();
        let element = div()
            .id(("plugin-tree-row", index))
            .group(ROW_GROUP)
            .h(px(look.height))
            .w_full()
            .px(px(ROW_INSET))
            .pb(px(CARD_GAP))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.click_row(&tree, index, event.click_count(), window, cx)
            }))
            .child(card);
        unfold_style(element, motion.unfold).into_any_element()
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
                        ui::shortcut_in(&Hide, &self.focus_handle, window, cx),
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
            // A column of cards in a row is a board's lane: a tinted surface under its heading and
            // cards, as the columns of Tracker's board.
            ElementKind::Column
                if parent == Parent::Row
                    && self.holds_cards.get(index).copied().unwrap_or(false) =>
            {
                div()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap(px(LANE_PADDING))
                    .pt(px(LANE_PADDING + 2.))
                    .rounded(px(ui::RADIUS_LG))
                    .bg(UiColors::tint(ui.foreground, 0.035))
                    .children(children(cx, Parent::Column))
                    .into_any_element()
            }
            ElementKind::Column => div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(BLOCK_GAP))
                .when(holds_tree, |column| column.flex_1().min_h_0())
                .children(children(cx, Parent::Column))
                .into_any_element(),
            // A row of trees (a board's columns, each a heading and a tree) takes the height left,
            // and its children stretch down it: each tree scrolls by itself.
            // A row of columns of cards (a board) keeps each column readable and scrolls sideways
            // when the tab is too narrow for all of them — by a sideways swipe or Shift+wheel: a
            // scroll down over the board goes to its lanes.
            ElementKind::Row if self.holds_cards.get(index).copied().unwrap_or(false) => div()
                .id(SharedString::from(format!("plugin-row-{id}")))
                .w_full()
                .min_w_0()
                .flex_1()
                .min_h_0()
                .flex()
                .gap(px(ROW_GAP + 2.))
                .px(px(ROW_INSET))
                .pb(px(ROW_INSET))
                .overflow_x_scroll()
                .map(own_axis_only)
                .children(children(cx, Parent::Row))
                .into_any_element(),
            ElementKind::Row => div()
                .w_full()
                .min_w_0()
                .flex()
                .gap(px(ROW_GAP))
                // Without an alignment a flex row stretches its children (taffy's default).
                .map(|row| match holds_tree {
                    true => row.flex_1().min_h_0(),
                    // Buttons that don't fit go on the next line rather than off the edge.
                    false => row.items_center().flex_wrap().gap_y(px(ROW_LINE_GAP)),
                })
                .children(children(cx, Parent::Row))
                .into_any_element(),
            ElementKind::Toolbar => div()
                .flex_none()
                .w_full()
                .h(px(TOOLBAR_HEIGHT))
                .px(px(ROW_INSET))
                .flex()
                .items_center()
                .gap(px(TOOLBAR_GAP))
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
            ElementKind::TextField(_) => match self.fields.get(id).map(|field| &field.input) {
                Some(FieldInput::Line(input)) => {
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
                        .child(div().flex_1().min_w_0().flex().child(input.clone()))
                        .into_any_element()
                }
                Some(FieldInput::Lines(input)) => self.render_lines(id, input, cx),
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
                _ => ui::divider(ui)
                    .mx(px(GAP))
                    .my(px(DIVIDER_MARGIN))
                    .into_any_element(),
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
            ElementKind::Badge(spec) => self.render_badge(id, spec, cx),
            ElementKind::Properties(properties) => self.render_properties(id, properties, cx),
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
                    | ElementKind::Badge(_)
                    | ElementKind::Row
            );
        let grows = matches!(element.kind, ElementKind::Tree(_) | ElementKind::Spacer)
            || (matches!(element.kind, ElementKind::Column | ElementKind::Row) && holds_tree);
        let block = div().when(inset, |block| block.px(px(CONTENT_INSET)));
        let block = if parent == Parent::Row {
            match element.kind {
                // Trees side by side share the row's width and take its whole height; a column of
                // cards keeps a readable width.
                _ if grows && holds_tree => {
                    let cards = self.holds_cards.get(index).copied().unwrap_or(false);
                    block
                        .flex_1()
                        .map(|block| match cards {
                            true => block.min_w(px(CARD_COLUMN_MIN_WIDTH)),
                            false => block.min_w_0(),
                        })
                        .min_h_0()
                        .flex()
                        .flex_col()
                }
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
                    | ElementKind::Badge(_)
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
        // Where the element was drawn: a plugin's picker opens under it (part 10.3).
        let bounds = self.element_bounds.clone();
        let element_id = element.id.clone();
        block
            .on_children_prepainted(move |children, _, _| {
                if let Some(drawn) = children.first() {
                    bounds.borrow_mut().insert(element_id.clone(), *drawn);
                }
            })
            .child(drawn_element)
            .into_any_element()
    }

    /// The labels' column of every properties list (API 0.3) — the longest label as the UI font
    /// draws it, within [`PROPERTY_LABEL_SHARE`] of the content's width (the island's before the
    /// first frame) — and every label and value cut to its column with "…".
    fn measure_properties(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.label_widths.clear();
        self.property_texts.clear();
        let Some(view) = &self.view else {
            return;
        };
        let width = self
            .content_bounds
            .get()
            .map(|area| f32::from(area.size.width))
            .unwrap_or(self.width.get());
        // The list's rows are inset from the content's edges, and their text from the rows'.
        let room = width - 2. * CONTENT_INSET;
        let font = window.text_style().font();
        let mut wrapper = cx.text_system().line_wrapper(font.clone(), px(TEXT_SIZE));
        let measure = |text: &str| {
            let run = TextRun {
                len: text.len(),
                font: font.clone(),
                color: gpui::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let text: SharedString = text.to_string().into();
            let shaped = window
                .text_system()
                .shape_line(text, px(TEXT_SIZE), &[run], None);
            f32::from(shaped.width)
        };
        for element in &view.elements {
            let ElementKind::Properties(properties) = &element.kind else {
                continue;
            };
            let labels: Vec<String> = properties
                .iter()
                .map(|property| single_line(&property.label))
                .collect();
            let widths: Vec<f32> = labels.iter().map(|label| measure(label)).collect();
            let column = label_column_width(&widths, room);
            self.label_widths.insert(element.id.clone(), column);
            for ((property, label), label_width) in properties.iter().zip(labels).zip(widths) {
                let label = if label_width > column {
                    cut_to(&mut wrapper, &font, &label, column)
                } else {
                    label
                };
                let chevron = if property.editable {
                    CHEVRON_SIZE + 4.
                } else {
                    0.
                };
                let value_room = room - column - PROPERTY_COLUMN_GAP - chevron;
                let value = cut_spans(&mut wrapper, &font, &property.value, value_room);
                self.property_texts
                    .insert(format!("{}/{}", element.id, property.id), (label, value));
            }
        }
    }

    /// A properties list (API 0.3): the labels in one dim column, the values beside them — an
    /// editable one with a chevron, the whole row answering the mouse; a click is
    /// `activated(property)`. Each value's bounds are kept as "<list>/<property>" for a picker
    /// opened under it.
    fn render_properties(
        &self,
        id: &str,
        properties: &[Property],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let label_width = self
            .label_widths
            .get(id)
            .copied()
            .unwrap_or(PROPERTY_LABEL_MIN_WIDTH);
        let rows = properties.iter().map(|property| {
            let (label, spans) = self
                .property_texts
                .get(&format!("{id}/{}", property.id))
                .cloned()
                .unwrap_or_else(|| (single_line(&property.label), property.value.clone()));
            let value = if spans.iter().all(|span| span.text.trim().is_empty()) {
                div().text_color(ui.dim).child("—").into_any_element()
            } else {
                // One line, cut with "…" by `measure_properties`, as a properties panel's: a value
                // cut by `line_clamp` breaks off mid-word, and gpui 0.2.2 panics on Cyrillic when the
                // clamp gets an ellipsis.
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(styled(&spans, ui, false, true))
                    .into_any_element()
            };
            // Where the value was drawn: a picker the plugin opens for this property stands under
            // it ("<list>/<property>").
            let (bounds, key) = (self.element_bounds.clone(), format!("{id}/{}", property.id));
            let row = div()
                .on_children_prepainted(move |cells, _, _| {
                    if let Some(value) = cells.get(1) {
                        bounds.borrow_mut().insert(key.clone(), *value);
                    }
                })
                .id(ElementId::Name(
                    format!("plugin-property-{id}-{}", property.id).into(),
                ))
                .w_full()
                .min_h(px(PROPERTY_ROW_HEIGHT))
                .px(px(CONTENT_INSET - ROW_INSET))
                .py(px(PROPERTY_PADDING_Y))
                .flex()
                .items_start()
                .gap(px(PROPERTY_COLUMN_GAP))
                .rounded(px(RADIUS_SM))
                .line_height(px(PROPERTY_ROW_HEIGHT - 2. * PROPERTY_PADDING_Y))
                .child(
                    div()
                        .flex_none()
                        .w(px(label_width))
                        .min_w_0()
                        .text_color(ui.text_muted)
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(label),
                )
                // The value takes the rest of the row and wraps there; an editable one's chevron
                // stands at the row's end, as a dropdown field's.
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_start()
                        .gap_1()
                        .child(div().flex_1().min_w_0().child(value))
                        .when(property.editable, |value| {
                            value.child(
                                div()
                                    .flex_none()
                                    .h(px(PROPERTY_ROW_HEIGHT - 2. * PROPERTY_PADDING_Y))
                                    .flex()
                                    .items_center()
                                    .child(
                                        icon(IconName::ChevronDown, ui.text_muted)
                                            .size(px(CHEVRON_SIZE)),
                                    ),
                            )
                        }),
                );
            if !property.editable {
                return row.into_any_element();
            }
            let (list, key) = (id.to_string(), property.id.clone());
            row.cursor_pointer()
                .hover(move |style| style.bg(ui.hover))
                .active(move |style| style.bg(ui.pressed))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.emit(&list, UiEvent::Activated(key.clone()), cx)
                }))
                .into_any_element()
        });
        div()
            .w_full()
            .px(px(ROW_INSET))
            .flex()
            .flex_col()
            .gap(px(PROPERTY_ROW_GAP))
            .text_size(px(TEXT_SIZE))
            .children(rows)
            .into_any_element()
    }

    /// A multi-line field (API 0.3): a box like a text field's around the growing input.
    fn render_lines(
        &self,
        id: &str,
        input: &Entity<PromptInput>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let focused = self.focused_field.as_deref() == Some(id);
        let (submit, line) = (id.to_string(), id.to_string());
        let focus = input.focus_handle(cx);
        div()
            .id(ElementId::Name(format!("plugin-lines-{id}").into()))
            .key_context(LINES_CONTEXT)
            .w_full()
            .px_2p5()
            .py(px(6.))
            .rounded(px(ui::RADIUS_MD))
            .bg(ui.input_background)
            .border_1()
            .border_color(if focused {
                ui.focus_border
            } else {
                ui.input_border
            })
            .when(focused, |field| field.shadow(ui::focus_ring(ui)))
            .when(!focused, |field| {
                field.hover(move |style| style.border_color(ui.elevated_border))
            })
            .cursor(CursorStyle::IBeam)
            // A click on the box's padding puts the caret into the field.
            .on_click(move |_, window, _| window.focus(&focus))
            .on_action(
                cx.listener(move |this, _: &SubmitField, _, cx| this.submit_field(&submit, cx)),
            )
            .on_action(cx.listener(move |this, _: &NewLine, _, cx| this.new_line(&line, cx)))
            .child(input.clone())
            .into_any_element()
    }

    /// A badge (API 0.3): a colored label; with `menu` it is a dropdown — a button of a toolbar's
    /// height with the label and a chevron (a status with its transitions, the board to show),
    /// and a click is `clicked`.
    fn render_badge(&self, id: &str, spec: &BadgeSpec, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let neutral = matches!(spec.tone, Tone::Normal | Tone::Muted);
        let color = match spec.tone {
            // A neutral badge is muted, as the tree's badges.
            Tone::Normal | Tone::Muted => ui.text_muted,
            tone => tone_color(tone, ui),
        };
        if !spec.menu {
            return ui::badge(single_line(&spec.text), color).into_any_element();
        }
        // A neutral dropdown reads as a field's button; a toned one keeps its color as a chip.
        let (text_color, background, border, hover) = if neutral {
            (
                ui.foreground,
                ui.input_background,
                ui.input_border,
                ui.hover,
            )
        } else {
            (
                color,
                UiColors::tint(color, 0.14),
                UiColors::tint(color, 0.32),
                UiColors::tint(color, 0.22),
            )
        };
        let element = id.to_string();
        div()
            .id(ElementId::Name(format!("plugin-badge-{id}").into()))
            .flex_none()
            .max_w(px(320.))
            .h(px(ui::ICON_BUTTON_SIZE))
            .pl(px(10.))
            .pr(px(7.))
            .flex()
            .items_center()
            .gap(px(5.))
            .rounded(px(ui::RADIUS_SM))
            .border_1()
            .border_color(border)
            .bg(background)
            .text_size(px(theme::TEXT_SM))
            .font_weight(FontWeight::MEDIUM)
            .text_color(text_color)
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .child(div().min_w_0().truncate().child(single_line(&spec.text)))
            .child(
                icon(
                    IconName::ChevronDown,
                    if neutral { ui.text_muted } else { color },
                )
                .size(px(12.)),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.emit(&element, UiEvent::Clicked, cx)))
            .into_any_element()
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
            // The accent button keeps its icon, before the label, as the other buttons do.
            (Some(label), Some(button_icon)) if spec.primary => {
                ui::primary_button(element_id, label, spec.enabled, ui)
                    .pl(px(8.))
                    .gap_1p5()
                    .flex_row_reverse()
                    .justify_end()
                    .child(button_icon.render(ui.accent_text).size(px(14.)))
            }
            (Some(label), None) if spec.primary => {
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
        // Unavailable: dimmed, no hover or press; the change fades.
        let button = ui::available(button, spec.enabled);
        if !spec.enabled {
            return button.into_any_element();
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
            if spec.cards {
                // An empty board column says so where its cards would start.
                return div()
                    .w_full()
                    .flex_1()
                    .px(px(ROW_INSET + CARD_PADDING_X))
                    .pt_1()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(text)
                    .into_any_element();
            }
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
        // A sideways swipe over a board's lane slides the board, not the lane down.
        .map(own_axis_only)
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
        self.focused_field = self
            .fields
            .iter()
            .find(|(_, field)| field.input.focus_handle(cx).is_focused(window))
            .map(|(id, _)| id.clone());
        self.measure_properties(window, cx);
        let content = self.render_content(cx);
        let frame = div()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .relative()
            .h_full()
            .flex()
            .flex_col();
        let frame = match self.chrome {
            Chrome::Island => frame.flex_none().w(px(self.width.get())),
            Chrome::Tab => frame.w_full().min_w_0(),
        };
        frame
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
            .on_action(cx.listener(Self::run_row_action))
            .on_action(cx.listener(Self::focus_next_field))
            .map(|frame| match self.chrome {
                Chrome::Island => frame
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
                            .flex()
                            .flex_col()
                            .on_children_prepainted(record_area(&self.content_bounds))
                            .child(content),
                    )
                    .child(resize_handle(
                        self.resizing,
                        ui::resize_handle_center(ui::style(cx)),
                        ui,
                    )),
                // A tab: the editor's island frames it, the tab names it.
                Chrome::Tab => frame.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .pt(px(TAB_PADDING))
                        .px(px(TAB_PADDING))
                        .flex()
                        .flex_col()
                        .on_children_prepainted(record_area(&self.content_bounds))
                        .child(content),
                ),
            })
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
/// Its middle is `center` past the edge (`ui::resize_handle_center`): in the gap between islands,
/// or on the line between Flat panels (part 10.4).
fn resize_handle(resizing: bool, center: f32, ui: UiColors) -> impl IntoElement {
    div()
        .id("plugin-view-resize")
        .group("plugin-view-resize")
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
/// A row's tag: a small badge in its tone (a neutral one muted, as the tree's badges).
fn tag_badge(tag: &BadgeSpec, ui: UiColors) -> gpui::Div {
    let color = match tag.tone {
        Tone::Normal | Tone::Muted => ui.text_muted,
        tone => tone_color(tone, ui),
    };
    ui::badge(single_line(&tag.text), color)
}

/// How many of a card's tags fit its line of `width` — the rest become "+N", which needs room
/// too; all of them when the width isn't known yet.
fn fitting_tags(tags: &[BadgeSpec], width: Option<f32>, window: &mut Window) -> usize {
    let Some(width) = width else {
        return tags.len();
    };
    let mut font = window.text_style().font();
    font.weight = FontWeight::MEDIUM;
    let text_system = window.text_system().clone();
    let tag_width = |text: &str| {
        let run = TextRun {
            len: text.len(),
            font: font.clone(),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let text: SharedString = text.to_string().into();
        let shaped = text_system.shape_line(text, px(TAG_TEXT_SIZE), &[run], None);
        f32::from(shaped.width) + TAG_PADDING
    };
    let widths: Vec<f32> = tags
        .iter()
        .map(|tag| tag_width(&single_line(&tag.text)))
        .collect();
    fit_widths(&widths, width, |hidden| tag_width(&format!("+{hidden}")))
}

/// The labels' column of a properties list: as wide as its longest label, within
/// [`PROPERTY_LABEL_SHARE`] of the `available` width (a longer label truncates) and never under
/// [`PROPERTY_LABEL_MIN_WIDTH`].
fn label_column_width(widths: &[f32], available: f32) -> f32 {
    let longest = widths.iter().copied().fold(0., f32::max).ceil();
    let cap = (available * PROPERTY_LABEL_SHARE).max(PROPERTY_LABEL_MIN_WIDTH);
    longest.clamp(PROPERTY_LABEL_MIN_WIDTH, cap)
}

/// `text` cut to `width` with "…" (as gpui's own ellipsis would, when it can).
fn cut_to(
    wrapper: &mut gpui::LineWrapperHandle,
    font: &gpui::Font,
    text: &str,
    width: f32,
) -> String {
    let mut runs = vec![TextRun {
        len: text.len(),
        font: font.clone(),
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    }];
    wrapper
        .truncate_line(text.to_string().into(), px(width.max(0.)), "…", &mut runs)
        .to_string()
}

/// A value's spans cut to `width` with "…" — the spans before the cut keep their styles, the one
/// it falls in ends with the ellipsis.
fn cut_spans(
    wrapper: &mut gpui::LineWrapperHandle,
    font: &gpui::Font,
    spans: &[Span],
    width: f32,
) -> Vec<Span> {
    let text: String = spans.iter().map(|span| single_line(&span.text)).collect();
    let cut = cut_to(wrapper, font, &text, width);
    if cut == text {
        return spans.to_vec();
    }
    keep_bytes(spans, cut.len() - "…".len())
}

/// The first `keep` bytes of the spans' (single-line) text, "…" after them.
fn keep_bytes(spans: &[Span], keep: usize) -> Vec<Span> {
    let mut left = keep;
    let mut kept = Vec::new();
    for span in spans {
        let text = single_line(&span.text);
        if left >= text.len() {
            left -= text.len();
            kept.push(Span {
                text,
                ..span.clone()
            });
            continue;
        }
        let mut end = left;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        kept.push(Span {
            text: format!("{}…", &text[..end]),
            ..span.clone()
        });
        return kept;
    }
    if let Some(last) = kept.last_mut() {
        last.text.push('…');
    }
    kept
}

/// How many of the `widths` (with [`TAG_GAP`] between) fit in `room`, keeping room for the
/// "+N" of the rest (its width by `more`).
fn fit_widths(widths: &[f32], room: f32, mut more: impl FnMut(usize) -> f32) -> usize {
    let mut used = 0.;
    let mut shown = 0;
    for (index, width) in widths.iter().enumerate() {
        let next = used + if index == 0 { 0. } else { TAG_GAP } + width;
        if next > room {
            break;
        }
        used = next;
        shown += 1;
    }
    while shown > 0 && shown < widths.len() && used + TAG_GAP + more(widths.len() - shown) > room {
        shown -= 1;
        used = widths[..shown].iter().sum::<f32>() + TAG_GAP * shown.saturating_sub(1) as f32;
    }
    shown
}

pub(crate) fn tone_color(tone: Tone, ui: UiColors) -> gpui::Hsla {
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

/// Records the visible area of a view's content (its first child) as drawn.
fn record_area(
    area: &Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
) -> impl Fn(Vec<Bounds<Pixels>>, &mut Window, &mut App) + 'static {
    let area = area.clone();
    move |children, _, _| area.set(children.first().copied())
}

/// An element whose bottom edge is in the content's visible area, across from it: a popup can
/// open under it. One scrolled above or below is out of sight.
fn in_sight(element: Bounds<Pixels>, area: Bounds<Pixels>) -> bool {
    element.bottom() > area.top()
        && element.bottom() <= area.bottom() + px(1.)
        && element.right() > area.left()
        && element.left() < area.right()
}

/// The element scrolls along its own axis only. gpui turns a scroll along the other axis into its
/// own: a wheel's or a swipe's scroll down would slide a board's row sideways under its lanes.
fn own_axis_only<E: Styled>(mut element: E) -> E {
    element.style().restrict_scroll_to_axis = Some(true);
    element
}

/// Line breaks and tabs as spaces: the byte length stays the same.
pub(crate) fn single_line(text: &str) -> String {
    text.replace(['\n', '\r', '\t'], " ")
}

/// Which elements have a tree inside them (by index), following children as drawn: each element
/// once, no deeper than [`MAX_DEPTH`].
fn holds_tree(elements: &[Element]) -> Vec<bool> {
    holds(elements, |kind| matches!(kind, ElementKind::Tree(_)))
}

/// Which elements have a tree of cards inside them.
fn holds_cards(elements: &[Element]) -> Vec<bool> {
    holds(
        elements,
        |kind| matches!(kind, ElementKind::Tree(spec) if spec.cards),
    )
}

/// Which elements have an element of the kind `is` inside them (or are one).
fn holds(elements: &[Element], is: impl Fn(&ElementKind) -> bool) -> Vec<bool> {
    fn visit(
        elements: &[Element],
        index: usize,
        depth: usize,
        is: &dyn Fn(&ElementKind) -> bool,
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
        let mut holds = is(&element.kind);
        for &child in &element.children {
            holds |= visit(elements, child as usize, depth + 1, is, out);
        }
        out[index] = Some(holds);
        holds
    }
    let mut out = vec![None; elements.len()];
    if !elements.is_empty() {
        visit(elements, 0, 0, &is, &mut out);
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

/// What a tree's rows share when drawn: the card's height (room for tags and buttons when some
/// row has them), and the room of a card's line of tags.
#[derive(Clone, Copy)]
struct RowsLook {
    height: f32,
    actions: bool,
    tags_width: Option<f32>,
}

/// A drawn row's motion: how open its chevron is (0 → 1, turning after the user toggled it) and how
/// far it has unfolded (1 in place) — [`Disclosure::chevron`], [`Disclosure::unfold`].
#[derive(Clone, Copy)]
struct RowMotion {
    chevron: f32,
    unfold: f32,
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
    /// The rows the user expands and collapses: their chevrons turn, the rows they show unfold.
    disclosure: Disclosure<String>,
    selected: Option<String>,
    scroll: UniformListScrollHandle,
    /// The rows are cards (`tree-spec.cards`).
    cards: bool,
    /// Some row has tags, some has buttons: every card leaves room for them.
    has_tags: bool,
    has_actions: bool,
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
            disclosure: Disclosure::default(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            cards: false,
            has_tags: false,
            has_actions: false,
        }
    }

    /// The height of a drawn row: a list row's, or a card's with its gap.
    fn row_height(&self) -> f32 {
        if self.cards {
            card_height(self.has_tags, self.has_actions)
        } else {
            ROW_HEIGHT
        }
    }

    /// The plugin's new rows. A parent must come before its children; a row pointing elsewhere
    /// goes to the top level.
    fn update(&mut self, spec: TreeSpec) {
        self.cards = spec.cards;
        self.rows = spec.rows;
        self.has_tags = self.rows.iter().any(|row| !row.tags.is_empty());
        self.has_actions = self.rows.iter().any(|row| !row.actions.is_empty());
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
        self.disclosure_rows();
    }

    /// Tells the disclosure where the rows under each expanded row are drawn now: the deeper ones
    /// right after it (none yet when the plugin hasn't sent its children).
    fn disclosure_rows(&mut self) {
        let (rows, visible, expanded) = (&self.rows, &self.visible, &self.expanded);
        self.disclosure.rows_changed(|key| {
            let index = visible.iter().position(|v| rows[v.row].key == *key)?;
            let node = visible[index];
            if !expanded
                .get(key)
                .copied()
                .unwrap_or(rows[node.row].expanded)
            {
                return None;
            }
            let end = visible[index + 1..]
                .iter()
                .position(|v| v.depth <= node.depth)
                .map_or(visible.len(), |offset| index + 1 + offset);
            Some(index + 1..end)
        });
    }

    /// The user (a click, a key) expanded or collapsed the row `key` — after
    /// [`TreeState::toggle`]: its chevron turns, the rows it shows unfold. What the plugin expands
    /// (`tree-row.expanded`) simply shows.
    fn user_toggled(&mut self, key: String, expanded: bool) {
        self.disclosure.toggled(key, expanded);
        self.disclosure_rows();
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
            subtitle: None,
            tags: Vec::new(),
            actions: Vec::new(),
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
            cards: false,
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
            cards: false,
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
            cards: false,
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
            cards: false,
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
    fn a_row_of_columns_with_trees_holds_trees() {
        let tree = || {
            ElementKind::Tree(TreeSpec {
                rows: Vec::new(),
                empty_text: None,
                cards: false,
            })
        };
        // A board: a toolbar, then a row of columns — a heading and a tree each.
        let elements = vec![
            element("root", ElementKind::Column, vec![1, 2]),
            element("bar", ElementKind::Toolbar, vec![]),
            element("columns", ElementKind::Row, vec![3, 6]),
            element("open", ElementKind::Column, vec![4, 5]),
            element("open-head", ElementKind::Text(Vec::new()), vec![]),
            element("open-tree", tree(), vec![]),
            element("done", ElementKind::Column, vec![7]),
            element("done-tree", tree(), vec![]),
        ];
        assert_eq!(
            holds_tree(&elements),
            [true, false, true, true, false, true, true, true]
        );
        // Lists, not cards: the row doesn't scroll sideways.
        assert_eq!(holds_cards(&elements), [false; 8]);
    }

    #[test]
    fn a_row_of_columns_of_cards_holds_cards() {
        let cards = || {
            ElementKind::Tree(TreeSpec {
                rows: Vec::new(),
                empty_text: None,
                cards: true,
            })
        };
        let elements = vec![
            element("root", ElementKind::Column, vec![1, 2]),
            element("bar", ElementKind::Toolbar, vec![]),
            element("columns", ElementKind::Row, vec![3, 5]),
            element("open", ElementKind::Column, vec![4]),
            element("open-cards", cards(), vec![]),
            element("done", ElementKind::Column, vec![6]),
            element("done-cards", cards(), vec![]),
        ];
        assert_eq!(
            holds_cards(&elements),
            [true, false, true, true, true, true, true]
        );
        let mut tree = TreeState::new();
        tree.update(TreeSpec {
            rows: vec![row("TRACK-1", None, false)],
            empty_text: None,
            cards: true,
        });
        assert_eq!(tree.row_height(), card_height(false, false));
    }

    #[test]
    fn cards_make_room_for_tags_and_buttons_when_some_row_has_them() {
        use flux_plugin::api::ui::RowAction;
        let tag = BadgeSpec {
            text: "backend".into(),
            tone: Tone::Accent,
            menu: false,
        };
        let take = RowAction {
            id: "take".into(),
            label: Some("Take to Work".into()),
            icon: None,
            tooltip: None,
            primary: true,
        };
        let mut tree = TreeState::new();
        let mut tagged = row("TRACK-1", None, false);
        tagged.tags = vec![tag];
        let mut plain = row("TRACK-2", None, false);
        tree.update(TreeSpec {
            rows: vec![tagged.clone(), plain.clone()],
            empty_text: None,
            cards: true,
        });
        // One height for the whole column: the plain card keeps the tags' line empty.
        assert_eq!(tree.row_height(), card_height(true, false));
        assert!(card_height(true, false) > card_height(false, false));
        plain.actions = vec![take];
        tree.update(TreeSpec {
            rows: vec![tagged, plain],
            empty_text: None,
            cards: true,
        });
        assert_eq!(tree.row_height(), card_height(true, true));
        assert!(card_height(true, true) > card_height(true, false));
        // A list's rows keep their height whatever they carry.
        tree.cards = false;
        assert_eq!(tree.row_height(), ROW_HEIGHT);
    }

    #[test]
    fn tags_that_dont_fit_leave_room_for_their_count() {
        let more = |_| 24.;
        // All fit: no "+N".
        assert_eq!(fit_widths(&[40., 40.], 100., more), 2);
        // The third doesn't fit, and "+1" needs room: two of them stay.
        assert_eq!(fit_widths(&[40., 40., 40.], 120., more), 2);
        // "+2" doesn't fit after the second: one of them stays.
        assert_eq!(fit_widths(&[40., 40., 40.], 100., more), 1);
        // Not even one fits: only "+N".
        assert_eq!(fit_widths(&[300.], 100., more), 0);
    }

    #[test]
    fn an_anchor_scrolled_out_of_sight_isnt_one() {
        let area = Bounds::new(
            gpui::point(px(0.), px(100.)),
            gpui::size(px(300.), px(400.)),
        );
        let at = |y: f32| Bounds::new(gpui::point(px(20.), px(y)), gpui::size(px(80.), px(20.)));
        assert!(in_sight(at(120.), area));
        // Its bottom edge at the area's bottom: still in sight.
        assert!(in_sight(at(480.), area));
        // Scrolled below the area, or above it.
        assert!(!in_sight(at(490.), area));
        assert!(!in_sight(at(60.), area));
        assert!(!in_sight(at(900.), area));
    }

    #[test]
    fn a_properties_labels_column_fits_the_longest_label_within_its_share() {
        // As wide as the longest label…
        assert_eq!(label_column_width(&[60.4, 95.2, 80.], 400.), 96.);
        // …never narrower than the least width, so short labels still line their values up…
        assert_eq!(
            label_column_width(&[20., 31.], 400.),
            PROPERTY_LABEL_MIN_WIDTH
        );
        // …and not wider than its share of the list: a longer label truncates.
        assert_eq!(
            label_column_width(&[300.], 400.),
            400. * PROPERTY_LABEL_SHARE
        );
        // A list too narrow for its share keeps the least width.
        assert_eq!(label_column_width(&[300.], 100.), PROPERTY_LABEL_MIN_WIDTH);
        assert_eq!(label_column_width(&[], 400.), PROPERTY_LABEL_MIN_WIDTH);
    }

    #[test]
    fn a_value_cut_keeps_the_styles_before_the_cut() {
        let spans = vec![
            Span {
                text: "до ".into(),
                tone: Tone::Muted,
                bold: false,
                code: false,
                highlight: false,
            },
            Span {
                text: "20 октября".into(),
                tone: Tone::Error,
                bold: true,
                code: false,
                highlight: false,
            },
        ];
        // The cut falls into the second span: the first stays, the second ends with "…".
        let cut = keep_bytes(&spans, "до 20 ок".len());
        assert_eq!(cut.len(), 2);
        assert_eq!(cut[0].text, "до ");
        assert_eq!(cut[1].text, "20 ок…");
        assert_eq!(cut[1].tone, Tone::Error);
        // A cut inside a Cyrillic letter goes back to its start.
        let cut = keep_bytes(&spans, "до 20 о".len() + 1);
        assert_eq!(cut[1].text, "20 о…");
        // A cut at a span's end leaves the next span out.
        let cut = keep_bytes(&spans, "до ".len());
        assert_eq!(cut.len(), 2);
        assert_eq!(cut[1].text, "…");
    }

    #[test]
    fn single_line_keeps_byte_lengths() {
        let text = "a\tb\nc\r";
        assert_eq!(single_line(text), "a b c ");
        assert_eq!(single_line(text).len(), text.len());
    }

    /// A trackpad's scroll event (gpui's patch, `ScrollGesture`: a board's row and its lanes take
    /// only the movement along the gesture's axis).
    fn swipe(phase: gpui::TouchPhase, x: f32, y: f32) -> gpui::ScrollWheelEvent {
        gpui::ScrollWheelEvent {
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(x), px(y))),
            touch_phase: phase,
            ..Default::default()
        }
    }

    #[test]
    fn a_swipe_keeps_to_its_predominant_axis() {
        use gpui::{Axis, ScrollGesture, TouchPhase};
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut gesture = ScrollGesture::default();
        // Fingers down (no movement yet), then a swipe down that drifts a little sideways: an
        // event of it alone would scroll sideways, the gesture doesn't.
        assert_eq!(
            gesture.update(&swipe(TouchPhase::Started, 0., 0.), at(0)),
            None
        );
        let down = [(0.5, -2.), (1., -3.), (-1., -6.), (3., -1.), (-2., 0.)];
        for (i, (x, y)) in down.into_iter().enumerate() {
            let event = swipe(TouchPhase::Moved, x, y);
            assert_eq!(
                gesture.update(&event, at(8 * i as u64)),
                Some(Axis::Vertical)
            );
        }
        // The fingers lift; the momentum keeps the axis.
        assert_eq!(
            gesture.update(&swipe(TouchPhase::Ended, 0., 0.), at(48)),
            None
        );
        let momentum = swipe(TouchPhase::Moved, 2., -1.);
        assert_eq!(gesture.update(&momentum, at(56)), Some(Axis::Vertical));
        // The next gesture decides again: sideways, with a little vertical movement.
        let sideways = [(3., 0.5), (5., -1.), (4., 2.), (0., -1.)];
        for (i, (x, y)) in sideways.into_iter().enumerate() {
            let phase = if i == 0 {
                TouchPhase::Started
            } else {
                TouchPhase::Moved
            };
            let event = swipe(phase, x, y);
            assert_eq!(
                gesture.update(&event, at(64 + 8 * i as u64)),
                Some(Axis::Horizontal)
            );
        }
        // Until the gesture has moved enough, each event goes its own way.
        assert_eq!(
            gesture.update(&swipe(TouchPhase::Started, 0.5, -1.), at(200)),
            Some(Axis::Vertical)
        );
        assert_eq!(
            gesture.update(&swipe(TouchPhase::Moved, 1.5, 0.), at(208)),
            Some(Axis::Horizontal)
        );
    }

    #[test]
    fn a_scroll_without_phases_starts_anew_after_a_pause() {
        use gpui::{Axis, ScrollGesture, TouchPhase};
        let start = Instant::now();
        let mut gesture = ScrollGesture::default();
        assert_eq!(
            gesture.update(&swipe(TouchPhase::Moved, 0., -6.), start),
            Some(Axis::Vertical)
        );
        let sideways = swipe(TouchPhase::Moved, 6., 0.);
        let soon = start + Duration::from_millis(16);
        assert_eq!(gesture.update(&sideways, soon), Some(Axis::Vertical));
        let later = soon + ScrollGesture::PAUSE + Duration::from_millis(1);
        assert_eq!(gesture.update(&sideways, later), Some(Axis::Horizontal));
    }

    #[test]
    fn a_wheel_step_is_a_gesture_by_itself() {
        use gpui::{Axis, ScrollDelta, ScrollGesture, ScrollWheelEvent};
        let wheel = |x: f32, y: f32| ScrollWheelEvent {
            delta: ScrollDelta::Lines(gpui::point(x, y)),
            ..Default::default()
        };
        let now = Instant::now();
        let mut gesture = ScrollGesture::default();
        assert_eq!(gesture.update(&wheel(0., -1.), now), Some(Axis::Vertical));
        // Shift+wheel: macOS sends it sideways.
        assert_eq!(gesture.update(&wheel(-1., 0.), now), Some(Axis::Horizontal));
        assert_eq!(gesture.axis(), Some(Axis::Horizontal));
    }

    #[test]
    fn a_diagonal_scroll_goes_down() {
        use gpui::{Axis, ScrollGesture};
        let axis = |x: f32, y: f32| ScrollGesture::predominant_axis(gpui::point(px(x), px(y)));
        assert_eq!(axis(0., 0.), None);
        assert_eq!(axis(5., -5.), Some(Axis::Vertical));
        assert_eq!(axis(-5.5, 5.), Some(Axis::Vertical));
        assert_eq!(axis(-6.5, 5.), Some(Axis::Horizontal));
        assert_eq!(axis(0.5, 0.), Some(Axis::Horizontal));
        assert_eq!(axis(0., 0.5), Some(Axis::Vertical));
    }
}
