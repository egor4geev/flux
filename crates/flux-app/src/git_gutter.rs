//! Git in the editor: the HEAD version of the document (from `GitStore`) and the changed blocks of
//! the unsaved text against it, as markers in the gutter — added green, modified blue, deleted a gray
//! wedge between lines, as in JetBrains IDEs. A click on a marker shows the change: what it
//! replaced, with Previous / Next, Rollback, Show Diff and Copy. ⌥⌘Z rolls back the changes under the
//! cursors, ⌃⌥⇧↓ / ⌃⌥⇧↑ go to the next / previous change.
//!
//! The blocks are computed in the background, a moment after the last keystroke; meanwhile the old
//! ones follow the edits (their lines are mapped through the change), so the markers don't jump.
//! Line endings: the HEAD version and the text are compared without `\r` before `\n`, so a file
//! checked out with CRLF (`core.autocrlf`) isn't all changed.

use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use flux_core::text::line_start;
use flux_core::{Assoc, ChangeSet, Rope, TextChange};
use flux_git::{Hunk, HunkKind};
use gpui::{
    AnyElement, App, Bounds, ClickEvent, ClipboardItem, Context, Corner, CursorStyle,
    DispatchPhase, Div, HighlightStyle, Hitbox, HitboxBehavior, Hsla, KeyBinding, KeyContext,
    MouseDownEvent, MouseMoveEvent, PaintQuad, Path, PathBuilder, Pixels, StyledText, Task,
    WeakEntity, Window, actions, anchored, deferred, div, fill, point, prelude::*, px, size,
};

use crate::editor::Editor;
use crate::element::LayoutCache;
use crate::git::GitStore;
use crate::i18n::{tr, trf};
use crate::icons::IconName;
use crate::popup::{self, Anchor, POPUP_GAP, Side, WINDOW_MARGIN};
use crate::theme::{self, Theme, UiColors};
use crate::ui;

actions!(
    git,
    [
        /// ⌥⌘Z: the changes under the cursors go back to HEAD.
        RollbackLines,
        /// ⌃⌥⇧↓ / ⌃⌥⇧↑.
        NextChange,
        PreviousChange,
        /// The popup of the change under the cursor (as a click on its marker).
        ShowChange,
        /// Esc while the popup is shown.
        HideChange,
    ]
);

/// A marker: its width, wider under the mouse; its right edge keeps this gap from the text area.
const MARKER_WIDTH: f32 = 3.;
const MARKER_HOVER_WIDTH: f32 = 5.;
const MARKER_GAP: f32 = 3.;
/// The deleted wedge: half its height and its depth.
const WEDGE_HALF: f32 = 4.;
const WEDGE_DEPTH: f32 = 5.;
/// The click area of a marker reaches this far left of the text area (over the line numbers'
/// padding), and a deleted wedge this far above and below its line boundary.
const HIT_LEFT: f32 = 12.;
const HIT_RIGHT: f32 = 2.;
const HIT_WEDGE: f32 = 5.;
/// Typing is coalesced for this long before the blocks are computed again.
const RECOMPUTE_DELAY: Duration = Duration::from_millis(30);
/// Larger documents get no markers.
const MAX_TEXT_CHARS: usize = 4 * 1024 * 1024;
/// The popup: at most this many lines of the old text are shown (it scrolls), this wide at most.
const POPUP_MAX_LINES: usize = 14;
const POPUP_MAX_WIDTH: f32 = 720.;
const POPUP_LINE_HEIGHT: f32 = 18.;
const POPUP_TOOLBAR_HEIGHT: f32 = 34.;

pub fn init(cx: &mut App) {
    let editor = Some("Editor");
    cx.bind_keys([
        // As in JetBrains IDEs (macOS keymap): Rollback, Next / Previous Change.
        KeyBinding::new("alt-cmd-z", RollbackLines, editor),
        KeyBinding::new("ctrl-alt-shift-down", NextChange, editor),
        KeyBinding::new("ctrl-alt-shift-up", PreviousChange, editor),
        KeyBinding::new("escape", HideChange, Some("Editor && showing_change")),
    ]);
}

/// The editor's git state.
#[derive(Default)]
pub struct GitState {
    /// The window's git hub, once the editor is registered with it.
    pub store: Option<WeakEntity<GitStore>>,
    /// The HEAD version, without `\r` before `\n`; `None` — no markers (not in a repository, a new
    /// file, binary, too large).
    pub base: Option<Arc<str>>,
    /// Changed blocks of the text against the base, in order. While a computation is pending, the
    /// previous ones shifted by the edits.
    pub hunks: Vec<Hunk>,
    /// Bumped by every change of the text or the base: a result computed for an older one is
    /// dropped.
    version: u64,
    /// The hunks were computed for the current text.
    fresh: bool,
    task: Option<Task<()>>,
    popup: Option<ChangePopup>,
}

impl GitState {
    /// The hunks, exact for the current text: a pending computation is done now (actions that act on
    /// lines can't use shifted ones).
    pub fn exact_hunks<'a>(&'a mut self, text: &Rope) -> &'a [Hunk] {
        if !self.fresh {
            self.hunks = match &self.base {
                Some(base) => compute(base, text),
                None => Vec::new(),
            };
            self.fresh = true;
            self.version += 1;
            self.task = None;
        }
        &self.hunks
    }
}

/// The popup of one change.
struct ChangePopup {
    hunk: Hunk,
    /// The number of the change among all, from 1, and how many there are.
    number: usize,
    total: usize,
    /// The HEAD lines the change replaced (tabs expanded, no final line break); empty for added
    /// lines.
    old_text: String,
    /// The same as in HEAD, for Copy.
    old_raw: String,
    /// Changed words of a modified block: byte ranges in `old_text`.
    changed: Vec<Range<usize>>,
}

/// The HEAD version arrived (or changed: a commit, a checkout): the blocks are computed anew.
pub fn set_base(editor: &mut Editor, base: Option<Arc<str>>, cx: &mut Context<Editor>) {
    editor.git.base = base.map(|base| match base.contains('\r') {
        true => Arc::from(base.replace("\r\n", "\n")),
        false => base,
    });
    editor.git.popup = None;
    if editor.git.base.is_none() {
        editor.git.hunks.clear();
        editor.git.task = None;
        editor.git.fresh = true;
        editor.git.version += 1;
        return cx.notify();
    }
    schedule(editor, Duration::ZERO, cx);
}

/// The text changed: the blocks follow the edit now and are computed again a moment later.
pub fn text_changed(editor: &mut Editor, changes: &[TextChange], cx: &mut Context<Editor>) {
    editor.git.popup = None;
    if editor.git.base.is_none() {
        return;
    }
    let text = editor.document.text().clone();
    for (index, change) in changes.iter().enumerate() {
        let after = changes.get(index + 1).map_or(&text, |next| &next.old_text);
        shift_hunks(
            &mut editor.git.hunks,
            &change.old_text,
            after,
            &change.changes,
        );
    }
    schedule(editor, RECOMPUTE_DELAY, cx);
}

/// The document's path changed (Save As, a rename in the tree): its HEAD version is another file's.
pub fn path_changed(editor: &mut Editor, cx: &mut Context<Editor>) {
    let Some(store) = editor.git.store.clone() else {
        return;
    };
    let this = cx.entity();
    editor.git.base = None;
    editor.git.hunks.clear();
    editor.git.popup = None;
    editor.git.task = None;
    editor.git.version += 1;
    cx.defer(move |cx| {
        store
            .update(cx, |store, cx| store.load_base(&this, cx))
            .ok();
    });
}

/// Computes the blocks in the background after `delay`; a newer request replaces this one.
fn schedule(editor: &mut Editor, delay: Duration, cx: &mut Context<Editor>) {
    let Some(base) = editor.git.base.clone() else {
        return;
    };
    editor.git.version += 1;
    editor.git.fresh = false;
    let version = editor.git.version;
    let text = editor.document.text().clone();
    editor.git.task = Some(cx.spawn(async move |this, cx| {
        if !delay.is_zero() {
            cx.background_executor().timer(delay).await;
        }
        let hunks = cx
            .background_executor()
            .spawn(async move { compute(&base, &text) })
            .await;
        this.update(cx, |editor, cx| {
            if editor.git.version == version {
                editor.git.hunks = hunks;
                editor.git.fresh = true;
                cx.notify();
            }
        })
        .ok();
    }));
}

/// The blocks of `text` against the (normalized) base.
fn compute(base: &str, text: &Rope) -> Vec<Hunk> {
    if text.len_chars() > MAX_TEXT_CHARS {
        return Vec::new();
    }
    let text = text.to_string();
    let text = match text.contains('\r') {
        true => text.replace("\r\n", "\n"),
        false => text,
    };
    flux_git::diff_lines(base, &text)
}

/// Moves the blocks' lines through an edit (`old` → `new` text): the markers stay with their lines
/// until the blocks are computed again.
fn shift_hunks(hunks: &mut [Hunk], old: &Rope, new: &Rope, changes: &ChangeSet) {
    let map = |line: u32| -> u32 {
        let line = line as usize;
        if line >= old.len_lines() {
            return new.len_lines() as u32;
        }
        let pos = changes.map_pos(line_start(old, line), Assoc::Before);
        new.char_to_line(pos.min(new.len_chars())) as u32
    };
    for hunk in hunks.iter_mut() {
        let start = map(hunk.new.start);
        let end = if hunk.new.is_empty() {
            start
        } else {
            map(hunk.new.end).max(start)
        };
        hunk.new = start..end;
    }
}

// --- Markers ---

/// The markers of the visible lines.
#[derive(Default)]
pub struct GutterPaint {
    quads: Vec<PaintQuad>,
    wedges: Vec<(Path<Pixels>, Hsla)>,
    hitboxes: Vec<Hitbox>,
    /// Click areas, and the one under the mouse when the frame was laid out.
    areas: Vec<Bounds<Pixels>>,
    hovered: Option<usize>,
}

impl GutterPaint {
    pub fn paint(&mut self, window: &mut Window) {
        for quad in self.quads.drain(..) {
            window.paint_quad(quad);
        }
        for (wedge, color) in self.wedges.drain(..) {
            window.paint_path(wedge, color);
        }
        for hitbox in &self.hitboxes {
            window.set_cursor_style(CursorStyle::PointingHand, hitbox);
        }
        if self.areas.is_empty() {
            return;
        }
        // The marker under the mouse is drawn wider: redraw when the mouse moves onto another one.
        let areas = std::mem::take(&mut self.areas);
        let hovered = self.hovered;
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, _| {
            if phase == DispatchPhase::Bubble
                && areas.iter().position(|area| area.contains(&event.position)) != hovered
            {
                window.refresh();
            }
        });
    }
}

/// The click area of a block's marker.
fn marker_area(hunk: &Hunk, layout: &LayoutCache, gutter_right: Pixels) -> Bounds<Pixels> {
    let top = layout.line_top(hunk.new.start as usize);
    let (top, bottom) = match hunk.kind() {
        HunkKind::Deleted => (top - px(HIT_WEDGE), top + px(HIT_WEDGE)),
        _ => (top, layout.line_top(hunk.new.end as usize)),
    };
    Bounds::from_corners(
        point(gutter_right - px(HIT_LEFT), top),
        point(gutter_right + px(HIT_RIGHT), bottom),
    )
}

/// Markers for the blocks that touch the visible lines, at the right edge of the gutter.
pub fn prepaint(
    state: &GitState,
    layout: &LayoutCache,
    last_line: usize,
    gutter: Bounds<Pixels>,
    _em: Pixels,
    ui: &UiColors,
    window: &mut Window,
) -> GutterPaint {
    let mut paint = GutterPaint::default();
    if gutter.size.width <= px(0.) || state.hunks.is_empty() {
        return paint;
    }
    let right = gutter.right() - px(MARKER_GAP);
    let mouse = window.mouse_position();
    let first = layout.first_line as u32;
    let last = last_line as u32;
    for hunk in &state.hunks {
        if hunk.new.end < first || hunk.new.start > last {
            continue;
        }
        let area = marker_area(hunk, layout, gutter.right());
        let hovered = gutter.contains(&mouse) && area.contains(&mouse);
        let top = layout.line_top(hunk.new.start as usize);
        match hunk.kind() {
            HunkKind::Deleted => {
                let (half, depth) = match hovered {
                    true => (WEDGE_HALF + 1.5, WEDGE_DEPTH + 2.),
                    false => (WEDGE_HALF, WEDGE_DEPTH),
                };
                let left = right - px(depth);
                let mut wedge = PathBuilder::fill();
                wedge.move_to(point(left, top - px(half)));
                wedge.line_to(point(right, top));
                wedge.line_to(point(left, top + px(half)));
                wedge.close();
                if let Ok(path) = wedge.build() {
                    paint.wedges.push((path, ui.diff_deleted));
                }
            }
            kind => {
                let width = match hovered {
                    true => MARKER_HOVER_WIDTH,
                    false => MARKER_WIDTH,
                };
                let height = layout.line_top(hunk.new.end as usize) - top;
                let color = match kind {
                    HunkKind::Added => ui.diff_added,
                    _ => ui.diff_modified,
                };
                paint.quads.push(fill(
                    Bounds::new(point(right - px(width), top), size(px(width), height)),
                    color,
                ));
            }
        }
        if hovered {
            paint.hovered = Some(paint.areas.len());
        }
        paint
            .hitboxes
            .push(window.insert_hitbox(area, HitboxBehavior::Normal));
        paint.areas.push(area);
    }
    paint
}

/// A click in the gutter on a marker: shows its change. `true` — the click was taken.
pub fn mouse_down(
    editor: &mut Editor,
    event: &MouseDownEvent,
    _window: &mut Window,
    cx: &mut Context<Editor>,
) -> bool {
    if editor.message.is_some() || editor.git.hunks.is_empty() || event.click_count != 1 {
        return false;
    }
    let Some(layout) = &editor.layout else {
        return false;
    };
    let gutter_right = layout.text_bounds.left();
    let found = editor
        .git
        .hunks
        .iter()
        .position(|hunk| marker_area(hunk, layout, gutter_right).contains(&event.position));
    let Some(index) = found else {
        return false;
    };
    open_popup(editor, index, cx);
    true
}

// --- The change popup ---

/// Adds `showing_change` to the editor's key context while the popup is shown (Esc hides it).
pub fn extend_key_context(editor: &Editor, context: &mut KeyContext) {
    if editor.git.popup.is_some() {
        context.add("showing_change");
    }
}

/// Shows the popup of the change at `index`.
fn open_popup(editor: &mut Editor, index: usize, cx: &mut Context<Editor>) {
    let (Some(base), Some(hunk)) = (
        editor.git.base.clone(),
        editor.git.hunks.get(index).cloned(),
    ) else {
        return;
    };
    let old_raw = line_span(&base, hunk.old.clone()).to_string();
    let old_text = display_lines(&old_raw);
    let changed = match hunk.kind() {
        HunkKind::Modified => {
            let text = editor.document.text();
            let start = line_char(text, hunk.new.start);
            let end = line_char(text, hunk.new.end);
            let new_raw = text.slice(start..end).to_string().replace("\r\n", "\n");
            flux_git::diff_words(&old_text, &display_lines(&new_raw)).0
        }
        _ => Vec::new(),
    };
    editor.git.popup = Some(ChangePopup {
        hunk,
        number: index + 1,
        total: editor.git.hunks.len(),
        old_text,
        old_raw,
        changed,
    });
    cx.notify();
}

/// The popup of the change under the cursor (the palette's "Show Change").
fn show_at_cursor(editor: &mut Editor, cx: &mut Context<Editor>) {
    let lines = cursor_lines(editor);
    let text = editor.document.text().clone();
    let hunks = editor.git.exact_hunks(&text);
    match hunks
        .iter()
        .position(|hunk| touches(hunk, lines.0, lines.1))
    {
        Some(index) => open_popup(editor, index, cx),
        None => editor.show_status(tr("No changes at the cursor").into(), cx),
    }
}

fn hide_popup(editor: &mut Editor, cx: &mut Context<Editor>) {
    if editor.git.popup.take().is_some() {
        cx.notify();
    }
}

/// The popup under the change (above it when there is no room), at the left edge of the text: a
/// toolbar, then the lines the change replaced.
pub fn render(
    editor: &Editor,
    window: &mut Window,
    cx: &mut Context<Editor>,
) -> Option<AnyElement> {
    let popup = editor.git.popup.as_ref()?;
    let layout = editor.layout.as_ref()?;
    let ui = Theme::ui(cx);
    let bounds = layout.text_bounds;
    let top = layout.line_top(popup.hunk.new.start as usize);
    let bottom = layout.line_top(popup.hunk.new.end as usize);
    // The change scrolled out of view: the popup waits for it.
    if bottom < bounds.top() || top > bounds.bottom() {
        return None;
    }
    let anchor = Anchor {
        x: bounds.left(),
        line_top: top.max(bounds.top()),
        line_bottom: bottom.min(bounds.bottom()),
    };
    let lines = if popup.old_text.is_empty() {
        0
    } else {
        popup.old_text.lines().count().clamp(1, POPUP_MAX_LINES)
    };
    let height = px(POPUP_TOOLBAR_HEIGHT + lines as f32 * POPUP_LINE_HEIGHT + 16.);
    let (side, room) = popup::side(&anchor, height, window.viewport_size().height, Side::Below);

    let button =
        |id: &'static str, name: IconName, label: &'static str, action: &dyn gpui::Action| {
            ui::icon_button(id, name, ui)
                .tooltip(ui::tooltip(label, ui::shortcut_for(action, window)))
        };
    let toolbar = div()
        .flex_none()
        .h(px(POPUP_TOOLBAR_HEIGHT))
        .px_1p5()
        .flex()
        .items_center()
        .gap_0p5()
        .child(
            button(
                "change-previous",
                IconName::ArrowUp,
                tr("Previous Change"),
                &PreviousChange,
            )
            .on_click(cx.listener(|editor, _: &ClickEvent, _, cx| step_popup(editor, false, cx))),
        )
        .child(
            button(
                "change-next",
                IconName::ArrowDown,
                tr("Next Change"),
                &NextChange,
            )
            .on_click(cx.listener(|editor, _: &ClickEvent, _, cx| step_popup(editor, true, cx))),
        )
        .child(div().mx_1().w(px(1.)).h(px(16.)).bg(ui.divider))
        .child(
            button(
                "change-rollback",
                IconName::Rollback,
                tr("Rollback"),
                &RollbackLines,
            )
            .on_click(cx.listener(|editor, _: &ClickEvent, _, cx| {
                if let Some(popup) = editor.git.popup.take() {
                    rollback(editor, &[popup.hunk], cx);
                }
            })),
        )
        .child(
            button(
                "change-diff",
                IconName::Diff,
                tr("Show Diff"),
                &crate::git::ShowDiff,
            )
            .on_click(cx.listener(|editor, _: &ClickEvent, window, cx| {
                hide_popup(editor, cx);
                window.dispatch_action(Box::new(crate::git::ShowDiff), cx);
            })),
        )
        .when(!popup.old_raw.is_empty(), |toolbar| {
            toolbar.child(
                ui::text_button("change-copy", tr("Copy"), false, ui)
                    .ml_1()
                    .tooltip(ui::tooltip(tr("Copy the previous text"), None))
                    .on_click(cx.listener(|editor, _: &ClickEvent, _, cx| {
                        if let Some(popup) = editor.git.popup.take() {
                            cx.write_to_clipboard(ClipboardItem::new_string(popup.old_raw));
                            cx.notify();
                        }
                    })),
            )
        })
        .child(div().flex_1().min_w(px(12.)))
        .child(
            div()
                .flex_none()
                .pr_1p5()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.dim)
                .child(trf("{0} of {1}", &[&popup.number, &popup.total])),
        );
    let body = (!popup.old_text.is_empty()).then(|| {
        let (block, word) = match popup.hunk.kind() {
            HunkKind::Modified => (ui.diff_modified_bg, ui.diff_modified_word),
            _ => (ui.diff_deleted_bg, ui.diff_deleted_word),
        };
        let highlights: Vec<(Range<usize>, HighlightStyle)> = popup
            .changed
            .iter()
            .filter(|range| range.end <= popup.old_text.len())
            .map(|range| {
                let style = HighlightStyle {
                    background_color: Some(word),
                    ..Default::default()
                };
                (range.clone(), style)
            })
            .collect();
        div()
            .id("change-old-text")
            .mx_1p5()
            .mb_1p5()
            .max_h(room.min(px(POPUP_MAX_LINES as f32 * POPUP_LINE_HEIGHT + 12.)))
            .overflow_y_scroll()
            .px_2()
            .py_1p5()
            .rounded(px(ui::RADIUS_SM))
            .bg(block)
            .font_family(theme::code_font())
            .text_size(px(theme::TEXT_MD))
            .line_height(px(POPUP_LINE_HEIGHT))
            .whitespace_nowrap()
            .child(StyledText::new(popup.old_text.clone()).with_highlights(highlights))
    });
    let panel = popup::panel(ui)
        .occlude()
        .min_w(px(320.))
        .max_w(px(POPUP_MAX_WIDTH))
        .flex()
        .flex_col()
        .on_mouse_down_out(cx.listener(|editor, _: &MouseDownEvent, _, cx| hide_popup(editor, cx)))
        .child(toolbar)
        .children(body);
    let (corner, y) = match side {
        Side::Below => (Corner::TopLeft, anchor.line_bottom + px(POPUP_GAP)),
        Side::Above => (Corner::BottomLeft, anchor.line_top - px(POPUP_GAP)),
    };
    Some(
        deferred(
            anchored()
                .anchor(corner)
                .position(point(anchor.x, y))
                .snap_to_window_with_margin(px(WINDOW_MARGIN))
                .child(panel),
        )
        .with_priority(1)
        .into_any_element(),
    )
}

/// Previous / Next in the popup: the cursor goes to that change and the popup shows it.
fn step_popup(editor: &mut Editor, forward: bool, cx: &mut Context<Editor>) {
    if let Some(index) = go_to_change(editor, forward, cx) {
        open_popup(editor, index, cx);
    }
}

// --- Actions ---

/// The editor's git actions.
pub fn actions(root: Div, editor: &Editor, cx: &mut Context<Editor>) -> Div {
    root.on_action(cx.listener(|editor, _: &NextChange, _, cx| {
        go_to_change(editor, true, cx);
    }))
    .on_action(cx.listener(|editor, _: &PreviousChange, _, cx| {
        go_to_change(editor, false, cx);
    }))
    .on_action(cx.listener(|editor, _: &RollbackLines, _, cx| rollback_at_cursors(editor, cx)))
    .on_action(cx.listener(|editor, _: &ShowChange, _, cx| show_at_cursor(editor, cx)))
    .when(editor.git.popup.is_some(), |root| {
        root.on_action(cx.listener(|editor, _: &HideChange, _, cx| hide_popup(editor, cx)))
    })
}

/// Moves the cursor to the start of the next (previous) change, wrapping around; returns its index.
fn go_to_change(editor: &mut Editor, forward: bool, cx: &mut Context<Editor>) -> Option<usize> {
    let line = {
        let text = editor.document.text();
        text.char_to_line(editor.document.selection().primary().head) as u32
    };
    let text = editor.document.text().clone();
    let hunks = editor.git.exact_hunks(&text);
    let index = next_change(hunks, line, forward)?;
    let line = hunks[index].new.start as usize;
    hide_popup(editor, cx);
    let position = editor.position(line, 0);
    editor.select_range(position..position, cx);
    Some(index)
}

/// The change after (before) `line`, wrapping around.
fn next_change(hunks: &[Hunk], line: u32, forward: bool) -> Option<usize> {
    if hunks.is_empty() {
        return None;
    }
    if forward {
        hunks
            .iter()
            .position(|hunk| hunk.new.start > line)
            .or(Some(0))
    } else {
        hunks
            .iter()
            .rposition(|hunk| hunk.new.end.max(hunk.new.start + 1) <= line)
            .or(Some(hunks.len() - 1))
    }
}

/// ⌥⌘Z: the changes touched by the selections go back to their HEAD lines, in one edit (one undo).
fn rollback_at_cursors(editor: &mut Editor, cx: &mut Context<Editor>) {
    let ranges: Vec<(u32, u32)> = {
        let text = editor.document.text();
        editor
            .document
            .selection()
            .iter()
            .map(|range| selected_lines(text, range.from(), range.to()))
            .collect()
    };
    let text = editor.document.text().clone();
    let touched: Vec<Hunk> = editor
        .git
        .exact_hunks(&text)
        .iter()
        .filter(|hunk| {
            ranges
                .iter()
                .any(|(first, last)| touches(hunk, *first, *last))
        })
        .cloned()
        .collect();
    if touched.is_empty() {
        return editor.show_status(tr("No changes at the cursor").into(), cx);
    }
    rollback(editor, &touched, cx);
}

/// Replaces the changes' lines with their HEAD lines, in one edit.
fn rollback(editor: &mut Editor, hunks: &[Hunk], cx: &mut Context<Editor>) {
    let Some(base) = editor.git.base.clone() else {
        return;
    };
    let edits = rollback_edits(
        &base,
        editor.document.text(),
        editor.document.line_ending(),
        hunks,
    );
    editor.git.popup = None;
    editor.replace_ranges(edits, cx);
}

/// The edits that put `hunks` (exact for `text`) back to their lines in `base`: character ranges of
/// `text` with their replacement, in order. `ending` is the document's line break.
fn rollback_edits(
    base: &str,
    text: &Rope,
    ending: &str,
    hunks: &[Hunk],
) -> Vec<(Range<usize>, String)> {
    hunks
        .iter()
        .map(|hunk| {
            let start = line_char(text, hunk.new.start);
            let end = line_char(text, hunk.new.end);
            let old = line_span(base, hunk.old.clone());
            let old = match ending {
                "\n" => old.to_string(),
                ending => old.replace('\n', ending),
            };
            (start..end, old)
        })
        .collect()
}

/// Whether a change touches the lines `first..=last` (a deletion — the line right after it).
fn touches(hunk: &Hunk, first: u32, last: u32) -> bool {
    hunk.new.start <= last && hunk.new.end.max(hunk.new.start + 1) > first
}

/// The lines of the primary cursor's selection.
fn cursor_lines(editor: &Editor) -> (u32, u32) {
    let text = editor.document.text();
    let primary = editor.document.selection().primary();
    selected_lines(text, primary.from(), primary.to())
}

/// The lines a selection covers; a selection that ends at the start of a line doesn't take it.
fn selected_lines(text: &Rope, from: usize, to: usize) -> (u32, u32) {
    let first = text.char_to_line(from);
    let mut last = text.char_to_line(to);
    if to > from && last > first && line_start(text, last) == to {
        last -= 1;
    }
    (first as u32, last as u32)
}

/// The character where a line starts; past the last line, the end of the text.
fn line_char(text: &Rope, line: u32) -> usize {
    let line = line as usize;
    if line >= text.len_lines() {
        text.len_chars()
    } else {
        line_start(text, line)
    }
}

/// The lines `range` of a text, with their line breaks.
fn line_span(text: &str, range: Range<u32>) -> &str {
    let mut starts = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .chain(std::iter::once(text.len()));
    let start = starts
        .clone()
        .nth(range.start as usize)
        .unwrap_or(text.len());
    let end = starts
        .nth(range.end as usize)
        .unwrap_or(text.len())
        .max(start);
    &text[start.min(text.len())..end.min(text.len())]
}

/// Lines for the popup: tabs expanded, without the final line break.
fn display_lines(text: &str) -> String {
    let text = text.strip_suffix('\n').unwrap_or(text);
    text.replace('\t', &" ".repeat(theme::TAB_WIDTH))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(old: Range<u32>, new: Range<u32>) -> Hunk {
        Hunk { old, new }
    }

    #[test]
    fn spans_of_lines() {
        let text = "a\nb\nc";
        assert_eq!(line_span(text, 0..1), "a\n");
        assert_eq!(line_span(text, 1..3), "b\nc");
        assert_eq!(line_span(text, 2..2), "");
        assert_eq!(line_span(text, 3..3), "");
        assert_eq!(line_span("", 0..0), "");
    }

    #[test]
    fn rollback_puts_head_lines_back() {
        let base = "a\nb\nc\nd\ne\n";
        let current = "a\nB\nc\nx\ny\nd\n";
        let text = Rope::from_str(current);
        let hunks = flux_git::diff_lines(base, current);
        // Everything back: the text becomes the base.
        let mut rope = text.clone();
        for (range, old) in rollback_edits(base, &text, "\n", &hunks).into_iter().rev() {
            rope.remove(range.clone());
            rope.insert(range.start, &old);
        }
        assert_eq!(rope.to_string(), base);
        // Only the deletion of "e": it comes back at the end.
        let edits = rollback_edits(base, &text, "\n", &hunks[2..]);
        assert_eq!(edits, vec![(12..12, "e\n".to_string())]);
        // A CRLF document gets CRLF lines back.
        let crlf = Rope::from_str("a\r\nB\r\nc\r\n");
        let hunks = flux_git::diff_lines("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(
            rollback_edits("a\nb\nc\n", &crlf, "\r\n", &hunks),
            vec![(3..6, "b\r\n".to_string())]
        );
    }

    #[test]
    fn changes_under_the_cursors() {
        // Lines 3–4 modified, a deletion before line 7.
        let modified = hunk(3..5, 3..5);
        let deleted = hunk(8..9, 7..7);
        assert!(touches(&modified, 3, 3));
        assert!(touches(&modified, 4, 6));
        assert!(!touches(&modified, 5, 6));
        assert!(!touches(&modified, 0, 2));
        // A deletion is touched from the line after it.
        assert!(touches(&deleted, 7, 7));
        assert!(!touches(&deleted, 6, 6));
        assert!(!touches(&deleted, 8, 9));
        let text = Rope::from_str("one\ntwo\nthree\n");
        // A selection ending at the start of a line doesn't take that line.
        assert_eq!(selected_lines(&text, 0, 4), (0, 0));
        assert_eq!(selected_lines(&text, 0, 5), (0, 1));
        assert_eq!(selected_lines(&text, 5, 5), (1, 1));
    }

    #[test]
    fn next_and_previous_changes_wrap() {
        let hunks = vec![hunk(1..2, 1..2), hunk(5..5, 6..8), hunk(9..10, 12..12)];
        assert_eq!(next_change(&hunks, 0, true), Some(0));
        assert_eq!(next_change(&hunks, 1, true), Some(1));
        assert_eq!(next_change(&hunks, 12, true), Some(0));
        assert_eq!(next_change(&hunks, 12, false), Some(1));
        assert_eq!(next_change(&hunks, 13, false), Some(2));
        assert_eq!(next_change(&hunks, 0, false), Some(2));
        assert_eq!(next_change(&[], 0, true), None);
    }

    #[test]
    fn blocks_follow_edits_before_they_are_computed_again() {
        let old = Rope::from_str("a\nb\nc\nd\n");
        // A modified block on line 2 ("c"), a deletion before line 3.
        let mut hunks = vec![hunk(2..3, 2..3), hunk(4..5, 3..3)];
        // Two lines typed at the top.
        let changes = ChangeSet::from_changes(old.len_chars(), [(0, 0, Some("x\ny\n".into()))]);
        let mut new = old.clone();
        changes.apply(&mut new);
        shift_hunks(&mut hunks, &old, &new, &changes);
        assert_eq!(hunks, vec![hunk(2..3, 4..5), hunk(4..5, 5..5)]);
        // A line removed inside the block shrinks it.
        let old = new;
        let start = line_start(&old, 4);
        let changes = ChangeSet::from_changes(old.len_chars(), [(start, start + 2, None)]);
        let mut new = old.clone();
        changes.apply(&mut new);
        shift_hunks(&mut hunks, &old, &new, &changes);
        assert_eq!(hunks, vec![hunk(2..3, 4..4), hunk(4..5, 4..4)]);
    }

    #[test]
    fn blocks_are_computed_without_carriage_returns() {
        let hunks = compute("a\nb\n", &Rope::from_str("a\r\nB\r\n"));
        assert_eq!(hunks, vec![hunk(1..2, 1..2)]);
        assert_eq!(display_lines("\tx\n"), "    x");
    }
}
