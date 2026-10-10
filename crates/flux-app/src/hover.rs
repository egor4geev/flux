//! Hover: the type and documentation of the symbol under the mouse (after a pause) or at the cursor
//! (F1), with the diagnostics at that place on top. Under the problems, as in JetBrains' error
//! tooltip: "Fix with Claude" (a new session) and "More actions… ⌥↵" (the context actions there).
//! F2 / ⇧F2 show the problem they go to in the same popup, without the documentation.
//!
//! A mouse popup stays while the mouse is over its symbol or over the popup itself; when it leaves
//! both, the popup hides after a moment (time to cross the gap to the popup). An edit, a cursor
//! movement, the editor losing focus, or Esc hide any popup; F1 again hides the one it showed.
//!
//! With ⌘ held, the word under the mouse is drawn as a link (underline, pointing hand): ⌘-click
//! goes to its definition (`navigation::cmd_click`).

use std::ops::Range;
use std::time::Duration;

use flux_core::Selection;
use flux_core::movement;
use flux_core::text::{CharClass, char_class, line_len, line_start};
use flux_lsp::lsp_types::request::HoverRequest;
use flux_lsp::lsp_types::{
    HoverContents, HoverParams, HoverProviderCapability, MarkedString, TextDocumentIdentifier,
    TextDocumentPositionParams,
};
use flux_syntax::language_for_path;
use gpui::{
    AnyElement, App, Context, Corner, CursorStyle, DispatchPhase, Div, KeyBinding, KeyContext,
    Modifiers, ModifiersChangedEvent, MouseMoveEvent, Pixels, Point, Task, Window, actions,
    anchored, canvas, deferred, div, point, prelude::*, px,
};

use crate::diagnostics::{Diagnostic, Severity};
use crate::editor::Editor;
use crate::i18n::tr;
use crate::icons::{IconName, icon};
use crate::markdown::{self, Block, CodeBlock};
use crate::popup::{self, POPUP_GAP, Side, WINDOW_MARGIN};
use crate::theme::{self, Theme, UiColors};
use crate::ui;

/// How long the mouse rests over a symbol before the popup shows.
const HOVER_DELAY: Duration = Duration::from_millis(500);
/// After the mouse leaves the symbol: time to reach the popup.
const HIDE_DELAY: Duration = Duration::from_millis(300);
const MAX_WIDTH: f32 = 560.;
const MAX_HEIGHT: f32 = 360.;
/// The popup goes above its line if there is this much room there (or more than below).
const MIN_ROOM_ABOVE: f32 = 160.;
/// The popup's text starts this far right of its left edge (border + padding): the popup is
/// shifted left by it, so the text lines up with the symbol.
const TEXT_INSET: f32 = 13.;

actions!(editor, [ShowHover]);
actions!(hover, [HideHover]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("f1", ShowHover, Some("Editor")),
        KeyBinding::new("escape", HideHover, Some("Editor && showing_hover")),
    ]);
}

/// Hover state of an editor (`Editor::hover`).
#[derive(Default)]
pub struct HoverState {
    /// The mouse rests over a symbol: the popup shows when the timer fires.
    pending: Option<Pending>,
    popup: Option<Popup>,
    /// The character under the mouse, as of its last move over the text.
    mouse_over: Option<usize>,
    /// The mouse is over the popup.
    over_popup: bool,
    /// The mouse left the symbol: the popup hides after [`HIDE_DELAY`].
    hide_task: Option<Task<()>>,
    next_id: usize,
    /// ⌘ is held over this word: it is drawn as a link.
    link: Option<Range<usize>>,
}

struct Pending {
    range: Range<usize>,
    _task: Task<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Mouse,
    Keyboard,
}

struct Popup {
    id: usize,
    trigger: Trigger,
    /// The symbol: the popup is anchored at its start, the mouse may move within it.
    range: Range<usize>,
    /// The selection when shown: a cursor movement hides the popup.
    selection: Selection,
    diagnostics: Vec<Diagnostic>,
    blocks: Vec<Block>,
    /// The hover request in flight.
    request: Option<Task<()>>,
}

/// From `Editor::text_changed`: an edit hides everything and cancels the waiting.
pub fn text_changed(editor: &mut Editor) {
    let hover = &mut editor.hover;
    hover.pending = None;
    hover.popup = None;
    hover.hide_task = None;
    hover.link = None;
}

/// From `Editor::on_mouse_move`: a pause over a symbol shows its popup; leaving the symbol of the
/// shown popup hides it after a moment.
pub fn mouse_moved(editor: &mut Editor, event: &MouseMoveEvent, cx: &mut Context<Editor>) {
    // Dragging a selection: no hover.
    let position = match event.pressed_button {
        Some(_) => None,
        None => position_under(editor, event.position),
    };
    editor.hover.mouse_over = position;
    update_link(editor, event.modifiers, cx);
    if let Some(popup) = &editor.hover.popup
        && popup.trigger == Trigger::Mouse
    {
        if position.is_some_and(|p| popup.range.contains(&p)) {
            editor.hover.hide_task = None;
            editor.hover.pending = None;
            return;
        }
        schedule_hide(editor, cx);
    }
    let Some((position, range)) = position.and_then(|p| Some((p, symbol_range(editor, p)?))) else {
        editor.hover.pending = None;
        return;
    };
    if editor
        .hover
        .pending
        .as_ref()
        .is_some_and(|pending| pending.range == range)
    {
        return;
    }
    let task = cx.spawn({
        let range = range.clone();
        async move |this, cx| {
            cx.background_executor().timer(HOVER_DELAY).await;
            this.update(cx, |editor, cx| {
                editor.hover.pending = None;
                show(editor, position, range, Trigger::Mouse, cx);
            })
            .ok();
        }
    });
    editor.hover.pending = Some(Pending { range, _task: task });
}

/// Adds `showing_hover` to the editor's key context while a popup is shown (Esc hides it).
pub fn extend_key_context(editor: &Editor, window: &Window, context: &mut KeyContext) {
    if is_visible(editor, window) {
        context.add("showing_hover");
    }
}

/// Registers the editor actions: F1 always, hiding only while there is a popup. Also follows ⌘
/// for the link under the mouse.
pub fn actions(root: Div, editor: &Editor, cx: &mut Context<Editor>) -> Div {
    root.on_modifiers_changed(cx.listener(|editor, event: &ModifiersChangedEvent, _, cx| {
        update_link(editor, event.modifiers, cx)
    }))
    .on_action(cx.listener(|editor, _: &ShowHover, _, cx| show_at_cursor(editor, cx)))
    .when(editor.hover.popup.is_some(), |root| {
        root.on_action(cx.listener(|editor, _: &HideHover, _, cx| {
            if editor.hover.popup.take().is_some() {
                cx.notify();
            } else {
                cx.propagate();
            }
        }))
    })
}

/// F1: the popup for the symbol at the cursor (at the end of a word — for that word); F1 again
/// hides it.
fn show_at_cursor(editor: &mut Editor, cx: &mut Context<Editor>) {
    let text = editor.document.text();
    let len = text.len_chars();
    let head = editor.document.selection().primary().head;
    let is_word = |pos: usize| pos < len && char_class(text.char(pos)) == CharClass::Word;
    let position = if !is_word(head) && head > 0 && is_word(head - 1) {
        head - 1
    } else {
        head
    };
    if let Some(popup) = &editor.hover.popup
        && popup.trigger == Trigger::Keyboard
        && popup.selection == *editor.document.selection()
    {
        editor.hover.popup = None;
        cx.notify();
        return;
    }
    let range = if is_word(position) {
        let word = movement::word_range_at(text, position);
        word.from()..word.to()
    } else {
        position..(position + 1).min(len)
    };
    show(editor, position, range, Trigger::Keyboard, cx);
}

/// F2 / ⇧F2 went to the problem at `position`: its popup at the caret — the problem only, with its
/// actions; hidden by Esc, typing and caret moves.
pub fn show_problem(editor: &mut Editor, position: usize, cx: &mut Context<Editor>) {
    let diagnostics: Vec<Diagnostic> = editor
        .diagnostics
        .at(position)
        .into_iter()
        .cloned()
        .collect();
    let Some(first) = diagnostics.first() else {
        return;
    };
    let len = editor.document.text().len_chars();
    let range = if first.range.is_empty() {
        position..(position + 1).min(len.max(position + 1))
    } else {
        first.range.clone()
    };
    let id = editor.hover.next_id;
    editor.hover.next_id += 1;
    editor.hover.pending = None;
    editor.hover.hide_task = None;
    editor.hover.over_popup = false;
    editor.hover.popup = Some(Popup {
        id,
        trigger: Trigger::Keyboard,
        range,
        selection: editor.document.selection().clone(),
        diagnostics,
        blocks: Vec::new(),
        request: None,
    });
    cx.notify();
}

/// Shows the popup for `position`: the diagnostics there right away, the server's hover when it
/// answers. Nothing to show — nothing shown (F1 says so in the status bar).
fn show(
    editor: &mut Editor,
    position: usize,
    range: Range<usize>,
    trigger: Trigger,
    cx: &mut Context<Editor>,
) {
    let diagnostics: Vec<Diagnostic> = editor
        .diagnostics
        .at(position)
        .into_iter()
        .cloned()
        .collect();
    let id = editor.hover.next_id;
    editor.hover.next_id += 1;
    let request = request(editor, position, id, cx);
    if request.is_none() && diagnostics.is_empty() {
        editor.hover.popup = None;
        if trigger == Trigger::Keyboard {
            editor.show_status(tr("No documentation").into(), cx);
        }
        return;
    }
    let hover = &mut editor.hover;
    hover.popup = Some(Popup {
        id,
        trigger,
        range,
        selection: editor.document.selection().clone(),
        diagnostics,
        blocks: Vec::new(),
        request,
    });
    hover.hide_task = None;
    hover.over_popup = false;
    cx.notify();
}

/// `textDocument/hover` at `position`, if the document's server provides it; the answer is
/// prepared (Markdown parsed, code highlighted) in the background.
fn request(
    editor: &Editor,
    position: usize,
    id: usize,
    cx: &mut Context<Editor>,
) -> Option<Task<()>> {
    let (server, uri) = crate::lsp::document_server(editor)?;
    let capabilities = server.capabilities()?;
    let supported = match &capabilities.hover_provider {
        Some(HoverProviderCapability::Simple(supported)) => *supported,
        Some(HoverProviderCapability::Options(_)) => true,
        None => false,
    };
    if !supported {
        return None;
    }
    let text = editor.document.text().clone();
    let params = HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: flux_lsp::position::to_lsp(&text, position),
        },
        work_done_progress_params: Default::default(),
    };
    let response = server.request::<HoverRequest>(params);
    let language = editor.document.path().and_then(language_for_path);
    let scopes = markdown::scopes(cx);
    Some(cx.spawn(async move |this, cx| {
        let hover = response.await.ok().flatten();
        let range = hover
            .as_ref()
            .and_then(|hover| hover.range)
            .map(|range| flux_lsp::position::range_from_lsp(&text, range));
        let blocks = match hover {
            Some(hover) => {
                cx.background_executor()
                    .spawn(async move {
                        let mut blocks = hover_blocks(hover.contents);
                        markdown::highlight(&mut blocks, language, &scopes);
                        blocks
                    })
                    .await
            }
            None => Vec::new(),
        };
        this.update(cx, |editor, cx| received(editor, id, blocks, range, cx))
            .ok();
    }))
}

fn received(
    editor: &mut Editor,
    id: usize,
    blocks: Vec<Block>,
    range: Option<Range<usize>>,
    cx: &mut Context<Editor>,
) {
    let Some(popup) = editor.hover.popup.as_mut().filter(|popup| popup.id == id) else {
        return;
    };
    popup.request = None;
    popup.blocks = blocks;
    // The server knows the symbol better than our word boundaries (`r#type`, `a::b`).
    if let Some(range) =
        range.filter(|range| !range.is_empty() && range.contains(&popup.range.start))
    {
        popup.range = range;
    }
    if popup.blocks.is_empty() && popup.diagnostics.is_empty() {
        let keyboard = popup.trigger == Trigger::Keyboard;
        editor.hover.popup = None;
        if keyboard {
            editor.show_status(tr("No documentation").into(), cx);
        }
    }
    cx.notify();
}

fn hover_blocks(contents: HoverContents) -> Vec<Block> {
    let mut blocks = match contents {
        HoverContents::Scalar(marked) => marked_blocks(marked),
        HoverContents::Array(items) => {
            let mut blocks = Vec::new();
            for marked in items {
                blocks.push(Block::Rule);
                blocks.extend(marked_blocks(marked));
            }
            blocks
        }
        HoverContents::Markup(content) => markdown::markup(&content),
    };
    markdown::tidy(&mut blocks);
    blocks
}

fn marked_blocks(marked: MarkedString) -> Vec<Block> {
    match marked {
        MarkedString::String(text) => markdown::parse(&text),
        MarkedString::LanguageString(code) => vec![Block::Code(CodeBlock {
            language: Some(code.language.to_ascii_lowercase()),
            text: code.value.trim_end_matches('\n').to_string(),
            highlights: Vec::new(),
        })],
    }
}

/// The character under a point of the window: over the text of a line, not past its end.
fn position_under(editor: &Editor, point: Point<Pixels>) -> Option<usize> {
    let layout = editor.layout.as_ref()?;
    if !layout.text_bounds.contains(&point) {
        return None;
    }
    let text = editor.document.text();
    let row = ((point.y - layout.origin.y) / layout.line_height).floor();
    if row < 0. {
        return None;
    }
    let line = row as usize;
    if line >= text.len_lines() {
        return None;
    }
    let line_layout = layout.line(line)?;
    let x = point.x - layout.origin.x;
    if x < px(0.) {
        return None;
    }
    let byte = line_layout.shaped.index_for_x(x)?;
    let column = line_layout
        .char_to_byte
        .partition_point(|&b| b <= byte)
        .checked_sub(1)?;
    (column < line_len(text, line)).then(|| line_start(text, line) + column)
}

/// The symbol at `pos`: its word, or the character itself if a diagnostic is there.
fn symbol_range(editor: &Editor, pos: usize) -> Option<Range<usize>> {
    word_at(editor, pos)
        .or_else(|| (!editor.diagnostics.at(pos).is_empty()).then_some(pos..pos + 1))
}

fn word_at(editor: &Editor, pos: usize) -> Option<Range<usize>> {
    let text = editor.document.text();
    (char_class(text.char(pos)) == CharClass::Word).then(|| {
        let word = movement::word_range_at(text, pos);
        word.from()..word.to()
    })
}

/// ⌘ alone (as for ⌘-click) over a word of a document with a language server: that word is the
/// link.
fn update_link(editor: &mut Editor, modifiers: Modifiers, cx: &mut Context<Editor>) {
    let command = modifiers.platform && !modifiers.shift && !modifiers.alt && !modifiers.control;
    let link = editor
        .hover
        .mouse_over
        .filter(|_| command && crate::lsp::document_server(editor).is_some())
        .and_then(|pos| word_at(editor, pos));
    if link != editor.hover.link {
        editor.hover.link = link;
        cx.notify();
    }
}

/// The link under the mouse: an accent underline and the pointing hand over the word. Doesn't
/// take the mouse: the click goes to the editor (⌘-click).
fn render_link(editor: &Editor, window: &Window, cx: &App) -> Option<AnyElement> {
    let link = editor.hover.link.as_ref()?;
    if !window.modifiers().platform {
        return None;
    }
    let start = popup::anchor(editor, link.start)?;
    let end = popup::anchor(editor, link.end)?;
    if start.line_top != end.line_top {
        return None;
    }
    let ui = Theme::ui(cx);
    Some(
        anchored()
            .position(point(start.x, start.line_top))
            .child(
                div()
                    .w(end.x - start.x)
                    .h(start.line_bottom - start.line_top)
                    .border_b_1()
                    .border_color(ui.accent_text)
                    .cursor(CursorStyle::PointingHand),
            )
            .into_any_element(),
    )
}

fn schedule_hide(editor: &mut Editor, cx: &mut Context<Editor>) {
    if editor.hover.hide_task.is_some() {
        return;
    }
    editor.hover.hide_task = Some(cx.spawn(async move |this, cx| {
        cx.background_executor().timer(HIDE_DELAY).await;
        this.update(cx, |editor, cx| {
            let hover = &mut editor.hover;
            hover.hide_task = None;
            let Some(popup) = &hover.popup else {
                return;
            };
            let stays = hover.over_popup
                || hover
                    .mouse_over
                    .is_some_and(|position| popup.range.contains(&position));
            if popup.trigger == Trigger::Mouse && !stays {
                hover.popup = None;
                cx.notify();
            }
        })
        .ok();
    }));
}

/// From the popup's mouse watcher: where the mouse is relative to the popup and the text.
fn watch_mouse(editor: &mut Editor, over_popup: bool, over_text: bool, cx: &mut Context<Editor>) {
    let hover = &mut editor.hover;
    hover.over_popup = over_popup;
    if over_text || over_popup {
        return;
    }
    // Out of the editor (another panel, the title bar): no symbol under the mouse anymore.
    hover.mouse_over = None;
    if hover
        .popup
        .as_ref()
        .is_some_and(|popup| popup.trigger == Trigger::Mouse)
    {
        schedule_hide(editor, cx);
    }
}

fn is_visible(editor: &Editor, window: &Window) -> bool {
    let Some(popup) = &editor.hover.popup else {
        return false;
    };
    (!popup.blocks.is_empty() || !popup.diagnostics.is_empty())
        && popup.selection == *editor.document.selection()
        && editor.focus_handle.is_focused(window)
        && popup::anchor(editor, popup.range.start).is_some()
}

/// The link under the mouse, and the popup above the symbol's line (below it when there is no
/// room above).
pub fn render(editor: &Editor, window: &mut Window, cx: &mut Context<Editor>) -> Vec<AnyElement> {
    let link = render_link(editor, window, cx);
    link.into_iter()
        .chain(render_popup(editor, window, cx))
        .collect()
}

fn render_popup(
    editor: &Editor,
    window: &mut Window,
    cx: &mut Context<Editor>,
) -> Option<AnyElement> {
    if !is_visible(editor, window) {
        return None;
    }
    let popup = editor.hover.popup.as_ref()?;
    let anchor = popup::anchor(editor, popup.range.start)?;
    if editor.autoscroll.is_some() {
        window.request_animation_frame();
    }
    let actions = problem_actions(editor, popup, cx);
    let theme = Theme::get(cx);
    let ui = theme.ui;
    let (above, below) = popup::space(&anchor, window.viewport_size().height);
    let side = if above >= px(MIN_ROOM_ABOVE) || above > below {
        Side::Above
    } else {
        Side::Below
    };
    let room = match side {
        Side::Above => above,
        Side::Below => below,
    };

    // The mouse is watched over the whole window while the popup is shown: leaving the text and
    // the popup hides it, even if the mouse never moves over the text again.
    let editor_entity = cx.entity();
    let text_bounds = editor.layout.as_ref()?.text_bounds;
    let watcher = canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let over_popup = bounds.contains(&event.position);
                let over_text = text_bounds.contains(&event.position);
                editor_entity.update(cx, |editor, cx| {
                    watch_mouse(editor, over_popup, over_text, cx)
                });
            });
        },
    )
    .absolute()
    .inset_0();

    let content = div()
        .id(("hover", popup.id))
        .max_w(px(MAX_WIDTH))
        .max_h(room.min(px(MAX_HEIGHT)))
        .overflow_y_scroll()
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .children(popup.diagnostics.iter().map(|d| diagnostic_row(d, ui)))
        .children(actions)
        .when(
            !popup.diagnostics.is_empty() && !popup.blocks.is_empty(),
            |content| content.child(ui::divider(ui)),
        )
        .when(!popup.blocks.is_empty(), |content| {
            content.child(markdown::render(&popup.blocks, ui.foreground, theme))
        });
    let panel = popup::panel(ui).occlude().child(content).child(watcher);
    let x = anchor.x - px(TEXT_INSET);
    let (corner, y) = match side {
        Side::Below => (Corner::TopLeft, anchor.line_bottom + px(POPUP_GAP)),
        Side::Above => (Corner::BottomLeft, anchor.line_top - px(POPUP_GAP)),
    };
    Some(
        deferred(
            anchored()
                .anchor(corner)
                .position(point(x, y))
                .snap_to_window_with_margin(px(WINDOW_MARGIN))
                .child(panel),
        )
        .with_priority(1)
        .into_any_element(),
    )
}

/// Under the problems: "Fix with Claude" (while Claude Code is on and the document is a file) and
/// "More actions… ⌥↵" — the context actions at the problem (the caret goes there first).
fn problem_actions(
    editor: &Editor,
    popup: &Popup,
    cx: &mut Context<Editor>,
) -> Option<AnyElement> {
    let problems: Vec<&Diagnostic> = popup
        .diagnostics
        .iter()
        .filter(|d| d.severity != Severity::Hint)
        .collect();
    let first = problems.first()?;
    let ui = Theme::ui(cx);
    let link = |id: &'static str| {
        div()
            .id((id, popup.id))
            .flex()
            .items_center()
            .gap_1()
            .px_1()
            .py_0p5()
            .rounded(px(ui::RADIUS_SM))
            .cursor_pointer()
            .text_color(ui.accent_text)
            .hover(move |style| style.bg(ui.hover))
    };
    let fix = (crate::claude_actions::offered(cx))
        .then(|| editor.document.path().map(|path| path.to_path_buf()))
        .flatten()
        .map(|path| {
            let action = crate::claude_actions::FixProblemsWithClaude {
                path,
                problems: problems
                    .iter()
                    .map(|d| crate::claude_actions::Problem::of(d, editor))
                    .collect(),
            };
            link("hover-fix-with-claude")
                .child(icon(IconName::Claude, ui.accent_text).size(px(13.)))
                .child(tr("Fix with Claude"))
                .on_click(move |_, window, cx| window.dispatch_action(Box::new(action.clone()), cx))
        });
    let start = first.range.start;
    let more = link("hover-more-actions")
        .child(tr("More actions…"))
        .child(ui::keys("⌥↵", ui))
        .on_click(cx.listener(move |editor, _, window, cx| {
            editor.hover.popup = None;
            editor.select_range(start..start, cx);
            window.dispatch_action(Box::new(crate::code_actions::ShowContextActions), cx);
        }));
    Some(
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .ml(px(-4.))
            .text_size(px(theme::TEXT_SM))
            .children(fix)
            .child(more)
            .into_any_element(),
    )
}

fn diagnostic_row(diagnostic: &Diagnostic, ui: UiColors) -> impl IntoElement {
    let name = match diagnostic.severity {
        Severity::Error => IconName::Error,
        Severity::Warning => IconName::Warning,
        Severity::Info | Severity::Hint => IconName::Info,
    };
    let color = diagnostic.severity.color(&ui).unwrap_or(ui.dim);
    let source = match (&diagnostic.source, &diagnostic.code) {
        (Some(source), Some(code)) => Some(format!("{source}({code})")),
        (Some(source), None) => Some(source.clone()),
        (None, Some(code)) => Some(code.clone()),
        (None, None) => None,
    };
    div()
        .flex()
        .items_start()
        .gap_2()
        .child(icon(name, color).size(px(14.)).mt(px(2.)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(diagnostic.message.clone())
                .children(source.map(|source| {
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .child(source)
                })),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_lsp::lsp_types::{LanguageString, MarkupContent, MarkupKind};

    #[test]
    fn hover_contents_of_every_shape_become_blocks() {
        let markup = HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: "```rust\nfn len(&self) -> usize\n```\n\n---\n\nReturns the length.".into(),
        });
        let blocks = hover_blocks(markup);
        assert_eq!(blocks.len(), 3, "{blocks:#?}");
        assert!(matches!(&blocks[0], Block::Code(code) if code.text == "fn len(&self) -> usize"));
        assert_eq!(blocks[1], Block::Rule);

        // Old-style marked strings: a code block and Markdown, separated; no leading rule.
        let array = HoverContents::Array(vec![
            MarkedString::LanguageString(LanguageString {
                language: "Go".into(),
                value: "func Println(a ...any)\n".into(),
            }),
            MarkedString::String("Println formats.".into()),
        ]);
        let blocks = hover_blocks(array);
        assert_eq!(blocks.len(), 3, "{blocks:#?}");
        assert!(
            matches!(&blocks[0], Block::Code(code) if code.language.as_deref() == Some("go")
                && code.text == "func Println(a ...any)")
        );
        assert_eq!(blocks[1], Block::Rule);

        let plain = HoverContents::Markup(MarkupContent {
            kind: MarkupKind::PlainText,
            value: "x: *int".into(),
        });
        assert!(
            matches!(&hover_blocks(plain)[0], Block::Paragraph(spans) if spans[0].text == "x: *int")
        );
        let empty = HoverContents::Scalar(MarkedString::String("  \n".into()));
        assert!(hover_blocks(empty).is_empty());
    }
}
