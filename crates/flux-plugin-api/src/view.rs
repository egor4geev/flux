//! A tool window's content, built as a tree in Rust and flattened into the `ui::View` Flux draws:
//!
//! ```ignore
//! use flux_plugin_api::view::*;
//!
//! let mut tree = Tree::new().empty_text(tr("No TODO items"));
//! let file = tree.add(RowSpec::new("src/main.rs", "main.rs").icon("file:main.rs").badge("2"));
//! tree.add_child(file, RowSpec::new("src/main.rs:12", "TODO: handle errors").detail("12"));
//! let view = column("root", [
//!     toolbar("toolbar", [icon_button("refresh", "refresh", &tr("Refresh"))]),
//!     tree.into_element("items"),
//! ])
//! .into_view();
//! flux_plugin_api::host::ui::set_view("todo", &view);
//! ```
//!
//! ([`set_view`] does the last two steps at once.) Every element has an id: events name it, and
//! Flux keeps the element's state (a field's text, the scroll, expanded and selected rows) between
//! views by it. To start an element afresh — Expand All applying the rows' `expanded` again —
//! give it a new id.

use crate::host::ui::{
    ButtonSpec, Element as RawElement, ElementKind, Span, TextFieldSpec, ToggleSpec, TreeRow,
    TreeSpec, View,
};
pub use crate::host::ui::{ToggleStyle, Tone};

/// An element with its children, before flattening.
pub struct Element {
    id: String,
    kind: ElementKind,
    children: Vec<Element>,
}

impl Element {
    fn new(id: &str, kind: ElementKind) -> Self {
        Element {
            id: id.to_string(),
            kind,
            children: Vec::new(),
        }
    }

    fn with_children(mut self, children: impl IntoIterator<Item = Element>) -> Self {
        self.children = children.into_iter().collect();
        self
    }

    /// The view with this element as its root.
    pub fn into_view(self) -> View {
        let mut elements = Vec::new();
        flatten(self, &mut elements);
        View { elements }
    }
}

/// Shows `root` as the content of the tool window `window` (`[[tool-windows]]` of the manifest).
pub fn set_view(window: &str, root: Element) {
    crate::host::ui::set_view(window, &root.into_view());
}

/// Appends the element and its subtree; returns its index.
fn flatten(element: Element, out: &mut Vec<RawElement>) -> u32 {
    let index = out.len();
    out.push(RawElement {
        id: element.id,
        kind: element.kind,
        children: Vec::new(),
    });
    let children: Vec<u32> = element
        .children
        .into_iter()
        .map(|child| flatten(child, out))
        .collect();
    out[index].children = children;
    index as u32
}

/// Children top to bottom.
pub fn column(id: &str, children: impl IntoIterator<Item = Element>) -> Element {
    Element::new(id, ElementKind::Column).with_children(children)
}

/// Children left to right, centered vertically.
pub fn row(id: &str, children: impl IntoIterator<Item = Element>) -> Element {
    Element::new(id, ElementKind::Row).with_children(children)
}

/// The window's toolbar: buttons in a row under the title.
pub fn toolbar(id: &str, children: impl IntoIterator<Item = Element>) -> Element {
    Element::new(id, ElementKind::Toolbar).with_children(children)
}

/// Text of several spans.
pub fn text(id: &str, spans: impl IntoIterator<Item = Span>) -> Element {
    Element::new(id, ElementKind::Text(spans.into_iter().collect()))
}

/// Plain text.
pub fn label(id: &str, text: &str) -> Element {
    Element::new(id, ElementKind::Text(vec![span(text)]))
}

/// Markdown, drawn as Flux draws documentation: paragraphs, lists, code with highlighting, links.
pub fn markdown(id: &str, markdown: &str) -> Element {
    Element::new(id, ElementKind::Markdown(markdown.to_string()))
}

/// A text button; [`Button`] sets the rest.
pub fn button(id: &str, label: &str) -> Button {
    Button {
        id: id.to_string(),
        spec: ButtonSpec {
            label: Some(label.to_string()),
            icon: None,
            tooltip: None,
            primary: false,
            enabled: true,
        },
    }
}

/// An icon button with a tooltip: the toolbar's kind.
pub fn icon_button(id: &str, icon: &str, tooltip: &str) -> Element {
    Button {
        id: id.to_string(),
        spec: ButtonSpec {
            label: None,
            icon: Some(icon.to_string()),
            tooltip: Some(tooltip.to_string()),
            primary: false,
            enabled: true,
        },
    }
    .into()
}

/// A button being built; it becomes an [`Element`] with `.into()`. A press comes as `clicked`.
pub struct Button {
    id: String,
    spec: ButtonSpec,
}

impl Button {
    /// An icon before the label: a built-in name, `file:<name>`, or an SVG of the plugin.
    pub fn icon(mut self, icon: &str) -> Self {
        self.spec.icon = Some(icon.to_string());
        self
    }

    /// Shown when the pointer rests on the button.
    pub fn tooltip(mut self, tooltip: &str) -> Self {
        self.spec.tooltip = Some(tooltip.to_string());
        self
    }

    /// The accent button: the main action, or the chosen one of a group.
    pub fn primary(mut self) -> Self {
        self.spec.primary = true;
        self
    }

    /// A disabled button is grayed out and doesn't press.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.spec.enabled = enabled;
        self
    }
}

impl From<Button> for Element {
    fn from(button: Button) -> Element {
        Element::new(&button.id, ElementKind::Button(button.spec))
    }
}

/// A text field; its changes come as `changed`, ↵ as `submitted`.
pub fn text_field(id: &str, text: &str, placeholder: Option<&str>) -> Element {
    Element::new(
        id,
        ElementKind::TextField(TextFieldSpec {
            text: text.to_string(),
            placeholder: placeholder.map(str::to_string),
            code: false,
        }),
    )
}

/// A switch with a label; a change comes as `toggled`.
pub fn switch(id: &str, label: &str, on: bool) -> Element {
    toggle(id, label, on, ToggleStyle::Switch)
}

/// A checkbox with a label; a change comes as `toggled`.
pub fn checkbox(id: &str, label: &str, on: bool) -> Element {
    toggle(id, label, on, ToggleStyle::Checkbox)
}

fn toggle(id: &str, label: &str, on: bool, style: ToggleStyle) -> Element {
    Element::new(
        id,
        ElementKind::Toggle(ToggleSpec {
            label: label.to_string(),
            on,
            style,
        }),
    )
}

/// A thin line between groups.
pub fn divider(id: &str) -> Element {
    Element::new(id, ElementKind::Divider)
}

/// Takes the free space of a row or a column: pushes what follows to the end.
pub fn spacer(id: &str) -> Element {
    Element::new(id, ElementKind::Spacer)
}

/// A progress bar: 0.0–1.0; none — indeterminate.
pub fn progress(id: &str, fraction: Option<f32>) -> Element {
    Element::new(id, ElementKind::Progress(fraction))
}

/// A span of plain text; the methods set its style.
pub fn span(text: &str) -> Span {
    Span {
        text: text.to_string(),
        tone: Tone::Normal,
        bold: false,
        code: false,
        highlight: false,
    }
}

/// Styles of a [`Span`]: `span("12").tone(Tone::Dim)`.
pub trait SpanStyle {
    /// The color's role: `Tone::Dim` for secondary text, `Tone::Error` for a failure.
    fn tone(self, tone: Tone) -> Self;
    /// Bold.
    fn bold(self) -> Self;
    /// In the code font.
    fn code(self) -> Self;
    /// Marked as a match.
    fn highlight(self) -> Self;
}

impl SpanStyle for Span {
    fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    fn code(mut self) -> Self {
        self.code = true;
        self
    }

    fn highlight(mut self) -> Self {
        self.highlight = true;
        self
    }
}

/// The rows of a tree or a list.
#[derive(Default)]
pub struct Tree {
    rows: Vec<TreeRow>,
    empty_text: Option<String>,
}

impl Tree {
    /// No rows yet.
    pub fn new() -> Self {
        Tree::default()
    }

    /// The text in the middle when there are no rows.
    pub fn empty_text(mut self, text: &str) -> Self {
        self.empty_text = Some(text.to_string());
        self
    }

    /// A top-level row; returns its index for [`Tree::add_child`].
    pub fn add(&mut self, row: RowSpec) -> u32 {
        self.push(None, row)
    }

    /// A child of the row at `parent`.
    pub fn add_child(&mut self, parent: u32, row: RowSpec) -> u32 {
        self.push(Some(parent), row)
    }

    fn push(&mut self, parent: Option<u32>, row: RowSpec) -> u32 {
        self.rows.push(TreeRow {
            key: row.key,
            parent,
            icon: row.icon,
            label: row.label,
            detail: row.detail,
            badge: row.badge,
            expanded: row.expanded,
        });
        (self.rows.len() - 1) as u32
    }

    /// No rows were added.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The tree as an element: it takes the remaining height and scrolls. A row's selection
    /// comes as `selected`, its opening (↵, a double click, a click on a row without children) as
    /// `activated`, its expansion as `expanded` — with the row's key.
    pub fn into_element(self, id: &str) -> Element {
        Element::new(
            id,
            ElementKind::Tree(TreeSpec {
                rows: self.rows,
                empty_text: self.empty_text,
            }),
        )
    }
}

/// A row: `RowSpec::new(key, label).icon("file:main.rs").detail("12")`.
pub struct RowSpec {
    key: String,
    label: Vec<Span>,
    icon: Option<String>,
    detail: Option<String>,
    badge: Option<String>,
    expanded: bool,
}

impl RowSpec {
    /// A row with a plain label; `key` is stable between views (events name the row by it).
    pub fn new(key: &str, label: &str) -> Self {
        Self::spans(key, vec![span(label)])
    }

    /// A row with a styled label.
    pub fn spans(key: &str, label: Vec<Span>) -> Self {
        RowSpec {
            key: key.to_string(),
            label,
            icon: None,
            detail: None,
            badge: None,
            expanded: false,
        }
    }

    /// A built-in icon, `file:<name>` for a file type, or an SVG of the plugin.
    pub fn icon(mut self, icon: &str) -> Self {
        self.icon = Some(icon.to_string());
        self
    }

    /// Dim text after the label: a path, a line number.
    pub fn detail(mut self, detail: &str) -> Self {
        self.detail = Some(detail.to_string());
        self
    }

    /// A count on the right.
    pub fn badge(mut self, badge: &str) -> Self {
        self.badge = Some(badge.to_string());
        self
    }

    /// Expanded when first shown.
    pub fn expanded(mut self) -> Self {
        self.expanded = true;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_depth_first() {
        let view = column(
            "root",
            [
                toolbar("bar", [icon_button("refresh", "refresh", "Refresh")]),
                label("hint", "Nothing yet"),
            ],
        )
        .into_view();
        let ids: Vec<&str> = view.elements.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["root", "bar", "refresh", "hint"]);
        assert_eq!(view.elements[0].children, [1, 3]);
        assert_eq!(view.elements[1].children, [2]);
    }

    #[test]
    fn tree_rows_point_at_parents() {
        let mut tree = Tree::new();
        let file = tree.add(RowSpec::new("a", "a.rs").expanded());
        tree.add_child(file, RowSpec::new("a:1", "TODO"));
        let ElementKind::Tree(spec) = tree.into_element("items").kind else {
            panic!("not a tree");
        };
        assert_eq!(spec.rows[1].parent, Some(0));
        assert!(spec.rows[0].expanded);
    }
}
