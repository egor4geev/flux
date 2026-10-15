//! The editor's scrollbar (part 10.5), as JetBrains IDEs draw it: a lane at the right edge with
//! marks of the whole document — Git changes in a thin column along its left side, problems across
//! the rest over find results, the caret's line across — a thumb that shows while the editor
//! scrolls or the mouse is over the lane (always, when macOS shows legacy scroll bars), and the
//! problems widget in the editor's top right corner (✖ 1 ⚠ 3 ↑ ↓; a green check when the file is
//! clean).
//!
//! Marks are laid out as IntelliJ lays out its error stripe (`EditorMarkupModelImpl`): the editor's
//! scrollable height — with the room after the last line — onto the lane, never stretched; a mark
//! of one line is 3 px tall. A click on a mark (or near it) puts the caret there, hovering shows what
//! the marks are. A click on the track pages towards it; ⌥-click, or macOS's "Jump to the spot
//! that's clicked", jumps there and keeps dragging. Rows come from the editor's
//! [`WrapMap`](crate::wrap::WrapMap): a soft-wrapped line takes several.
//!
//! A commit message field has no lane: a thin thumb over the text's right edge. Without soft wrap,
//! a thumb at the bottom scrolls sideways when the visible lines are wider than the text area.

use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use flux_core::text::line_start;
use flux_git::HunkKind;
use gpui::{
    AnyElement, App, Bounds, Context, Corner, CursorStyle, DispatchPhase, Element, ElementId,
    Entity, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, MouseButton, MouseDownEvent, MouseExitEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, Style, Task, Window, anchored, deferred, div, fill, point, prelude::*, px, relative,
    size,
};

use crate::diagnostics::{NextProblem, PreviousProblem, Severity};
use crate::editor::Editor;
use crate::i18n::{tr, trf, trn};
use crate::icons::{IconName, icon};
use crate::popup::{self, POPUP_GAP, WINDOW_MARGIN};
use crate::settings::MarkdownLayout;
use crate::theme::{self, Theme, UiColors};
use crate::ui;

pub fn init(_cx: &mut App) {}

/// The lane at the right edge of an editor: the text and its soft wraps end before it.
pub const LANE_WIDTH: f32 = 14.;
/// The lane's columns (from its left edge), as IntelliJ's thin and wide stripes: Git changes along
/// the left side; problems across the rest, over find results — narrower, in its middle.
const CHANGES: (f32, f32) = (1., 3.);
const PROBLEMS: (f32, f32) = (5., 8.);
const MATCHES: (f32, f32) = (6.5, 5.);
/// A mark is at least this tall (IntelliJ's minimal mark height).
const MIN_MARK: f32 = 3.;
/// The caret's line across the lane.
const CARET_HEIGHT: f32 = 2.;
/// Marks this close to the mouse, above or below, answer a hover or a click (IntelliJ: twice the
/// minimal mark height).
const REACH: f32 = 2. * MIN_MARK;
/// The track keeps this far from the ends of the lane.
const TRACK_MARGIN: f32 = 2.;
/// The thumb: this far from the lane's sides; at least this long.
const THUMB_INSET: f32 = 2.;
const MIN_THUMB: f32 = 24.;
/// A field's thumb over the text: its width, and the strip at the edge where the mouse shows it.
const FIELD_THUMB: f32 = 6.;
const FIELD_STRIP: f32 = 10.;
/// The sideways thumb: its thickness, the strip at the bottom where the mouse shows it.
const BOTTOM_THUMB: f32 = 7.;
const BOTTOM_STRIP: f32 = 12.;
/// How long a thumb stays after the editor stops scrolling (or the mouse leaves it), and how long
/// it then fades.
const HOLD: Duration = Duration::from_millis(1000);
const FADE: Duration = Duration::from_millis(150);
/// A press on the thumb that moves less than this is a click (a mark under the thumb is visited).
const CLICK_SLOP: f32 = 3.;
/// At most this many problems in a tooltip.
const TOOLTIP_PROBLEMS: usize = 6;
const TOOLTIP_WIDTH: f32 = 420.;
/// The problems widget: its distance from the top and from what is at its right (the lane), its
/// buttons and height (with its padding and border).
const WIDGET_TOP: f32 = 6.;
const WIDGET_GAP: f32 = 4.;
const WIDGET_BUTTON: f32 = 20.;
const WIDGET_HEIGHT: f32 = WIDGET_BUTTON + 2. * 2. + 2.;
const WIDGET_GROUP: &str = "problems-widget";
/// The Markdown layout buttons at the top right of an editor in Editor Only
/// (`markdown_preview::render_toolbar`: 3 buttons of 26 px with gaps, padding and a border, 14 px
/// from the edge) take that corner: the widget goes beside them.
const MARKDOWN_TOOLBAR_WIDTH: f32 = 14. + 3. * ui::ICON_BUTTON_SIZE + 2. * 2. + 2. * 2. + 2.;
const MARKDOWN_TOOLBAR_HEIGHT: f32 = ui::ICON_BUTTON_SIZE + 2. * 2. + 2.;
/// Narrower text areas show no widget.
const WIDGET_MIN_TEXT_WIDTH: f32 = 320.;

/// What the scrollbar of one editor keeps between frames.
#[derive(Default)]
pub struct ScrollbarState {
    /// The scroll offset of the last frame: a change shows the thumbs.
    last_scroll: Option<Point<f32>>,
    /// When the editor last scrolled up or down, and sideways (or the mouse left the thumb).
    shown_v: Option<Instant>,
    shown_h: Option<Instant>,
    /// The mouse is over the lane (a field: the strip at its edge), or over the bottom strip.
    over_v: bool,
    over_h: bool,
    /// A thumb being dragged.
    drag: Option<Drag>,
    /// The marks under the mouse, for the tooltip.
    hover: Option<Hover>,
    /// The document's marks by line, while their sources stay the same.
    marks: Option<LineMarks>,
    /// The marks laid out on the lane, while neither they nor the lane change.
    laid: Option<LaidMarks>,
    /// When the thumbs are due to fade, and the task that redraws the editor then.
    hide_due: Option<Instant>,
    hide_running: bool,
    hide_task: Option<Task<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Drag {
    vertical: bool,
    /// Where in the thumb the mouse took it, from its start.
    grab: f32,
    /// Where the press was, and whether the mouse has moved off it since.
    origin: f32,
    moved: bool,
}

/// The width the scrollbar takes at the right edge of the text area: the text and its soft wraps
/// end before it.
pub fn lane_width(editor: &Editor) -> Pixels {
    match kind(editor) {
        Kind::Document => px(LANE_WIDTH),
        Kind::Field => px(0.),
    }
}

/// What a scrollbar an editor has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A document (a tab, the preview of Find in Files, a side of a diff): the lane with marks,
    /// the thumbs, the problems widget.
    Document,
    /// A commit message field or a plugin form's code field: a thin thumb over the text.
    Field,
}

fn kind(editor: &Editor) -> Kind {
    match editor.message {
        Some(_) => Kind::Field,
        None => Kind::Document,
    }
}

/// The scrollbar, its marks, the problems widget and the marks' tooltip, over the editor's text
/// area (an absolutely positioned child of it).
pub fn render(
    editor: &mut Editor,
    window: &mut Window,
    cx: &mut Context<Editor>,
) -> Option<AnyElement> {
    // A host's decorations (the diff viewer's blocks) are the element's for this one frame: the text
    // takes them before the lane is laid out.
    let blocks = editor
        .frame_decorations
        .as_deref()
        .map(|decorations| Rc::new(block_marks(decorations, editor.document.text())));
    let lane = ScrollbarElement {
        editor: cx.entity(),
        blocks,
    };
    let widget = problems_widget(editor, window, cx);
    let tooltip = marks_tooltip(editor, cx);
    Some(
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .child(lane)
            .children(widget)
            .children(tooltip)
            .into_any_element(),
    )
}

// --- Marks ---

/// A column of the lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Column {
    Changes,
    Matches,
    Problems,
}

impl Column {
    /// Its left edge in the lane and its width.
    fn span(self) -> (f32, f32) {
        match self {
            Column::Changes => CHANGES,
            Column::Matches => MATCHES,
            Column::Problems => PROBLEMS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum MarkKind {
    Problem(Severity),
    /// A find result; `active` — the current one (brighter).
    Match {
        active: bool,
    },
    Change(HunkKind),
    /// A host's block (a side of a diff): its background color.
    Block(Hsla),
}

impl MarkKind {
    fn column(self) -> Column {
        match self {
            MarkKind::Problem(_) => Column::Problems,
            MarkKind::Match { .. } => Column::Matches,
            MarkKind::Change(_) | MarkKind::Block(_) => Column::Changes,
        }
    }

    /// The color of the mark; `None` — not drawn.
    fn color(self, ui: &UiColors) -> Option<Hsla> {
        Some(match self {
            MarkKind::Problem(Severity::Info) => UiColors::tint(ui.info, 0.8),
            MarkKind::Problem(severity) => severity.color(ui)?,
            MarkKind::Match { active: false } => UiColors::tint(ui.search_match_active, 0.75),
            MarkKind::Match { active: true } => UiColors::tint(ui.search_match_active, 1.),
            MarkKind::Change(HunkKind::Added) => ui.diff_added,
            MarkKind::Change(HunkKind::Modified) => ui.diff_modified,
            MarkKind::Change(HunkKind::Deleted) => ui.diff_deleted,
            // Blocks come as backgrounds: the mark is their color made solid.
            MarkKind::Block(color) => UiColors::tint(color, 0.9),
        })
    }

    /// Of marks over one another, the higher layer is drawn on top (errors over warnings, the
    /// current find result over the others).
    fn layer(self) -> u8 {
        match self {
            MarkKind::Problem(Severity::Error) => 3,
            MarkKind::Problem(Severity::Warning) | MarkKind::Match { active: true } => 2,
            MarkKind::Problem(_) => 1,
            _ => 0,
        }
    }
}

/// A mark by the document's lines.
#[derive(Debug, Clone, PartialEq)]
struct LineMark {
    kind: MarkKind,
    /// The lines it covers (half-open); empty — the boundary before `lines.start` (lines deleted
    /// there).
    lines: Range<usize>,
    /// Where a click puts the caret: a character position.
    target: usize,
}

/// The marks of problems and find results by line, for the sources they were found from.
struct LineMarks {
    key: u64,
    marks: Rc<Vec<LineMark>>,
}

/// The marks of the last frame laid out on the lane: kept while the marks, the lane and the rows
/// stay the same (scrolling and the caret's blinking change none of them).
struct LaidMarks {
    key: u64,
    marks: Rc<Vec<PixelMark>>,
    quads: Rc<Vec<(Bounds<Pixels>, Hsla)>>,
}

/// Changes when anything the cached marks come from does: the text's size, the problems, the find
/// results. (Finding the lines of thousands of find results every frame would cost; this is a
/// pass over numbers.)
fn source_key(editor: &Editor) -> u64 {
    let mut key = Fnv::default();
    let text = editor.document.text();
    key.add(text.len_chars());
    key.add(text.len_lines());
    for d in editor.diagnostics.iter() {
        key.add(d.range.start);
        key.add(d.range.end);
        key.add(d.severity as usize);
    }
    key.add(usize::MAX);
    for m in &editor.search.matches {
        key.add(m.start);
    }
    key.add(editor.search.active.unwrap_or(usize::MAX));
    key.0
}

/// FNV-1a over words.
struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    fn add(&mut self, value: usize) {
        self.0 = (self.0 ^ value as u64).wrapping_mul(0x0100_0000_01b3);
    }
}

/// The marks of problems (hints aren't drawn, as in the text) and find results (one per line, and
/// the current one).
fn problem_and_match_marks(editor: &Editor) -> Vec<LineMark> {
    let text = editor.document.text();
    let len = text.len_chars();
    let line_of = |pos: usize| text.char_to_line(pos.min(len));
    let mut marks = Vec::new();
    for d in editor.diagnostics.iter() {
        if d.severity == Severity::Hint {
            continue;
        }
        let first = line_of(d.range.start);
        let last = line_of(d.range.end).max(first);
        marks.push(LineMark {
            kind: MarkKind::Problem(d.severity),
            lines: first..last + 1,
            target: d.range.start,
        });
    }
    let search = &editor.search;
    let mut previous = None;
    for (index, m) in search.matches.iter().enumerate() {
        let active = search.active == Some(index);
        let line = line_of(m.start);
        if previous == Some(line) && !active {
            continue;
        }
        previous = Some(line);
        marks.push(LineMark {
            kind: MarkKind::Match { active },
            lines: line..line + 1,
            target: m.start,
        });
    }
    marks
}

/// The Git changes of the document against HEAD, as its gutter shows them.
fn change_marks(editor: &Editor) -> Vec<LineMark> {
    let text = editor.document.text();
    let last = text.len_lines().saturating_sub(1);
    editor
        .git
        .hunks
        .iter()
        .map(|hunk| {
            let lines = hunk.new.start as usize..hunk.new.end as usize;
            LineMark {
                kind: MarkKind::Change(hunk.kind()),
                target: line_start(text, lines.start.min(last)),
                lines,
            }
        })
        .collect()
}

/// A diff side's blocks and the places where the other side has lines this one doesn't.
fn block_marks(
    decorations: &crate::diff_view::Decorations,
    text: &flux_core::Rope,
) -> Vec<LineMark> {
    let last = text.len_lines().saturating_sub(1);
    let target = |line: usize| line_start(text, line.min(last));
    decorations
        .lines
        .iter()
        .map(|(lines, color)| LineMark {
            kind: MarkKind::Block(*color),
            lines: lines.clone(),
            target: target(lines.start),
        })
        .chain(
            decorations
                .boundaries
                .iter()
                .map(|&(line, color)| LineMark {
                    kind: MarkKind::Block(color),
                    lines: line..line,
                    target: target(line),
                }),
        )
        .collect()
}

/// Where the lane puts rows: the track's top, and pixels of the lane per row.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RowScale {
    top: f32,
    /// The track's length: marks stay within it.
    len: f32,
    per_row: f32,
}

impl RowScale {
    /// The rows of a document `rows` long, `line_height` each, scrolled in a view `viewport` tall
    /// down to its last row at the top: as IntelliJ maps its error stripe — the scrollable height
    /// onto the track, never stretched (a document shorter than the track keeps its marks at their
    /// lines).
    fn new(top: f32, len: f32, rows: usize, line_height: f32, viewport: f32) -> Self {
        let source = rows.saturating_sub(1) as f32 * line_height + viewport;
        let scale = if source <= len || source <= 0. {
            1.
        } else {
            len / source
        };
        Self {
            top,
            len,
            per_row: line_height * scale,
        }
    }

    fn y(&self, row: usize) -> f32 {
        self.top + row as f32 * self.per_row
    }
}

/// A mark laid out on the lane (y along the track, in window pixels).
#[derive(Debug, Clone, PartialEq)]
struct PixelMark {
    kind: MarkKind,
    top: f32,
    bottom: f32,
    lines: Range<usize>,
    target: usize,
}

/// The first row of `line` (of `line_count`); past the last line — the end of the rows.
fn first_row(line: usize, line_count: usize, wrap: &crate::wrap::WrapMap) -> usize {
    if line >= line_count {
        wrap.row_count()
    } else {
        wrap.line_to_row(line)
    }
}

/// Lays out marks on the lane: a mark of one line is `MIN_MARK` tall at its row, one of several
/// lines reaches the row of its last line, a boundary is centered on its row; all stay within the
/// track. In the order of `marks`.
fn layout_marks<'a>(
    marks: impl Iterator<Item = &'a LineMark>,
    line_count: usize,
    wrap: &crate::wrap::WrapMap,
    scale: &RowScale,
) -> Vec<PixelMark> {
    let bottom_limit = scale.top + scale.len;
    marks
        .map(|mark| {
            let y = scale.y(first_row(mark.lines.start, line_count, wrap));
            let (mut top, mut bottom) = if mark.lines.is_empty() {
                (y - MIN_MARK / 2., y + MIN_MARK / 2.)
            } else {
                // Down to the last line's first row.
                let last = scale.y(first_row(mark.lines.end - 1, line_count, wrap));
                (y, last.max(y) + MIN_MARK)
            };
            if bottom > bottom_limit {
                let shift = bottom - bottom_limit;
                top -= shift;
                bottom -= shift;
            }
            if top < scale.top {
                let shift = scale.top - top;
                top += shift;
                bottom += shift;
            }
            PixelMark {
                kind: mark.kind,
                top,
                bottom,
                lines: mark.lines.clone(),
                target: mark.target,
            }
        })
        .collect()
}

/// The quads that draw the marks: in each column, marks of one color that touch become one; layers
/// in order (errors last, on top).
fn mark_quads(marks: &[PixelMark], ui: &UiColors) -> Vec<(Column, u8, f32, f32, Hsla)> {
    let mut sorted: Vec<(Column, u8, f32, f32, Hsla)> = marks
        .iter()
        .filter_map(|mark| {
            let color = mark.kind.color(ui)?;
            Some((
                mark.kind.column(),
                mark.kind.layer(),
                mark.top,
                mark.bottom,
                color,
            ))
        })
        .collect();
    sorted.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)).then(a.2.total_cmp(&b.2)));
    let mut merged: Vec<(Column, u8, f32, f32, Hsla)> = Vec::with_capacity(sorted.len());
    for quad in sorted {
        match merged.last_mut() {
            Some(last)
                if last.0 == quad.0
                    && last.1 == quad.1
                    && last.4 == quad.4
                    && quad.2 <= last.3 + 0.5 =>
            {
                last.3 = last.3.max(quad.3);
            }
            _ => merged.push(quad),
        }
    }
    merged
}

/// The quads' rectangles: in their columns of the lane at `lane_left`.
fn quad_bounds(
    quads: &[(Column, u8, f32, f32, Hsla)],
    lane_left: f32,
) -> Vec<(Bounds<Pixels>, Hsla)> {
    quads
        .iter()
        .map(|&(column, _, top, bottom, color)| {
            let (x, width) = column.span();
            let bounds = Bounds::new(
                point(px(lane_left + x), px(top)),
                size(px(width), px(bottom - top)),
            );
            (bounds, color)
        })
        .collect()
}

/// The key of the cached line marks (computed this frame).
fn line_marks_key(editor: &Editor) -> u64 {
    editor.scrollbar.marks.as_ref().map_or(0, |marks| marks.key)
}

/// Changes when the laid out marks would: the changes, the other marks, the rows, the lane, the
/// colors.
fn laid_key(
    changes: &[LineMark],
    marks: u64,
    counts: [usize; 2],
    geometry: [f32; 4],
    ui: &UiColors,
) -> u64 {
    let mut key = Fnv::default();
    key.add(marks as usize);
    for change in changes {
        key.add(change.lines.start);
        key.add(change.lines.end);
        key.add(match change.kind {
            MarkKind::Change(kind) => kind as usize,
            MarkKind::Block(color) => color_bits(color),
            _ => 0,
        });
    }
    for count in counts {
        key.add(count);
    }
    for value in geometry {
        key.add(value.to_bits() as usize);
    }
    for color in [
        ui.error,
        ui.warning,
        ui.info,
        ui.search_match_active,
        ui.diff_added,
        ui.diff_modified,
        ui.diff_deleted,
    ] {
        key.add(color_bits(color));
    }
    key.0
}

fn color_bits(color: Hsla) -> usize {
    let mut key = Fnv::default();
    for value in [color.h, color.s, color.l, color.a] {
        key.add(value.to_bits() as usize);
    }
    key.0 as usize
}

/// The marks within reach of `y` (on the lane), the nearest first; problems before find results
/// before changes at the same distance.
fn marks_at(marks: &[PixelMark], y: f32) -> Vec<&PixelMark> {
    let distance = |mark: &PixelMark| {
        if y < mark.top {
            mark.top - y
        } else if y > mark.bottom {
            y - mark.bottom
        } else {
            0.
        }
    };
    let mut found: Vec<&PixelMark> = marks
        .iter()
        .filter(|mark| distance(mark) <= REACH)
        .collect();
    found.sort_by(|a, b| {
        distance(a)
            .total_cmp(&distance(b))
            .then(b.kind.column().cmp(&a.kind.column()))
            .then(b.kind.layer().cmp(&a.kind.layer()))
    });
    found
}

// --- Thumbs ---

/// A thumb along its track, in pixels along the track's axis.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Thumb {
    track_start: f32,
    track_len: f32,
    start: f32,
    len: f32,
    max_scroll: f32,
}

impl Thumb {
    /// A view `viewport` long over content `max_scroll + viewport` long, scrolled by `scroll`;
    /// `None` when there is nothing to scroll.
    fn new(
        track_start: f32,
        track_len: f32,
        viewport: f32,
        max_scroll: f32,
        scroll: f32,
    ) -> Option<Self> {
        if max_scroll <= 0. || track_len <= 0. || viewport <= 0. {
            return None;
        }
        let len = (track_len * viewport / (max_scroll + viewport))
            .max(MIN_THUMB.min(track_len))
            .min(track_len);
        let ratio = (scroll / max_scroll).clamp(0., 1.);
        Some(Self {
            track_start,
            track_len,
            start: track_start + (track_len - len) * ratio,
            len,
            max_scroll,
        })
    }

    fn contains(&self, at: f32) -> bool {
        at >= self.start && at <= self.start + self.len
    }

    /// The scroll that puts the thumb's start at `start`.
    fn scroll_for(&self, start: f32) -> f32 {
        let room = self.track_len - self.len;
        if room <= 0. {
            return 0.;
        }
        ((start - self.track_start) / room).clamp(0., 1.) * self.max_scroll
    }
}

/// How much of a thumb shows, 0–1: fully while the mouse is over it or drags it (or always, with
/// legacy scroll bars), for `HOLD` after the editor scrolled, then fading out over `FADE`.
fn thumb_alpha(always: bool, shown: Option<Instant>, now: Instant) -> f32 {
    if always {
        return 1.;
    }
    let Some(shown) = shown else {
        return 0.;
    };
    let since = now.saturating_duration_since(shown);
    if since <= HOLD {
        1.
    } else if since < HOLD + FADE {
        1. - (since - HOLD).as_secs_f32() / FADE.as_secs_f32()
    } else {
        0.
    }
}

// --- The element ---

struct ScrollbarElement {
    editor: Entity<Editor>,
    /// A host's blocks of this frame (a side of a diff), as marks.
    blocks: Option<Rc<Vec<LineMark>>>,
}

impl IntoElement for ScrollbarElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// One frame of the scrollbar, laid out.
struct Frame {
    geometry: Rc<Geometry>,
    /// The marks' quads: bounds and color.
    quads: Rc<Vec<(Bounds<Pixels>, Hsla)>>,
    caret: Option<Bounds<Pixels>>,
    /// How much each thumb shows by the time it scrolled (the mouse over it shows it fully).
    alpha_v: f32,
    alpha_h: f32,
}

/// What the mouse handlers of a frame need.
struct Geometry {
    kind: Kind,
    /// The lane (a field: the strip at the text's right edge).
    lane: Bounds<Pixels>,
    lane_hitbox: Hitbox,
    vertical: Option<Thumb>,
    /// The thumb across: its rectangle's left and width (the lane's or the field's).
    v_left: f32,
    v_width: f32,
    /// The bottom strip and its thumb.
    bottom: Option<(Bounds<Pixels>, Hitbox, Thumb)>,
    marks: Rc<Vec<PixelMark>>,
    /// One page up or down: the view's height less a line.
    page: f32,
}

impl Element for ScrollbarElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<Frame>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Frame> {
        if bounds.size.height <= px(0.) || bounds.size.width <= px(0.) {
            return None;
        }
        let now = Instant::now();
        let ui = Theme::ui(cx);
        let always = !overlay_scrollers();
        // The editor's element laid out the text before this one: its scroll is this frame's.
        let line_marks = self.editor.update(cx, |editor, _| {
            let scroll = editor.scroll;
            let state = &mut editor.scrollbar;
            if let Some(last) = state.last_scroll {
                if last.y != scroll.y {
                    state.shown_v = Some(now);
                }
                if last.x != scroll.x {
                    state.shown_h = Some(now);
                }
            }
            state.last_scroll = Some(scroll);
            if kind(editor) == Kind::Field {
                return None;
            }
            let key = source_key(editor);
            let cached = editor
                .scrollbar
                .marks
                .as_ref()
                .filter(|marks| marks.key == key);
            if cached.is_none() {
                let marks = Rc::new(problem_and_match_marks(editor));
                editor.scrollbar.marks = Some(LineMarks { key, marks });
            }
            editor
                .scrollbar
                .marks
                .as_ref()
                .map(|marks| marks.marks.clone())
        });

        let line_height = theme::LINE_HEIGHT;
        let viewport = f32::from(bounds.size.height);
        let editor = self.editor.read(cx);
        let kind = kind(editor);
        let scroll = editor.scroll;
        let (shown_v, shown_h) = (editor.scrollbar.shown_v, editor.scrollbar.shown_h);
        let rows = editor.wrap.row_count();
        let max_scroll = rows.saturating_sub(1) as f32 * line_height;

        // The lane, its track and the thumb along it.
        let lane = match kind {
            Kind::Document => Bounds::from_corners(
                point(bounds.right() - px(LANE_WIDTH), bounds.top()),
                bounds.bottom_right(),
            ),
            Kind::Field => Bounds::from_corners(
                point(bounds.right() - px(FIELD_STRIP), bounds.top()),
                bounds.bottom_right(),
            ),
        };
        let lane_left = f32::from(lane.left());
        let track_start = f32::from(lane.top()) + TRACK_MARGIN;
        let track_len = f32::from(lane.size.height) - 2. * TRACK_MARGIN;
        let vertical = Thumb::new(track_start, track_len, viewport, max_scroll, scroll.y);
        let (v_left, v_width) = match kind {
            Kind::Document => (lane_left + THUMB_INSET, LANE_WIDTH - 2. * THUMB_INSET),
            Kind::Field => (f32::from(bounds.right()) - FIELD_THUMB - 2., FIELD_THUMB),
        };

        // Marks: Git changes (or a host's blocks), problems and find results (kept by line), the
        // caret's line.
        let scale = RowScale::new(track_start, track_len, rows, line_height, viewport);
        let text = editor.document.text();
        let line_count = text.len_lines();
        let mut laid = None;
        let mut caret = None;
        // A lane too short for a mark has none.
        if let Some(line_marks) = line_marks.as_ref().filter(|_| track_len > MIN_MARK) {
            let changes = match &self.blocks {
                Some(blocks) => blocks.as_ref().clone(),
                None => change_marks(editor),
            };
            let key = laid_key(
                &changes,
                line_marks_key(editor),
                [rows, line_count],
                [track_start, track_len, viewport, lane_left],
                &ui,
            );
            laid = match &editor.scrollbar.laid {
                Some(known) if known.key == key => {
                    Some((key, known.marks.clone(), known.quads.clone(), true))
                }
                _ => {
                    let marks = layout_marks(
                        changes.iter().chain(line_marks.iter()),
                        line_count,
                        &editor.wrap,
                        &scale,
                    );
                    let quads = quad_bounds(&mark_quads(&marks, &ui), lane_left);
                    Some((key, Rc::new(marks), Rc::new(quads), false))
                }
            };
            // The caret's own row: in a wrapped line, the part it is on.
            let head = editor.document.selection().primary().head;
            let y = scale
                .y(editor.wrap.row_of(text, head))
                .min(track_start + track_len - CARET_HEIGHT);
            caret = Some(Bounds::new(
                point(px(lane_left + 0.5), px(y)),
                size(px(LANE_WIDTH - 1.), px(CARET_HEIGHT)),
            ));
        }
        let (pixel_marks, quads) = match &laid {
            Some((_, marks, quads, _)) => (marks.clone(), quads.clone()),
            None => (Rc::default(), Rc::default()),
        };

        // Sideways: the visible lines of the last frame, as the editor clamps its scroll.
        let bottom = (!editor.wrap.enabled())
            .then_some(editor.layout.as_ref())
            .flatten()
            .and_then(|layout| {
                let text_bounds = layout.text_bounds;
                let visible = f32::from(text_bounds.size.width) - theme::TEXT_PADDING * 2.;
                let widest = layout
                    .rows
                    .iter()
                    .map(|row| f32::from(row.right()))
                    .fold(0., f32::max);
                if widest <= visible && scroll.x <= 0. {
                    return None;
                }
                let max_scroll_x = (widest - visible / 2.).max(0.);
                let left = f32::from(text_bounds.left()) + TRACK_MARGIN;
                let right = f32::from(text_bounds.right()) - TRACK_MARGIN;
                let thumb = Thumb::new(left, right - left, visible, max_scroll_x, scroll.x)?;
                let strip = Bounds::from_corners(
                    point(text_bounds.left(), bounds.bottom() - px(BOTTOM_STRIP)),
                    point(text_bounds.right(), bounds.bottom()),
                );
                Some((strip, thumb))
            });

        if let Some((key, marks, quads, false)) = laid {
            self.editor.update(cx, |editor, _| {
                editor.scrollbar.laid = Some(LaidMarks { key, marks, quads });
            });
        }

        // How much the thumbs show; a redraw when they are due to fade, and while they fade.
        let alpha_v = thumb_alpha(always, shown_v, now);
        let alpha_h = thumb_alpha(always, shown_h, now);
        if !always {
            let fading = |shown: Option<Instant>| {
                shown.is_some_and(|shown| {
                    let since = now.saturating_duration_since(shown);
                    since > HOLD && since < HOLD + FADE
                })
            };
            if fading(shown_v) || fading(shown_h) {
                window.request_animation_frame();
            }
            let due = [shown_v, shown_h]
                .into_iter()
                .flatten()
                .map(|shown| shown + HOLD)
                .filter(|due| *due > now)
                .max();
            if let Some(due) = due {
                schedule_fade(&self.editor, due, cx);
            }
        }

        let behavior = match kind {
            Kind::Document => HitboxBehavior::BlockMouseExceptScroll,
            Kind::Field => HitboxBehavior::Normal,
        };
        let lane_hitbox = window.insert_hitbox(lane, behavior);
        let bottom = bottom.map(|(strip, thumb)| {
            let hitbox = window.insert_hitbox(strip, HitboxBehavior::Normal);
            (strip, hitbox, thumb)
        });

        #[cfg(feature = "scenario")]
        scenario_hover(&self.editor, &pixel_marks, lane, cx);

        Some(Frame {
            geometry: Rc::new(Geometry {
                kind,
                lane,
                lane_hitbox,
                vertical,
                v_left,
                v_width,
                bottom,
                marks: pixel_marks,
                page: (viewport - line_height).max(line_height),
            }),
            quads,
            caret,
            alpha_v,
            alpha_h,
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut (),
        frame: &mut Option<Frame>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(frame) = frame.take() else {
            return;
        };
        let ui = Theme::ui(cx);
        let geometry = frame.geometry.clone();
        let (drag, over_v, over_h) = {
            let state = &self.editor.read(cx).scrollbar;
            (state.drag, state.over_v, state.over_h)
        };
        let lane_hovered = geometry.lane_hitbox.is_hovered(window) || over_v;
        let bottom_hovered = geometry
            .bottom
            .as_ref()
            .is_some_and(|(_, hitbox, _)| hitbox.is_hovered(window))
            || over_h;
        let dragging_v = drag.is_some_and(|drag| drag.vertical);
        let dragging_h = drag.is_some_and(|drag| !drag.vertical);

        // The track under the mouse, the marks, the caret's line.
        if geometry.kind == Kind::Document
            && (lane_hovered || dragging_v)
            && geometry.vertical.is_some()
        {
            window.paint_quad(fill(geometry.lane, UiColors::tint(ui.foreground, 0.04)));
        }
        for (bounds, color) in frame.quads.iter() {
            window.paint_quad(fill(*bounds, *color));
        }
        if let Some(caret) = frame.caret {
            window.paint_quad(fill(caret, UiColors::tint(ui.foreground, 0.55)));
        }

        // The thumbs, translucent over the marks.
        let thumb_color = |alpha: f32, hovered: bool, dragging: bool| {
            let strength = if dragging {
                0.38
            } else if hovered {
                0.3
            } else {
                0.2
            };
            UiColors::tint(ui.foreground, strength * alpha)
        };
        if let Some(thumb) = geometry.vertical {
            let alpha = if lane_hovered || dragging_v {
                1.
            } else {
                frame.alpha_v
            };
            if alpha > 0. {
                let bounds = Bounds::new(
                    point(px(geometry.v_left), px(thumb.start)),
                    size(px(geometry.v_width), px(thumb.len)),
                );
                window.paint_quad(
                    fill(bounds, thumb_color(alpha, lane_hovered, dragging_v))
                        .corner_radii(px(geometry.v_width / 2.)),
                );
            }
        }
        if let Some((strip, _, thumb)) = &geometry.bottom {
            let alpha = if bottom_hovered || dragging_h {
                1.
            } else {
                frame.alpha_h
            };
            if alpha > 0. {
                let top = strip.bottom() - px(BOTTOM_THUMB + 2.);
                let bounds = Bounds::new(
                    point(px(thumb.start), top),
                    size(px(thumb.len), px(BOTTOM_THUMB)),
                );
                window.paint_quad(
                    fill(bounds, thumb_color(alpha, bottom_hovered, dragging_h))
                        .corner_radii(px(BOTTOM_THUMB / 2.)),
                );
            }
        }

        if geometry.kind == Kind::Document {
            window.set_cursor_style(CursorStyle::Arrow, &geometry.lane_hitbox);
        }
        listen(&self.editor, geometry, window);
    }
}

/// Redraws the editor when its thumbs are due to fade: one task, sleeping until the latest due
/// time (scrolling keeps moving it).
fn schedule_fade(editor: &Entity<Editor>, due: Instant, cx: &mut App) {
    let start = editor.update(cx, |editor, _| {
        let state = &mut editor.scrollbar;
        state.hide_due = Some(state.hide_due.map_or(due, |known| known.max(due)));
        !std::mem::replace(&mut state.hide_running, true)
    });
    if !start {
        return;
    }
    let weak = editor.downgrade();
    let task = cx.spawn(async move |cx| {
        loop {
            let Ok(due) = weak.read_with(cx, |editor, _| editor.scrollbar.hide_due) else {
                return;
            };
            let now = Instant::now();
            match due {
                Some(due) if due > now => cx.background_executor().timer(due - now).await,
                _ => break,
            }
        }
        weak.update(cx, |editor, cx| {
            editor.scrollbar.hide_due = None;
            editor.scrollbar.hide_running = false;
            cx.notify();
        })
        .ok();
    });
    editor.update(cx, |editor, _| editor.scrollbar.hide_task = Some(task));
}

// --- Mouse ---

/// The mouse handlers of one frame: drags and clicks of the thumbs, clicks on marks and on the
/// track, the marks under the mouse.
fn listen(editor: &Entity<Editor>, geometry: Rc<Geometry>, window: &mut Window) {
    window.on_mouse_event({
        let editor = editor.clone();
        let geometry = geometry.clone();
        move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                return;
            }
            if mouse_down(&editor, &geometry, event, window, cx) {
                cx.stop_propagation();
            }
        }
    });
    window.on_mouse_event({
        let editor = editor.clone();
        let geometry = geometry.clone();
        move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            if mouse_move(&editor, &geometry, event, window, cx) {
                cx.stop_propagation();
            }
        }
    });
    window.on_mouse_event({
        let editor = editor.clone();
        let geometry = geometry.clone();
        move |event: &MouseUpEvent, phase, _, cx| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                return;
            }
            let drag = editor.update(cx, |editor, cx| {
                let state = &mut editor.scrollbar;
                let drag = state.drag.take();
                if let Some(drag) = drag {
                    // The thumb stays a while after the drag, as after scrolling.
                    let now = Instant::now();
                    if drag.vertical {
                        state.shown_v = Some(now);
                    } else {
                        state.shown_h = Some(now);
                    }
                    cx.notify();
                }
                drag
            });
            let Some(drag) = drag else {
                return;
            };
            // A press on the thumb that didn't move is a click: a mark under it is visited.
            if drag.vertical
                && !drag.moved
                && geometry.kind == Kind::Document
                && let Some(mark) = marks_at(&geometry.marks, drag.origin).first()
            {
                go_to_mark(&editor, mark, cx);
            }
            cx.stop_propagation();
        }
    });
    window.on_mouse_event({
        let editor = editor.clone();
        move |_: &MouseExitEvent, _, _, cx| {
            editor.update(cx, |editor, cx| {
                let state = &mut editor.scrollbar;
                if state.drag.is_none() && (state.over_v || state.over_h || state.hover.is_some()) {
                    leave(state);
                    cx.notify();
                }
            });
        }
    });
}

/// The mouse left the lane and the bottom strip: the thumbs fade after a while, as after scrolling.
fn leave(state: &mut ScrollbarState) {
    let now = Instant::now();
    if state.over_v {
        state.shown_v = Some(now);
    }
    if state.over_h {
        state.shown_h = Some(now);
    }
    state.over_v = false;
    state.over_h = false;
    state.hover = None;
}

/// A press on the lane or a thumb: `true` — taken (the text doesn't get it).
fn mouse_down(
    editor: &Entity<Editor>,
    geometry: &Geometry,
    event: &MouseDownEvent,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let y = f32::from(event.position.y);
    let x = f32::from(event.position.x);
    if geometry.lane_hitbox.is_hovered(window) {
        let on_thumb = geometry.vertical.filter(|thumb| thumb.contains(y));
        match geometry.kind {
            // A field's strip lies over the text: only its thumb takes a press.
            Kind::Field => {
                let Some(thumb) = on_thumb else {
                    return false;
                };
                start_drag(editor, true, y - thumb.start, y, false, cx);
            }
            Kind::Document => {
                if let Some(thumb) = on_thumb {
                    start_drag(editor, true, y - thumb.start, y, false, cx);
                } else if let Some(mark) = marks_at(&geometry.marks, y).first() {
                    go_to_mark(editor, mark, cx);
                } else if let Some(thumb) = geometry.vertical {
                    click_track(editor, &thumb, true, y, geometry.page, event, cx);
                }
            }
        }
        return true;
    }
    if let Some((_, hitbox, thumb)) = &geometry.bottom
        && hitbox.is_hovered(window)
        && thumb.contains(x)
    {
        start_drag(editor, false, x - thumb.start, x, false, cx);
        return true;
    }
    false
}

fn start_drag(
    editor: &Entity<Editor>,
    vertical: bool,
    grab: f32,
    origin: f32,
    moved: bool,
    cx: &mut App,
) {
    editor.update(cx, |editor, cx| {
        editor.scrollbar.drag = Some(Drag {
            vertical,
            grab,
            origin,
            moved,
        });
        editor.scrollbar.hover = None;
        cx.notify();
    });
}

/// A press on the track beside the thumb: a page towards it, as macOS does; ⌥ — or macOS's "Jump to
/// the spot that's clicked" — puts the thumb's middle there and keeps dragging it.
fn click_track(
    editor: &Entity<Editor>,
    thumb: &Thumb,
    vertical: bool,
    at: f32,
    page: f32,
    event: &MouseDownEvent,
    cx: &mut App,
) {
    let jump = click_jumps() != event.modifiers.alt;
    if jump {
        let scroll = thumb.scroll_for(at - thumb.len / 2.);
        set_scroll(editor, vertical, scroll, cx);
        start_drag(editor, vertical, thumb.len / 2., at, true, cx);
        return;
    }
    let current = editor.read(cx).scroll;
    let current = if vertical { current.y } else { current.x };
    let towards = if at < thumb.start { -1. } else { 1. };
    let scroll = (current + towards * page).clamp(0., thumb.max_scroll);
    set_scroll(editor, vertical, scroll, cx);
}

fn set_scroll(editor: &Entity<Editor>, vertical: bool, scroll: f32, cx: &mut App) {
    editor.update(cx, |editor, cx| {
        if vertical {
            editor.scroll.y = scroll;
        } else {
            editor.scroll.x = scroll;
        }
        editor.autoscroll = None;
        cx.notify();
    });
}

/// The mouse moved: a thumb being dragged follows it; otherwise the lane notes whether the mouse
/// is over it and which marks are under it. `true` — taken (a drag: the text doesn't hover).
fn mouse_move(
    editor: &Entity<Editor>,
    geometry: &Geometry,
    event: &MouseMoveEvent,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let over_v = geometry.lane_hitbox.is_hovered(window);
    let over_h = geometry
        .bottom
        .as_ref()
        .is_some_and(|(_, hitbox, _)| hitbox.is_hovered(window));
    let drag = editor.read(cx).scrollbar.drag;
    if let Some(drag) = drag {
        if event.pressed_button != Some(MouseButton::Left) {
            // The button went up outside the window.
            editor.update(cx, |editor, cx| {
                editor.scrollbar.drag = None;
                cx.notify();
            });
            return false;
        }
        let (thumb, at) = if drag.vertical {
            (geometry.vertical, f32::from(event.position.y))
        } else {
            (
                geometry.bottom.as_ref().map(|(_, _, thumb)| *thumb),
                f32::from(event.position.x),
            )
        };
        if let Some(thumb) = thumb {
            set_scroll(editor, drag.vertical, thumb.scroll_for(at - drag.grab), cx);
        }
        editor.update(cx, |editor, _| {
            let state = &mut editor.scrollbar;
            // Where the mouse is when the drag ends: leaving the lane then starts the fade.
            state.over_v = over_v;
            state.over_h = over_h;
            if let Some(drag) = &mut state.drag
                && (at - drag.origin).abs() > CLICK_SLOP
            {
                drag.moved = true;
            }
        });
        return true;
    }
    let hover = (over_v && geometry.kind == Kind::Document)
        .then(|| {
            let y = f32::from(event.position.y);
            hover_at(editor, &geometry.marks, geometry.lane.left(), y, cx)
        })
        .flatten();
    editor.update(cx, |editor, cx| {
        let state = &mut editor.scrollbar;
        if state.over_v == over_v && state.over_h == over_h && state.hover == hover {
            return;
        }
        let (was_v, was_h) = (state.over_v, state.over_h);
        if was_v && !over_v || was_h && !over_h {
            let now = Instant::now();
            if was_v && !over_v {
                state.shown_v = Some(now);
            }
            if was_h && !over_h {
                state.shown_h = Some(now);
            }
        }
        state.over_v = over_v;
        state.over_h = over_h;
        state.hover = hover;
        cx.notify();
    });
    false
}

/// A click on a mark: the caret goes to what it marks (no selection, as IntelliJ's stripe does); a
/// problem's message goes to the status bar, as with F2.
fn go_to_mark(editor: &Entity<Editor>, mark: &PixelMark, cx: &mut App) {
    editor.update(cx, |editor, cx| {
        let pos = mark.target.min(editor.document.text().len_chars());
        editor.select_range(pos..pos, cx);
        if let MarkKind::Problem(_) = mark.kind
            && let Some(summary) = editor.diagnostics.at(pos).first().map(|d| d.summary())
        {
            editor.show_status(summary.into(), cx);
        }
    });
}

// --- The marks' tooltip ---

/// The marks under the mouse: what the tooltip says, and where.
#[derive(Debug, Clone, PartialEq)]
struct Hover {
    /// The middle of the marks, and the lane's left edge (window coordinates).
    y: Pixels,
    left: Pixels,
    items: Vec<HoverItem>,
    /// More problems than the tooltip lists.
    more: usize,
}

#[derive(Debug, Clone, PartialEq)]
enum HoverItem {
    Problem {
        severity: Severity,
        message: String,
        source: Option<String>,
    },
    /// Find results on the lines of the marks.
    Matches(usize),
    /// A Git change: its kind and lines (0-based, half-open).
    Change(HunkKind, Range<usize>),
}

/// What the marks within reach of `y` are: their problems (the most severe first), the number of
/// find results on their lines, their Git changes. `None` — no marks there.
fn hover_at(
    editor: &Entity<Editor>,
    marks: &[PixelMark],
    lane_left: Pixels,
    y: f32,
    cx: &App,
) -> Option<Hover> {
    let found = marks_at(marks, y);
    if found.is_empty() {
        return None;
    }
    let editor = editor.read(cx);
    let text = editor.document.text();
    let line_count = text.len_lines();
    let char_range = |lines: Range<usize>| {
        let start = line_start(text, lines.start.min(line_count));
        let end = if lines.end >= line_count {
            text.len_chars() + 1
        } else {
            line_start(text, lines.end)
        };
        start..end
    };
    // The lines the marks of a column start on.
    let lines_of = |column: Column| {
        let starts = found
            .iter()
            .filter(|mark| mark.kind.column() == column)
            .map(|mark| mark.lines.start);
        let first = starts.clone().min()?;
        let last = starts.max()?;
        Some(first..last + 1)
    };
    let mut items = Vec::new();
    let mut more = 0;
    if let Some(lines) = lines_of(Column::Problems) {
        let range = char_range(lines);
        let mut problems: Vec<&crate::diagnostics::Diagnostic> = editor
            .diagnostics
            .iter()
            .filter(|d| d.severity != Severity::Hint && range.contains(&d.range.start))
            .collect();
        problems.sort_by_key(|d| d.severity);
        more = problems.len().saturating_sub(TOOLTIP_PROBLEMS);
        items.extend(
            problems
                .into_iter()
                .take(TOOLTIP_PROBLEMS)
                .map(|d| HoverItem::Problem {
                    severity: d.severity,
                    message: d.message.clone(),
                    source: match (&d.source, &d.code) {
                        (Some(source), Some(code)) => Some(format!("{source}({code})")),
                        (Some(source), None) => Some(source.clone()),
                        (None, Some(code)) => Some(code.clone()),
                        (None, None) => None,
                    },
                }),
        );
    }
    if let Some(lines) = lines_of(Column::Matches) {
        let range = char_range(lines);
        let matches = &editor.search.matches;
        let from = matches.partition_point(|m| m.start < range.start);
        let to = matches.partition_point(|m| m.start < range.end);
        if to > from {
            items.push(HoverItem::Matches(to - from));
        }
    }
    for mark in &found {
        if let MarkKind::Change(kind) = mark.kind {
            let item = HoverItem::Change(kind, mark.lines.clone());
            if !items.contains(&item) {
                items.push(item);
            }
        }
    }
    if items.is_empty() {
        return None;
    }
    let top = found.iter().map(|mark| mark.top).fold(f32::MAX, f32::min);
    let bottom = found
        .iter()
        .map(|mark| mark.bottom)
        .fold(f32::MIN, f32::max);
    Some(Hover {
        y: px((top + bottom) / 2.),
        left: lane_left,
        items,
        more,
    })
}

/// The tooltip of the marks under the mouse: to the left of the lane, level with them (IntelliJ's
/// error stripe shows its hint there).
fn marks_tooltip(editor: &Editor, cx: &mut Context<Editor>) -> Option<AnyElement> {
    let hover = editor.scrollbar.hover.as_ref()?;
    if editor.scrollbar.drag.is_some() {
        return None;
    }
    let ui = Theme::ui(cx);
    let rows = hover.items.iter().map(|item| match item {
        HoverItem::Problem {
            severity,
            message,
            source,
        } => {
            let name = match severity {
                Severity::Error => IconName::Error,
                Severity::Warning => IconName::Warning,
                Severity::Info | Severity::Hint => IconName::Info,
            };
            let color = severity.color(&ui).unwrap_or(ui.dim);
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
                        .child(message.clone())
                        .children(source.clone().map(|source| {
                            div()
                                .text_size(px(theme::TEXT_SM))
                                .text_color(ui.dim)
                                .child(source)
                        })),
                )
                .into_any_element()
        }
        HoverItem::Matches(count) => div()
            .flex()
            .items_center()
            .gap_2()
            .child(icon(IconName::Search, ui.text_muted).size(px(14.)))
            .child(trn(*count, "{n} result", "{n} results"))
            .into_any_element(),
        HoverItem::Change(kind, lines) => {
            let color = match kind {
                HunkKind::Added => ui.diff_added,
                HunkKind::Modified => ui.diff_modified,
                HunkKind::Deleted => ui.diff_deleted,
            };
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .w(px(3.))
                        .h(px(14.))
                        .mx(px(5.5))
                        .rounded(px(1.5))
                        .bg(color),
                )
                .child(change_text(*kind, lines))
                .into_any_element()
        }
    });
    let content = div()
        .max_w(px(TOOLTIP_WIDTH))
        .px_3()
        .py_2()
        .flex()
        .flex_col()
        .gap_1p5()
        .children(rows)
        .when(hover.more > 0, |content| {
            content.child(
                div()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(trf("and {0} more", &[&hover.more])),
            )
        });
    let panel = popup::panel(ui).occlude().child(content);
    Some(
        deferred(
            anchored()
                .anchor(Corner::TopRight)
                .position(point(hover.left - px(POPUP_GAP), hover.y - px(14.)))
                .snap_to_window_with_margin(px(WINDOW_MARGIN))
                .child(panel),
        )
        .with_priority(1)
        .into_any_element(),
    )
}

/// "Added lines 3–5", "Lines deleted after line 7" — the lines from 1.
fn change_text(kind: HunkKind, lines: &Range<usize>) -> String {
    let first = lines.start + 1;
    let last = lines.end;
    match kind {
        HunkKind::Deleted if lines.start == 0 => tr("Lines deleted at the start").into(),
        HunkKind::Deleted => trf("Lines deleted after line {0}", &[&lines.start]),
        HunkKind::Added if last <= first => trf("Added line {0}", &[&first]),
        HunkKind::Added => trf("Added lines {0}–{1}", &[&first, &last]),
        HunkKind::Modified if last <= first => trf("Modified line {0}", &[&first]),
        HunkKind::Modified => trf("Modified lines {0}–{1}", &[&first, &last]),
    }
}

// --- The problems widget ---

/// The problems widget at the editor's top right (IntelliJ's inspection widget): the numbers of
/// errors, warnings and infos — a click goes to the next problem — and ↑ ↓ (⇧F2 / F2); a green
/// check when the file's language servers have looked at it and found nothing. Nothing in a field,
/// in a document no server checks, or while its servers are busy or couldn't load the project.
fn problems_widget(
    editor: &mut Editor,
    _window: &mut Window,
    cx: &mut Context<Editor>,
) -> Option<AnyElement> {
    if kind(editor) != Kind::Document {
        return None;
    }
    let (errors, warnings) = editor.diagnostics.counts();
    let infos = editor
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Info)
        .count();
    // The green check says the file is clean: only once its servers have looked at it — running,
    // with the project loaded and nothing in progress (a server that couldn't load the project
    // reports nothing at all).
    let settled = !editor.lsp.is_empty()
        && editor
            .lsp_store
            .as_ref()
            .and_then(|store| store.upgrade())
            .is_some_and(|store| store.read(cx).settled(cx.entity_id()));
    if errors + warnings + infos == 0 && !settled {
        return None;
    }
    // A narrow editor (a split, a small window) keeps its text: the status bar has the numbers too.
    let narrow = editor
        .layout
        .as_ref()
        .is_some_and(|layout| layout.text_bounds.size.width < px(WIDGET_MIN_TEXT_WIDTH));
    if narrow {
        return None;
    }
    let ui = Theme::ui(cx);
    // The Markdown layout buttons sit at the top right in Editor Only.
    let markdown = crate::markdown_preview::layout(editor, cx) == Some(MarkdownLayout::Editor);
    let (top, right) = if markdown {
        // Beside them, level with their middle.
        (
            WIDGET_TOP + (MARKDOWN_TOOLBAR_HEIGHT - WIDGET_HEIGHT) / 2.,
            MARKDOWN_TOOLBAR_WIDTH + WIDGET_GAP,
        )
    } else {
        (WIDGET_TOP, LANE_WIDTH + WIDGET_GAP)
    };
    let focus = editor.focus_handle.clone();
    let go = move |forward: bool| {
        let focus = focus.clone();
        move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
            // The actions reach the editor through the focus.
            window.focus(&focus);
            let action: Box<dyn gpui::Action> = if forward {
                Box::new(NextProblem)
            } else {
                Box::new(PreviousProblem)
            };
            window.dispatch_action(action, cx);
        }
    };
    let count = |name: IconName, color: Hsla, count: usize| {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(icon(name, color).size(px(13.)))
            .child(count.to_string())
    };
    let summary: AnyElement = if errors + warnings + infos == 0 {
        div()
            .id("problems-widget-none")
            .flex()
            .items_center()
            .justify_center()
            .size(px(WIDGET_BUTTON))
            .rounded(px(ui::RADIUS_SM))
            .child(icon(IconName::CheckCircle, ui.success).size(px(14.)))
            .tooltip(ui::tooltip(tr("No problems"), None))
            .into_any_element()
    } else {
        let tooltip = if infos > 0 {
            trf(
                "Errors: {0} · Warnings: {1} · Info: {2}",
                &[&errors, &warnings, &infos],
            )
        } else {
            trf("Errors: {0} · Warnings: {1}", &[&errors, &warnings])
        };
        div()
            .id("problems-widget-counts")
            .flex()
            .items_center()
            .gap_2()
            .h(px(WIDGET_BUTTON))
            .px_1p5()
            .rounded(px(ui::RADIUS_SM))
            .cursor_pointer()
            .hover(move |style| style.bg(ui.hover))
            .when(errors > 0, |item| {
                item.child(count(IconName::Error, ui.error, errors))
            })
            .when(warnings > 0, |item| {
                item.child(count(IconName::Warning, ui.warning, warnings))
            })
            .when(infos > 0, |item| {
                item.child(count(IconName::Info, ui.info, infos))
            })
            .tooltip(ui::tooltip(tooltip, ui::shortcut_for(&NextProblem, cx)))
            .on_click(go(true))
            .into_any_element()
    };
    let arrow = |id: &'static str, name: IconName, label: &'static str, forward: bool| {
        let action: Box<dyn gpui::Action> = if forward {
            Box::new(NextProblem)
        } else {
            Box::new(PreviousProblem)
        };
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .size(px(WIDGET_BUTTON))
            .rounded(px(ui::RADIUS_SM))
            .cursor_pointer()
            .hover(move |style| style.bg(ui.hover))
            .child(icon(name, ui.text_muted).size(px(13.)))
            .tooltip(ui::tooltip(
                tr(label),
                ui::shortcut_for(action.as_ref(), cx),
            ))
            .on_click(go(forward))
    };
    let problems = errors + warnings + infos > 0;
    Some(
        div()
            .id("problems-widget")
            .absolute()
            .top(px(top))
            .right(px(right))
            .flex()
            .items_center()
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(ui::RADIUS_MD))
            // The text under it is hidden: the island's color without its glass; the border as
            // the Markdown layout buttons have.
            .bg(UiColors::tint(ui.island, 1.))
            .border_1()
            .border_color(ui.divider)
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_SM))
            .text_color(ui.text_muted)
            .block_mouse_except_scroll()
            .group(WIDGET_GROUP)
            .child(
                // Quieter until the mouse comes to it, as the Markdown layout buttons.
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .opacity(0.8)
                    .group_hover(WIDGET_GROUP, |content| content.opacity(1.))
                    .child(summary)
                    .when(problems, |content| {
                        content
                            .child(arrow(
                                "problems-widget-previous",
                                IconName::ArrowUp,
                                "Previous Problem",
                                false,
                            ))
                            .child(arrow(
                                "problems-widget-next",
                                IconName::ArrowDown,
                                "Next Problem",
                                true,
                            ))
                    }),
            )
            .into_any_element(),
    )
}

// --- macOS ---

/// macOS shows overlay scroll bars (General → Show scroll bars: automatically, with a trackpad, or
/// when scrolling): the thumbs show only while the editor scrolls or the mouse is over them. With
/// legacy ones (a mouse, or "Always") they always show — IntelliJ follows the same setting.
fn overlay_scrollers() -> bool {
    #[cfg(feature = "scenario")]
    if scenario_always() {
        return false;
    }
    system::overlay_scrollers()
}

/// "Click in the scroll bar to: Jump to the spot that's clicked" (otherwise a click pages).
fn click_jumps() -> bool {
    system::click_jumps()
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // `msg_send!` of objc 0.2 checks a `cargo-clippy` feature.
mod system {
    use std::ffi::CStr;

    use objc::runtime::{NO, Object};
    use objc::{class, msg_send, sel, sel_impl};

    pub fn overlay_scrollers() -> bool {
        // SAFETY: a class method of AppKit's NSScroller without arguments, returning NSInteger.
        let style: isize = unsafe { msg_send![class!(NSScroller), preferredScrollerStyle] };
        // NSScrollerStyleOverlay.
        style == 1
    }

    pub fn click_jumps() -> bool {
        const KEY: &CStr = c"AppleScrollerPagingBehavior";
        // SAFETY: NSUserDefaults is thread-safe; the key is a NUL-terminated UTF-8 string turned
        // into an autoreleased NSString.
        unsafe {
            let defaults: *mut Object = msg_send![class!(NSUserDefaults), standardUserDefaults];
            let key: *mut Object = msg_send![class!(NSString), stringWithUTF8String: KEY.as_ptr()];
            let on: objc::runtime::BOOL = msg_send![defaults, boolForKey: key];
            on != NO
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod system {
    pub fn overlay_scrollers() -> bool {
        true
    }

    pub fn click_jumps() -> bool {
        false
    }
}

// --- Scenarios ---

/// `FLUX_SCENARIO_SCROLLBAR=always`: the thumbs always show (as with legacy scroll bars), so shots
/// have them.
#[cfg(feature = "scenario")]
fn scenario_always() -> bool {
    scenario_setting().is_some_and(|setting| setting.split(',').any(|part| part == "always"))
}

#[cfg(feature = "scenario")]
fn scenario_setting() -> Option<&'static str> {
    static SETTING: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    SETTING
        .get_or_init(|| std::env::var("FLUX_SCENARIO_SCROLLBAR").ok())
        .as_deref()
}

/// `FLUX_SCENARIO_SCROLLBAR=hover:<fraction>`: the mouse is taken to be over the lane at that
/// fraction of its height (a scenario can't move the mouse): the tooltip of the marks there shows.
#[cfg(feature = "scenario")]
fn scenario_hover(
    editor: &Entity<Editor>,
    marks: &[PixelMark],
    lane: Bounds<Pixels>,
    cx: &mut App,
) {
    let Some(fraction) = scenario_setting().and_then(|setting| {
        setting
            .split(',')
            .find_map(|part| part.strip_prefix("hover:")?.parse::<f32>().ok())
    }) else {
        return;
    };
    let y = f32::from(lane.top()) + f32::from(lane.size.height) * fraction;
    let hover = hover_at(editor, marks, lane.left(), y, cx);
    // Every frame: the marks change as the language server publishes.
    if editor.read(cx).scrollbar.hover != hover {
        editor.update(cx, |editor, cx| {
            editor.scrollbar.over_v = hover.is_some();
            editor.scrollbar.hover = hover;
            cx.notify();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(kind: MarkKind, lines: Range<usize>) -> LineMark {
        LineMark {
            kind,
            target: lines.start,
            lines,
        }
    }

    fn wrap(lines: usize) -> crate::wrap::WrapMap {
        let mut map = crate::wrap::WrapMap::default();
        map.reset(&flux_core::Rope::from(
            "\n".repeat(lines.saturating_sub(1)).as_str(),
        ));
        map
    }

    const ERROR: MarkKind = MarkKind::Problem(Severity::Error);
    const WARNING: MarkKind = MarkKind::Problem(Severity::Warning);

    #[test]
    fn a_short_document_keeps_its_marks_at_their_lines() {
        // 10 lines of 20 px in a view of 300 px, but a lane of 800 px: the scrollable height
        // (9 × 20 + 300 = 480) is shorter than the track, so it isn't stretched.
        let scale = RowScale::new(0., 800., 10, 20., 300.);
        assert_eq!(scale.per_row, 20.);
        assert_eq!(scale.y(4), 80.);
    }

    #[test]
    fn a_long_document_maps_its_scrollable_height_onto_the_track() {
        // 1000 lines of 20 px, scrolled down to the last line at the top of a 500 px view:
        // 999 × 20 + 500 = 20480 px onto 512 px — 1/40.
        let scale = RowScale::new(10., 512., 1000, 20., 500.);
        assert_eq!(scale.per_row, 0.5);
        assert_eq!(scale.y(0), 10.);
        assert_eq!(scale.y(999), 10. + 499.5);
    }

    #[test]
    fn marks_are_three_pixels_at_least_and_stay_on_the_track() {
        let scale = RowScale::new(0., 100., 1000, 20., 100.);
        let map = wrap(1000);
        let marks = [
            mark(ERROR, 10..11),
            mark(WARNING, 100..300),
            mark(MarkKind::Change(HunkKind::Deleted), 500..500),
            mark(ERROR, 999..1000),
        ];
        let laid = layout_marks(marks.iter(), 1000, &map, &scale);
        // One line: MIN_MARK tall at its row.
        assert!((laid[0].bottom - laid[0].top - MIN_MARK).abs() < 1e-4);
        // Several lines: from the first to the last line's row.
        assert!((laid[1].top - scale.y(100)).abs() < 1e-4);
        assert!((laid[1].bottom - (scale.y(299) + MIN_MARK)).abs() < 1e-4);
        // A boundary: centered on its row.
        assert!(((laid[2].top + laid[2].bottom) / 2. - scale.y(500)).abs() < 1e-4);
        // The last line's mark doesn't hang past the end of the track.
        assert!(laid[3].bottom <= 100. + 1e-4);
        assert!((laid[3].bottom - laid[3].top - MIN_MARK).abs() < 1e-4);
    }

    #[test]
    fn touching_marks_of_one_color_merge_and_layers_keep_their_order() {
        let ui = Theme::flux_night().ui;
        let pixel = |kind: MarkKind, top: f32, bottom: f32| PixelMark {
            kind,
            top,
            bottom,
            lines: 0..1,
            target: 0,
        };
        let marks = [
            pixel(ERROR, 10., 13.),
            pixel(ERROR, 13., 16.),
            pixel(WARNING, 11., 14.),
            pixel(ERROR, 40., 43.),
            pixel(MarkKind::Match { active: false }, 10., 13.),
        ];
        let quads = mark_quads(&marks, &ui);
        let summary: Vec<(Column, u8, f32, f32)> = quads
            .iter()
            .map(|quad| (quad.0, quad.1, quad.2, quad.3))
            .collect();
        assert_eq!(
            summary,
            vec![
                (Column::Matches, 0, 10., 13.),
                (Column::Problems, 2, 11., 14.),
                (Column::Problems, 3, 10., 16.),
                (Column::Problems, 3, 40., 43.),
            ]
        );
    }

    #[test]
    fn hints_are_not_drawn() {
        let ui = Theme::flux_night().ui;
        assert_eq!(MarkKind::Problem(Severity::Hint).color(&ui), None);
        assert!(MarkKind::Problem(Severity::Info).color(&ui).is_some());
    }

    #[test]
    fn the_nearest_marks_answer_a_click_problems_first() {
        let pixel = |kind: MarkKind, top: f32| PixelMark {
            kind,
            top,
            bottom: top + MIN_MARK,
            lines: 0..1,
            target: top as usize,
        };
        let marks = [
            pixel(MarkKind::Change(HunkKind::Added), 50.),
            pixel(WARNING, 50.),
            pixel(ERROR, 70.),
        ];
        let at = |y: f32| -> Vec<usize> { marks_at(&marks, y).iter().map(|m| m.target).collect() };
        // On the marks at 50: the warning before the change beside it.
        assert_eq!(at(51.), vec![50, 50]);
        assert_eq!(marks_at(&marks, 51.)[0].kind, WARNING);
        // Within reach below a mark, and no farther.
        assert_eq!(at(53. + REACH), vec![50, 50]);
        assert!(at(53. + REACH + 0.5).is_empty());
        assert_eq!(at(66.), vec![70]);
        // Between two: the nearer first.
        let close = [pixel(WARNING, 50.), pixel(ERROR, 60.)];
        let order =
            |y: f32| -> Vec<usize> { marks_at(&close, y).iter().map(|m| m.target).collect() };
        assert_eq!(order(56.), vec![50, 60]);
        assert_eq!(order(57.5), vec![60, 50]);
    }

    #[test]
    fn thumbs_follow_the_scroll_and_drags_map_back() {
        // A 600 px view of content scrollable by 2400 px on a 300 px track: the thumb is a fifth.
        let thumb = Thumb::new(10., 300., 600., 2400., 0.).unwrap();
        assert_eq!(thumb.len, 60.);
        assert_eq!(thumb.start, 10.);
        let middle = Thumb::new(10., 300., 600., 2400., 1200.).unwrap();
        assert_eq!(middle.start, 10. + 120.);
        let end = Thumb::new(10., 300., 600., 2400., 2400.).unwrap();
        assert_eq!(end.start + end.len, 310.);
        // Dragging the thumb's start back gives the scroll; past the ends — clamped.
        assert_eq!(middle.scroll_for(130.), 1200.);
        assert_eq!(middle.scroll_for(-50.), 0.);
        assert_eq!(middle.scroll_for(1000.), 2400.);
        // Nothing to scroll — no thumb.
        assert_eq!(Thumb::new(0., 300., 600., 0., 0.), None);
    }

    #[test]
    fn a_long_document_gets_a_thumb_of_the_minimal_length() {
        let thumb = Thumb::new(0., 300., 600., 2_000_000., 1_000_000.).unwrap();
        assert_eq!(thumb.len, MIN_THUMB);
        assert!((thumb.start - (300. - MIN_THUMB) / 2.).abs() < 1e-3);
        // A track shorter than the minimal thumb: the thumb fills it.
        let tiny = Thumb::new(0., 12., 600., 2400., 0.).unwrap();
        assert_eq!(tiny.len, 12.);
        assert_eq!(tiny.scroll_for(5.), 0.);
    }

    #[test]
    fn thumbs_hold_then_fade() {
        let now = Instant::now();
        assert_eq!(thumb_alpha(false, None, now), 0.);
        assert_eq!(thumb_alpha(true, None, now), 1.);
        assert_eq!(thumb_alpha(false, Some(now), now), 1.);
        let fading = thumb_alpha(false, Some(now), now + HOLD + FADE / 2);
        assert!(fading > 0.4 && fading < 0.6);
        assert_eq!(thumb_alpha(false, Some(now), now + HOLD + FADE), 0.);
    }

    #[test]
    fn the_source_key_hash_depends_on_the_order() {
        let mut a = Fnv::default();
        a.add(1);
        a.add(2);
        let mut b = Fnv::default();
        b.add(2);
        b.add(1);
        assert_ne!(a.0, b.0);
    }

    #[test]
    fn change_texts_count_lines_from_one() {
        assert_eq!(change_text(HunkKind::Added, &(2..3)), "Added line 3");
        assert_eq!(
            change_text(HunkKind::Modified, &(2..5)),
            "Modified lines 3–5"
        );
        assert_eq!(
            change_text(HunkKind::Deleted, &(7..7)),
            "Lines deleted after line 7"
        );
        assert_eq!(
            change_text(HunkKind::Deleted, &(0..0)),
            "Lines deleted at the start"
        );
    }
}
