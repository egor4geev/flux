//! The diff viewer: a tab with a file's HEAD version and its working copy, as the diff of JetBrains
//! IDEs.
//!
//! - **Side by side**: HEAD on the left (read-only: selectable, copyable), the working copy on the
//!   right — the file's own editor, the one of its tab if it has one, so edits made here are edits
//!   of the file. The two scroll together, line by line through the changed blocks. Between them, a
//!   strip joins each block on the left to its block on the right; on each block, an arrow reverts it
//!   (the HEAD lines go back into the working copy, one undo step) and a checkbox includes it into
//!   the commit (a partial commit, as the checkboxes of JetBrains' diff).
//! - **Unified**: one read-only text — unchanged lines once, the HEAD lines of a block, then its new
//!   lines — with the line numbers of both versions in its own gutter.
//!
//! Blocks are colored by kind (added green, modified blue, deleted gray), the changed words of a
//! modified block stronger; where a side has no lines of a block, a thin line marks the place. F7 /
//! ⇧F7 go to the next / previous difference, F4 opens the file at the cursor.
//!
//! The viewer diffs the texts itself, in the background, whenever the working copy changes or HEAD
//! moves (the git hub reports it): with the same algorithm and the same line endings as the gutter
//! and the commit, so a block here is the block the commit checkboxes know.
//!
//! **Comparisons** ([`DiffView::compare`]) show a file at two revisions (Compare with Current, the
//! files of a stash), or at a revision and the working copy (Show Diff with Working Tree): the
//! revisions are read once, when the tab opens; a working copy side is the file's editor, as in the
//! HEAD diff, and its blocks can be reverted to the revision's; commit checkboxes are only in the
//! HEAD diff. The editors draw the
//! colors: before each frame the viewer hands each of them its [`Decorations`]
//! (`Editor::frame_decorations`), which the element takes; the same editor shown in its own tab gets
//! none.
//!
//! **Claude's proposals** ([`DiffView::proposal`], stage 9): an edit Claude asks permission for — the
//! file as it is on the left, the proposed text on the right in an editor of its own (editable,
//! never saved: the answer carries it). A block's arrow puts the file's lines back on the right,
//! which rejects that change; a banner accepts (⌘↵) or rejects the proposal, and the tab closes once
//! the question is answered here, in the chat, or withdrawn.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use flux_core::text::{line_len, line_start};
use flux_core::{Document, Rope};
use flux_git::{FileStatus, Hunk, HunkKind};
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    Hsla, KeyBinding, PaintQuad, PathBuilder, Pixels, Point, Render, ScrollWheelEvent,
    SharedString, Subscription, Task, TextRun, Window, actions, canvas, div, fill, font, point,
    prelude::*, px,
};

use crate::claude_session::{ClaudeSession, SessionEvent};
use crate::editor::{self, Editor, EditorEvent};
use crate::element::LayoutCache;
use crate::git::GitStore;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::theme::{self, Theme, UiColors};
use crate::ui;
use crate::workspace::Location;

actions!(
    diff,
    [
        /// F7: the next changed block.
        NextDifference,
        /// ⇧F7: the previous changed block.
        PreviousDifference,
        /// F4: the file at the cursor, in its own tab.
        JumpToSource,
        /// ⌃⌘→: the HEAD lines of the block at the cursor go back into the working copy.
        RevertChange,
        /// Side by side ↔ unified.
        ToggleUnified,
        /// ⌘↵ on a Claude proposal: the right side is the answer.
        AcceptProposal,
        /// A Claude proposal is refused.
        RejectProposal,
    ]
);

pub fn init(cx: &mut App) {
    let context = Some("DiffView");
    cx.bind_keys([
        // As in the diff of JetBrains IDEs.
        KeyBinding::new("f7", NextDifference, context),
        KeyBinding::new("shift-f7", PreviousDifference, context),
        KeyBinding::new("f4", JumpToSource, context),
        // "Accept Left Side": the HEAD block replaces the working copy's.
        KeyBinding::new("cmd-ctrl-right", RevertChange, context),
        KeyBinding::new("cmd-enter", AcceptProposal, context),
    ]);
}

/// The toolbar above the sides, and the captions under it.
const TOOLBAR_HEIGHT: f32 = 36.;
const CAPTION_HEIGHT: f32 = 24.;
/// The strip between the sides: the connectors, the revert arrows, the commit checkboxes.
const DIVIDER_WIDTH: f32 = 40.;
const DIVIDER_BUTTON: f32 = 16.;
/// Changed words are looked for only in blocks up to this many lines a side (a rewritten file
/// would cost a long diff every time it changes).
const WORD_DIFF_MAX_LINES: usize = 300;
/// How many leading characters are checked for NUL to call the working copy binary.
const BINARY_PROBE: usize = 8000;

// --- Decorations: what the viewer draws in an editor ---

/// What a host view draws in an editor for one frame.
#[derive(Debug, Clone, Default)]
pub struct Decorations {
    /// Whole lines (zero-based, half-open) with their background: changed blocks.
    pub lines: Vec<(Range<usize>, Hsla)>,
    /// Character ranges of the document with their background: changed words (each within a line).
    pub words: Vec<(Range<usize>, Hsla)>,
    /// Places where the other side has a block and this one has no lines: a thin line at the top of
    /// the line (at the end of the text past the last line).
    pub boundaries: Vec<(usize, Hsla)>,
}

/// The quads of the visible decorations.
#[derive(Default)]
pub struct DecorationsPaint {
    lines: Vec<PaintQuad>,
    words: Vec<PaintQuad>,
}

impl DecorationsPaint {
    /// Line backgrounds and boundaries: under the current line and the gutter numbers.
    pub fn paint_lines(&mut self, window: &mut Window) {
        for quad in self.lines.drain(..) {
            window.paint_quad(quad);
        }
    }

    /// Word backgrounds: under the selection and the text.
    pub fn paint_words(&mut self, window: &mut Window) {
        for quad in self.words.drain(..) {
            window.paint_quad(quad);
        }
    }
}

/// Quads for the decorations that touch the visible lines.
pub fn prepaint_decorations(
    decorations: Option<&Decorations>,
    text: &Rope,
    layout: &LayoutCache,
    last_line: usize,
    bounds: Bounds<Pixels>,
    _ui: &UiColors,
) -> DecorationsPaint {
    let mut paint = DecorationsPaint::default();
    let Some(decorations) = decorations else {
        return paint;
    };
    let first_line = layout.first_line;
    for (lines, color) in &decorations.lines {
        let start = lines.start.max(first_line);
        let end = lines.end.min(last_line);
        if start >= end {
            continue;
        }
        paint.lines.push(fill(
            Bounds::from_corners(
                point(bounds.left(), layout.line_top(start)),
                point(bounds.right(), layout.line_top(end)),
            ),
            *color,
        ));
    }
    for (line, color) in &decorations.boundaries {
        if *line < first_line || *line > last_line {
            continue;
        }
        let y = layout.line_top(*line);
        paint.lines.push(fill(
            Bounds::from_corners(
                point(bounds.left(), y - px(1.)),
                point(bounds.right(), y + px(1.)),
            ),
            *color,
        ));
    }
    let len = text.len_chars();
    for (range, color) in &decorations.words {
        let line = text.char_to_line(range.start.min(len));
        if line < first_line || line >= last_line {
            continue;
        }
        let Some(line_layout) = layout.line(line) else {
            continue;
        };
        let start = line_start(text, line);
        let columns = line_len(text, line);
        let x0 = line_layout.x_for_column((range.start - start).min(columns));
        let x1 = line_layout.x_for_column(range.end.saturating_sub(start).min(columns));
        if x1 <= x0 {
            continue;
        }
        let top = layout.line_top(line);
        paint.words.push(fill(
            Bounds::from_corners(
                point(layout.origin.x + x0, top),
                point(layout.origin.x + x1, top + layout.line_height),
            ),
            *color,
        ));
    }
    paint
}

// --- The model: the compared texts and their blocks ---

/// The compared texts (without `\r` before `\n`) and their changed blocks.
struct DiffModel {
    base: Arc<str>,
    text: Arc<str>,
    hunks: Vec<Hunk>,
    /// The changed words of each block (empty but for modified blocks), index-aligned with `hunks`.
    words: Vec<BlockWords>,
}

/// Changed words of a block: (line in the block, character columns), on each side.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct BlockWords {
    old: Vec<(u32, Range<usize>)>,
    new: Vec<(u32, Range<usize>)>,
}

/// Diffs two texts (already without `\r`): lines, then words inside modified blocks.
fn compute_model(base: Arc<str>, text: Arc<str>) -> DiffModel {
    let hunks = flux_git::diff_lines(&base, &text);
    let old_lines = line_ranges(&base);
    let new_lines = line_ranges(&text);
    let words = hunks
        .iter()
        .map(|hunk| {
            let small =
                hunk.old.len() <= WORD_DIFF_MAX_LINES && hunk.new.len() <= WORD_DIFF_MAX_LINES;
            if hunk.kind() != HunkKind::Modified || !small {
                return BlockWords::default();
            }
            let old = block(&base, &old_lines, &hunk.old);
            let new = block(&text, &new_lines, &hunk.new);
            let (removed, added) = flux_git::diff_words(old, new);
            BlockWords {
                old: line_spans(old, &removed),
                new: line_spans(new, &added),
            }
        })
        .collect();
    DiffModel {
        base,
        text,
        hunks,
        words,
    }
}

/// Text without `\r` before `\n`: the gutter, the commit and the viewer compare lines this way.
fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// Byte ranges of the lines, each with its line break.
fn line_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for (at, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            ranges.push(start..at + 1);
            start = at + 1;
        }
    }
    if start < text.len() {
        ranges.push(start..text.len());
    }
    ranges
}

/// The lines `lines` of a text, as one string slice.
fn block<'a>(text: &'a str, ranges: &[Range<usize>], lines: &Range<u32>) -> &'a str {
    let first = ranges.get(lines.start as usize);
    let last = (lines.end as usize)
        .checked_sub(1)
        .and_then(|last| ranges.get(last));
    match (first, last) {
        (Some(first), Some(last)) if !lines.is_empty() => &text[first.start..last.end],
        _ => "",
    }
}

/// Byte ranges of a block → (line in the block, character columns), split at line breaks; the
/// breaks themselves aren't marked.
fn line_spans(block: &str, ranges: &[Range<usize>]) -> Vec<(u32, Range<usize>)> {
    let lines = line_ranges(block);
    let mut spans = Vec::new();
    for range in ranges {
        let first = lines.partition_point(|line| line.end <= range.start);
        for (index, line) in lines.iter().enumerate().skip(first) {
            if line.start >= range.end {
                break;
            }
            let content_end = if block[line.clone()].ends_with('\n') {
                line.end - 1
            } else {
                line.end
            };
            let start = range.start.max(line.start);
            let end = range.end.min(content_end);
            if start >= end {
                continue;
            }
            let column = block[line.start..start].chars().count();
            let width = block[start..end].chars().count();
            spans.push((index as u32, column..column + width));
        }
    }
    spans
}

/// The line of the other side that corresponds to (fractional) line `line` of one side: the same
/// distance from the nearest block above; inside a block, the same share of it. `from_old` — `line`
/// is a HEAD line.
fn map_line(hunks: &[Hunk], line: f32, from_old: bool) -> f32 {
    let mut delta = 0.;
    for hunk in hunks {
        let (from, to) = if from_old {
            (&hunk.old, &hunk.new)
        } else {
            (&hunk.new, &hunk.old)
        };
        if line < from.start as f32 {
            break;
        }
        if line < from.end as f32 {
            let share = (line - from.start as f32) / from.len() as f32;
            return to.start as f32 + share * to.len() as f32;
        }
        delta = to.end as f32 - from.end as f32;
    }
    (line + delta).max(0.)
}

/// Where a follower scrolls (pixels) when the leader is at `leader`: the line in the middle of the
/// view maps through the blocks, so a block stays in sight on both sides; at its top or its bottom,
/// the leader takes the follower to its own top or bottom.
fn follow(
    hunks: &[Hunk],
    leader: f32,
    leader_max: f32,
    follower_max: f32,
    height: f32,
    from_old: bool,
) -> f32 {
    let lh = theme::LINE_HEIGHT;
    if leader <= 0. {
        return 0.;
    }
    if leader >= leader_max {
        return follower_max;
    }
    let middle = (leader + height / 2.) / lh;
    (map_line(hunks, middle, from_old) * lh - height / 2.).clamp(0., follower_max)
}

// --- The unified text ---

/// A line of the unified text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnifiedLine {
    /// Unchanged: its numbers in HEAD and in the working copy.
    Context { old: u32, new: u32 },
    /// A HEAD line of block `hunk`.
    Removed { old: u32, hunk: usize },
    /// A working copy line of block `hunk`.
    Added { new: u32, hunk: usize },
}

/// The unified text: unchanged lines once; for each block, its HEAD lines, then its new lines.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Unified {
    text: String,
    lines: Vec<UnifiedLine>,
    /// The first unified line of each block.
    hunk_starts: Vec<usize>,
}

impl Unified {
    /// The unified line showing working copy line `new` (the first line of the next block when the
    /// line is gone).
    fn line_of_new(&self, new: u32) -> usize {
        self.lines
            .iter()
            .position(|line| match line {
                UnifiedLine::Context { new: n, .. } | UnifiedLine::Added { new: n, .. } => {
                    *n >= new
                }
                UnifiedLine::Removed { .. } => false,
            })
            .unwrap_or(self.lines.len().saturating_sub(1))
    }

    /// The working copy line a unified line stands for: a removed line — where its block is.
    fn new_line_of(&self, line: usize, hunks: &[Hunk]) -> u32 {
        match self.lines.get(line) {
            Some(UnifiedLine::Context { new, .. } | UnifiedLine::Added { new, .. }) => *new,
            Some(UnifiedLine::Removed { hunk, .. }) => hunks[*hunk].new.start,
            None => 0,
        }
    }
}

fn build_unified(base: &str, text: &str, hunks: &[Hunk]) -> Unified {
    let old_lines = line_ranges(base);
    let new_lines = line_ranges(text);
    let content = |text: &str, range: &Range<usize>| -> String {
        text[range.clone()].trim_end_matches('\n').to_string()
    };
    let mut texts: Vec<String> = Vec::new();
    let mut lines = Vec::new();
    let mut hunk_starts = Vec::new();
    let (mut old, mut new) = (0u32, 0u32);
    let context = |until_new: u32,
                   old: &mut u32,
                   new: &mut u32,
                   texts: &mut Vec<String>,
                   lines: &mut Vec<UnifiedLine>| {
        while *new < until_new {
            if let Some(range) = new_lines.get(*new as usize) {
                texts.push(content(text, range));
                lines.push(UnifiedLine::Context {
                    old: *old,
                    new: *new,
                });
            }
            *old += 1;
            *new += 1;
        }
    };
    for (index, hunk) in hunks.iter().enumerate() {
        context(hunk.new.start, &mut old, &mut new, &mut texts, &mut lines);
        hunk_starts.push(lines.len());
        for line in hunk.old.clone() {
            if let Some(range) = old_lines.get(line as usize) {
                texts.push(content(base, range));
                lines.push(UnifiedLine::Removed {
                    old: line,
                    hunk: index,
                });
            }
        }
        for line in hunk.new.clone() {
            if let Some(range) = new_lines.get(line as usize) {
                texts.push(content(text, range));
                lines.push(UnifiedLine::Added {
                    new: line,
                    hunk: index,
                });
            }
        }
        old = hunk.old.end;
        new = hunk.new.end;
    }
    context(
        new_lines.len() as u32,
        &mut old,
        &mut new,
        &mut texts,
        &mut lines,
    );
    Unified {
        text: texts.join("\n"),
        lines,
        hunk_starts,
    }
}

// --- The viewer ---

/// What the HEAD side is.
#[derive(Clone)]
enum Base {
    /// Not read yet (or the repositories are still being found).
    Loading,
    /// The HEAD version, without `\r` before `\n`; empty for a file that isn't in HEAD.
    Text(Arc<str>),
    /// In HEAD, but not comparable: binary or too large.
    Unreadable,
    /// The file isn't in a repository.
    NoRepository,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

/// One side of a comparison (Compare with Current, Show Diff with Working Tree, stash files): a
/// file at a revision, or the working copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffSide {
    /// `rev:path` of the repository: a commit, a branch, `stash@{0}^1`, an index stage (`:2`);
    /// `path` is relative to the working tree (another path than the file's after a rename);
    /// `label` is the caption ("main", "Stash · WIP on main").
    Revision {
        rev: String,
        path: String,
        label: String,
    },
    /// The file in the working tree: its editor, editable.
    WorkingCopy,
}

/// What the diff viewer asks the workspace to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffViewEvent {
    /// Jump to Source (F4): the file at a location, in its own tab.
    OpenFile(Location),
    /// The view is done (an answered Claude proposal): its tab closes.
    Close,
}

/// The diff of one file: HEAD ↔ working copy.
pub struct DiffView {
    path: PathBuf,
    /// The path as the git hub knows it (canonical): the key of the commit checkboxes.
    key: PathBuf,
    git: Entity<GitStore>,
    base: Base,
    /// The file isn't in HEAD (new, untracked): the HEAD side shows a note instead of text.
    new_file: bool,
    /// The HEAD side; made once the HEAD version is read.
    base_editor: Option<Entity<Editor>>,
    /// The working copy; `None` — the file is deleted.
    working: Option<Entity<Editor>>,
    /// The working copy has NUL bytes: it isn't compared.
    binary: bool,
    /// The viewer opened the working copy's editor and the file has no tab of its own: closing the
    /// viewer asks to save its changes.
    owns_working: bool,
    model: Option<Rc<DiffModel>>,
    /// A diff runs in the background; `stale` — the texts changed meanwhile, run again after it.
    computing: Option<Task<()>>,
    stale: bool,
    unified: bool,
    /// The unified text's editor, and the model it was built from.
    unified_editor: Option<Entity<Editor>>,
    unified_text: Option<(Rc<DiffModel>, Rc<Unified>)>,
    /// The side that scrolled last leads the other one.
    driver: Side,
    /// The scroll offsets of both sides as of the last frame: the one that moved alone leads.
    synced: Option<(Point<f32>, Point<f32>)>,
    /// A comparison of two revisions (or a revision and the working copy) instead of HEAD and the
    /// working copy.
    compare: Option<Compare>,
    /// An edit Claude proposes instead of HEAD and the working copy.
    proposal: Option<Proposal>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

/// An edit Claude asks permission for: whose question it is, and the text Claude proposed (an
/// untouched right side allows the call as it is).
struct Proposal {
    session: Entity<ClaudeSession>,
    request: String,
    proposed: String,
    /// The answer went (or the question did): the tab is closing.
    done: bool,
}

/// What Claude is told when the user rejects a proposal in its diff: the turn stops.
const REJECTED: &str =
    "The user rejected this edit. Stop and wait for the user to tell you how to proceed.";

/// What a comparison compares, and how its right side is doing.
struct Compare {
    repo: usize,
    left: DiffSide,
    right: DiffSide,
    right_state: RightState,
}

/// The right side of a comparison: a revision is read in the background; a working copy is there
/// from the start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RightState {
    Ready,
    Loading,
    /// The revision has no such file.
    Missing,
    /// Binary or too large.
    Unreadable,
}

/// A file at a revision, as a side of a comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RevisionText {
    Text(Arc<str>),
    /// No such file there.
    Missing,
    /// Binary or too large to compare.
    Unreadable,
}

/// Larger files aren't compared (as the HEAD diff's limit).
const MAX_COMPARED_BYTES: usize = 4 * 1024 * 1024;

impl GitStore {
    /// A file's content at a revision of a repository, read once (one `git cat-file`): text, or why
    /// there is none.
    fn read_revision(
        &self,
        repo: usize,
        rev: &str,
        path: &str,
        cx: &mut Context<Self>,
    ) -> Task<RevisionText> {
        let (rev, path) = (rev.to_string(), path.to_string());
        let read = self.read(repo, cx, move |repo| {
            flux_git::BlobReader::new(repo).read(&rev, &path)
        });
        cx.background_spawn(async move { revision_text(read.await.ok().flatten()) })
    }
}

/// What a read of `rev:path` gives a comparison.
fn revision_text(content: Option<Vec<u8>>) -> RevisionText {
    match content {
        None => RevisionText::Missing,
        Some(content) if content.len() > MAX_COMPARED_BYTES || flux_git::is_binary(&content) => {
            RevisionText::Unreadable
        }
        Some(content) => RevisionText::Text(String::from_utf8_lossy(&content).as_ref().into()),
    }
}

impl EventEmitter<DiffViewEvent> for DiffView {}

impl DiffView {
    pub fn new(
        path: PathBuf,
        git: Entity<GitStore>,
        working: Option<Entity<Editor>>,
        owns_working: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::build(path, git, working, owns_working, None, window, cx);
        view.reload_base(window, cx);
        view
    }

    /// A comparison of two revisions, or of a revision and the working copy (`working` — the file's
    /// editor, for a working copy side). The HEAD diff is [`Self::new`].
    #[allow(clippy::too_many_arguments)]
    pub fn compare(
        path: PathBuf,
        repo: usize,
        left: DiffSide,
        right: DiffSide,
        git: Entity<GitStore>,
        working: Option<Entity<Editor>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let on_working_copy = right == DiffSide::WorkingCopy;
        let compare = Compare {
            repo,
            left,
            right,
            right_state: if on_working_copy {
                RightState::Ready
            } else {
                RightState::Loading
            },
        };
        let working = working.filter(|_| on_working_copy);
        let mut view = Self::build(path, git, working, false, Some(compare), window, cx);
        view.load_comparison(window, cx);
        view
    }

    fn build(
        path: PathBuf,
        git: Entity<GitStore>,
        working: Option<Entity<Editor>>,
        owns_working: bool,
        compare: Option<Compare>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // A commit, a checkout or the first status (a new file is known by it) may change the HEAD
        // side. A comparison's revisions are read once.
        let mut subscriptions = vec![cx.observe_in(&git, window, |this, _, window, cx| {
            if this.compare.is_none() && this.proposal.is_none() {
                this.reload_base(window, cx)
            }
        })];
        let mut binary = false;
        if let Some(editor) = &working {
            subscriptions.push(cx.subscribe(editor, |this, _, event: &EditorEvent, cx| {
                if *event == EditorEvent::Edited {
                    this.recompute(cx);
                }
            }));
            binary = editor
                .read(cx)
                .document
                .text()
                .chars()
                .take(BINARY_PROBE)
                .any(|c| c == '\0');
        }
        Self {
            key: canonical(&path),
            path,
            git,
            base: Base::Loading,
            new_file: false,
            base_editor: None,
            working,
            binary,
            owns_working,
            model: None,
            computing: None,
            stale: false,
            unified: false,
            unified_editor: None,
            unified_text: None,
            driver: Side::Right,
            synced: None,
            compare,
            proposal: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// An edit Claude proposes (`request` of `session`): `original` — the file now (`None` — it
    /// doesn't exist yet), `proposed` — the text after the edit, on the right in an editor of its own.
    #[allow(clippy::too_many_arguments)]
    pub fn proposal(
        path: PathBuf,
        original: Option<String>,
        proposed: String,
        session: Entity<ClaudeSession>,
        request: String,
        git: Entity<GitStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let right = cx.new(|cx| {
            let mut editor = Editor::new(Document::from_text(&proposed), window, cx);
            editor.set_highlight_path(&path, cx);
            editor
        });
        let mut view = Self::build(path, git, Some(right), false, None, window, cx);
        let subscription = cx.subscribe(&session, {
            let request = request.clone();
            move |this, _, event: &SessionEvent, cx| {
                let gone = match event {
                    SessionEvent::PendingRemoved(id) => *id == request,
                    SessionEvent::Exited => true,
                    _ => false,
                };
                if gone {
                    this.close_proposal(cx);
                }
            }
        });
        view._subscriptions.push(subscription);
        view.proposal = Some(Proposal {
            session,
            request,
            proposed,
            done: false,
        });
        let base = match original.as_deref() {
            Some(text) if text.contains('\r') => normalize(text),
            Some(text) => text.to_string(),
            None => String::new(),
        };
        view.apply_base(Base::Text(base.into()), original.is_none(), window, cx);
        view
    }

    /// Whether this is a Claude proposal (not a diff of the file's changes: ⌘D opens its own tab).
    pub fn is_proposal(&self) -> bool {
        self.proposal.is_some()
    }

    /// Whether this is the proposal of the question `request` of `session`.
    pub fn is_proposal_of(&self, session: &Entity<ClaudeSession>, request: &str) -> bool {
        self.proposal
            .as_ref()
            .is_some_and(|proposal| &proposal.session == session && proposal.request == request)
    }

    /// Accept: the right side — the proposal as it is, or as the user changed it — answers the
    /// question.
    fn accept_proposal(&mut self, cx: &mut Context<Self>) {
        let (Some(proposal), Some(right)) = (&self.proposal, &self.working) else {
            return;
        };
        if proposal.done {
            return;
        }
        let text = right.read(cx).document.text().to_string();
        let answer = if text == proposal.proposed {
            flux_claude::Answer::Allow {
                remember: Vec::new(),
            }
        } else {
            flux_claude::Answer::Edit {
                text,
                remember: Vec::new(),
            }
        };
        self.answer_proposal(answer, cx);
    }

    fn reject_proposal(&mut self, cx: &mut Context<Self>) {
        self.answer_proposal(
            flux_claude::Answer::Deny {
                message: REJECTED.to_string(),
                interrupt: true,
            },
            cx,
        );
    }

    fn answer_proposal(&mut self, answer: flux_claude::Answer, cx: &mut Context<Self>) {
        let Some(proposal) = &self.proposal else {
            return;
        };
        if proposal.done {
            return;
        }
        let (session, request) = (proposal.session.clone(), proposal.request.clone());
        session.update(cx, |session, cx| session.answer(&request, answer, cx));
        self.close_proposal(cx);
    }

    /// The question is over: the tab closes (once).
    fn close_proposal(&mut self, cx: &mut Context<Self>) {
        if let Some(proposal) = &mut self.proposal
            && !proposal.done
        {
            proposal.done = true;
            cx.emit(DiffViewEvent::Close);
        }
    }

    /// The sides of a comparison; `None` — the HEAD diff.
    pub fn compares(&self) -> Option<(&DiffSide, &DiffSide)> {
        self.compare
            .as_ref()
            .map(|compare| (&compare.left, &compare.right))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The working copy's editor — in a comparison of two revisions, the right side's read-only
    /// one.
    pub fn working(&self) -> Option<&Entity<Editor>> {
        self.working.as_ref()
    }

    /// The editor of the file in the working tree, if a side shows it: the status bar shows its
    /// position; a comparison of two revisions has none (its right side is a pathless read-only
    /// editor).
    pub fn working_copy(&self) -> Option<&Entity<Editor>> {
        let on_working_copy = self
            .compare
            .as_ref()
            .is_none_or(|compare| compare.right == DiffSide::WorkingCopy);
        // A proposal's right side is Claude's text, not the file.
        self.working
            .as_ref()
            .filter(|_| on_working_copy && self.proposal.is_none())
    }

    /// Whether closing the viewer must take care of the working copy's unsaved changes.
    pub fn owns_working(&self) -> bool {
        self.owns_working
    }

    pub fn set_owns_working(&mut self, owns: bool) {
        self.owns_working = owns;
    }

    /// The tab's label: the file name; a comparison adds what it compares ("main.rs (main ↔
    /// feature/x)"; with the working copy only the revision: "main.rs (feature/x)").
    pub fn title(&self) -> SharedString {
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if self.proposal.is_some() {
            return trf("{0} — Claude", &[&name]).into();
        }
        match &self.compare {
            Some(compare) if compare.right == DiffSide::WorkingCopy => {
                format!("{name} ({})", short_label(&compare.left, 20)).into()
            }
            // A stash's file against the commit the stash was made on: the captions tell the rest.
            Some(compare) if is_stash_pair(&compare.left, &compare.right) => {
                format!("{name} ({})", tr("stash")).into()
            }
            Some(compare) => format!(
                "{name} ({} ↔ {})",
                short_label(&compare.left, 14),
                short_label(&compare.right, 14)
            )
            .into(),
            None => name.into(),
        }
    }

    // --- A comparison's sides ---

    /// Reads the revisions of a comparison in the background.
    fn load_comparison(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(compare) = &self.compare else {
            return;
        };
        let repo = compare.repo;
        let left = self.read_side(repo, &compare.left.clone(), cx);
        cx.spawn_in(window, async move |this, cx| {
            let text = left.await;
            this.update_in(cx, |this, window, cx| this.set_left(text, window, cx))
                .ok();
        })
        .detach();
        if let Some(compare) = &self.compare
            && compare.right != DiffSide::WorkingCopy
        {
            let right = self.read_side(repo, &compare.right.clone(), cx);
            cx.spawn_in(window, async move |this, cx| {
                let text = right.await;
                this.update_in(cx, |this, window, cx| this.set_right(text, window, cx))
                    .ok();
            })
            .detach();
        }
    }

    /// A side's content: a revision from git, the working copy from disk (a left side only).
    fn read_side(
        &self,
        repo: usize,
        side: &DiffSide,
        cx: &mut Context<Self>,
    ) -> Task<RevisionText> {
        match side {
            DiffSide::Revision { rev, path, .. } => self
                .git
                .update(cx, |git, cx| git.read_revision(repo, rev, path, cx)),
            DiffSide::WorkingCopy => {
                let path = self.path.clone();
                cx.background_spawn(async move { revision_text(std::fs::read(&path).ok()) })
            }
        }
    }

    /// The left side of a comparison arrived.
    fn set_left(&mut self, text: RevisionText, window: &mut Window, cx: &mut Context<Self>) {
        let (base, new_file) = match text {
            RevisionText::Text(text) if text.contains('\r') => {
                (Base::Text(normalize(&text).into()), false)
            }
            RevisionText::Text(text) => (Base::Text(text), false),
            // The file isn't there: everything on the right is new.
            RevisionText::Missing => (Base::Text("".into()), true),
            RevisionText::Unreadable => (Base::Unreadable, false),
        };
        self.apply_base(base, new_file, window, cx);
    }

    /// The right side of a comparison (a revision) arrived: a read-only editor with its text.
    fn set_right(&mut self, text: RevisionText, window: &mut Window, cx: &mut Context<Self>) {
        let Some(compare) = &mut self.compare else {
            return;
        };
        compare.right_state = match &text {
            RevisionText::Text(_) => RightState::Ready,
            RevisionText::Missing => RightState::Missing,
            RevisionText::Unreadable => RightState::Unreadable,
        };
        if let RevisionText::Text(text) = text {
            let path = self.path.clone();
            self.working = Some(cx.new(|cx| {
                let mut editor = Editor::new(Document::from_text(&text), window, cx);
                editor.set_highlight_path(&path, cx);
                editor.read_only = true;
                editor
            }));
            self.synced = None;
        }
        self.recompute(cx);
        cx.notify();
    }

    /// The caption of a side.
    fn side_caption(side: &DiffSide) -> String {
        match side {
            DiffSide::Revision { label, .. } => label.clone(),
            DiffSide::WorkingCopy => tr("Working copy").to_string(),
        }
    }

    /// Whether F4 has a file to open: the working copy is on the right, or the file exists in the
    /// working tree (a comparison of revisions opens the file as it is now).
    fn can_jump(&self) -> bool {
        if self.proposal.is_some() {
            return self.path.exists();
        }
        match &self.compare {
            None => self.working.is_some(),
            Some(compare) if compare.right == DiffSide::WorkingCopy => self.working.is_some(),
            Some(_) => self.path.exists(),
        }
    }

    /// Whether the blocks get revert arrows: the right side is the working copy, or a proposal
    /// (the arrow rejects that change).
    fn can_revert(&self) -> bool {
        self.working_copy().is_some() || (self.is_proposal() && self.working.is_some())
    }

    /// The right side of a comparison is still being read.
    fn right_loading(&self) -> bool {
        self.compare
            .as_ref()
            .is_some_and(|compare| compare.right_state == RightState::Loading)
    }

    // --- The HEAD side ---

    /// Asks the hub for the HEAD version (it keeps it per HEAD: a repeated ask is free).
    fn reload_base(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.key.clone();
        let read = self.git.update(cx, |git, cx| git.base_text(&key, cx));
        cx.spawn_in(window, async move |this, cx| {
            let base = read.await;
            this.update_in(cx, |this, window, cx| this.set_base(base, window, cx))
                .ok();
        })
        .detach();
    }

    fn set_base(&mut self, text: Option<Arc<str>>, window: &mut Window, cx: &mut Context<Self>) {
        let git = self.git.read(cx);
        let repo = git.repo_for(&self.key);
        let head_known = repo.is_some_and(|repo| repo.status.branch.oid.is_some());
        let (base, new_file) = match text {
            Some(text) if text.contains('\r') => (Base::Text(normalize(&text).into()), false),
            Some(text) => (Base::Text(text), false),
            None if repo.is_none() && git.is_discovering() => (Base::Loading, false),
            None if repo.is_none() => (Base::NoRepository, false),
            None => match git.status_of(&self.key) {
                Some(FileStatus::Untracked | FileStatus::Added) => (Base::Text("".into()), true),
                // Before the first status, a new file looks like any other.
                _ if !head_known => (Base::Loading, false),
                Some(_) => (Base::Unreadable, false),
                None => (Base::Unreadable, false),
            },
        };
        self.apply_base(base, new_file, window, cx);
    }

    /// The left side changed: its read-only editor follows, and the blocks are computed again.
    fn apply_base(
        &mut self,
        base: Base,
        new_file: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let same = match (&self.base, &base) {
            (Base::Text(old), Base::Text(new)) => Arc::ptr_eq(old, new) || old == new,
            (Base::Loading, Base::Loading)
            | (Base::Unreadable, Base::Unreadable)
            | (Base::NoRepository, Base::NoRepository) => true,
            _ => false,
        };
        if same && self.new_file == new_file {
            return;
        }
        self.base = base;
        self.new_file = new_file;
        let text = match &self.base {
            Base::Text(text) if !new_file => Some(text.clone()),
            _ => None,
        };
        match (text, self.base_editor.clone()) {
            (Some(text), Some(editor)) => replace_text(&editor, &text, cx),
            (Some(text), None) => {
                let path = self.path.clone();
                self.base_editor = Some(cx.new(|cx| {
                    let mut editor = Editor::new(Document::from_text(&text), window, cx);
                    editor.set_highlight_path(&path, cx);
                    editor.read_only = true;
                    editor
                }));
                self.synced = None;
            }
            (None, _) => {
                self.base_editor = None;
                self.synced = None;
            }
        }
        self.recompute(cx);
        cx.notify();
    }

    // --- The diff ---

    /// Diffs the texts in the background; the latest result is drawn.
    fn recompute(&mut self, cx: &mut Context<Self>) {
        if self.right_loading() {
            return;
        }
        let Base::Text(base) = &self.base else {
            self.model = None;
            return;
        };
        if self.computing.is_some() {
            self.stale = true;
            return;
        }
        let base = base.clone();
        let text = self
            .working
            .as_ref()
            .map(|editor| editor.read(cx).document.text().to_string())
            .unwrap_or_default();
        let run = cx.background_spawn(async move {
            let text: Arc<str> = match text.contains('\r') {
                true => normalize(&text).into(),
                false => text.into(),
            };
            compute_model(base, text)
        });
        self.computing = Some(cx.spawn(async move |this, cx| {
            let model = run.await;
            this.update(cx, |this, cx| {
                this.computing = None;
                this.model = Some(Rc::new(model));
                if this.stale {
                    this.stale = false;
                    this.recompute(cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// The model, if it is of the current texts (nothing changed since it was computed).
    fn fresh_model(&self) -> Option<Rc<DiffModel>> {
        self.model
            .clone()
            .filter(|_| self.computing.is_none() && !self.stale)
    }

    /// Whether the files can be shown side by side or unified at all.
    fn comparable(&self) -> bool {
        matches!(self.base, Base::Text(_)) && !self.binary
    }

    // --- Navigation ---

    /// F7 / ⇧F7: the cursor goes to the next / previous block after (before) the cursor of the
    /// focused side, and both sides show it.
    fn go_to_difference(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(model) = self.model.clone() else {
            return;
        };
        if model.hunks.is_empty() {
            return;
        }
        if self.unified {
            return self.go_to_unified_difference(forward, window, cx);
        }
        let left_focused = self
            .base_editor
            .as_ref()
            .is_some_and(|editor| editor.focus_handle(cx).is_focused(window));
        let (editor, from_old) = match (&self.working, &self.base_editor) {
            (Some(working), _) if !left_focused => (working.clone(), false),
            (_, Some(base)) => (base.clone(), true),
            (Some(working), None) => (working.clone(), false),
            (None, None) => return,
        };
        let line = cursor_line(&editor, cx) as u32;
        let start = |hunk: &Hunk| {
            if from_old {
                hunk.old.start
            } else {
                hunk.new.start
            }
        };
        let target = if forward {
            model.hunks.iter().position(|hunk| start(hunk) > line)
        } else {
            model.hunks.iter().rposition(|hunk| start(hunk) < line)
        };
        if let Some(index) = target {
            let hunk = model.hunks[index].clone();
            for (side, line) in [
                (&self.base_editor, hunk.old.start),
                (&self.working, hunk.new.start),
            ] {
                if let Some(editor) = side {
                    move_cursor(editor, line as usize, cx);
                }
            }
            self.driver = if from_old { Side::Left } else { Side::Right };
            cx.notify();
        }
    }

    fn go_to_unified_difference(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_unified(window, cx);
        let (Some(editor), Some((_, unified))) = (&self.unified_editor, &self.unified_text) else {
            return;
        };
        let line = cursor_line(editor, cx);
        let target = if forward {
            unified.hunk_starts.iter().find(|start| **start > line)
        } else {
            unified
                .hunk_starts
                .iter()
                .rev()
                .find(|start| **start < line)
        };
        if let Some(target) = target {
            move_cursor(editor, *target, cx);
        }
    }

    /// F4: the file in its own tab, at the cursor's line (a HEAD line — where it is in the working
    /// copy).
    fn jump_to_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_jump() {
            return;
        }
        let model = self.model.clone();
        let hunks: &[Hunk] = model.as_ref().map_or(&[], |model| &model.hunks);
        // A proposal: the file is the left side; a line of the proposal goes where it would be in
        // the file.
        if self.proposal.is_some() && !self.unified {
            let line = match (&self.base_editor, &self.working) {
                (Some(base), _) if base.focus_handle(cx).is_focused(window) => {
                    cursor_line(base, cx)
                }
                (_, Some(right)) => {
                    map_line(hunks, cursor_line(right, cx) as f32, false).floor() as usize
                }
                _ => 0,
            };
            cx.emit(DiffViewEvent::OpenFile(Location {
                path: self.path.clone(),
                line,
                start: 0,
                end: 0,
            }));
            return;
        }
        let (line, column) = if self.unified {
            match (&self.unified_editor, &self.unified_text) {
                (Some(editor), Some((_, unified))) => (
                    unified.new_line_of(cursor_line(editor, cx), hunks) as usize,
                    0,
                ),
                _ => (0, 0),
            }
        } else {
            match &self.base_editor {
                Some(base) if base.focus_handle(cx).is_focused(window) => {
                    let line = cursor_line(base, cx) as f32;
                    (map_line(hunks, line, true).floor() as usize, 0)
                }
                _ => {
                    let working = self.working.as_ref().expect("checked above").read(cx);
                    let text = working.document.text();
                    let head = working.document.selection().primary().head;
                    let line = text.char_to_line(head);
                    (line, head - line_start(text, line))
                }
            }
        };
        cx.emit(DiffViewEvent::OpenFile(Location {
            path: self.path.clone(),
            line,
            start: column,
            end: column,
        }));
    }

    /// Switches between side by side and unified, keeping the place: the top line of one view is
    /// the top line of the other.
    fn set_unified(&mut self, unified: bool, window: &mut Window, cx: &mut Context<Self>) {
        if unified == self.unified || !self.comparable() {
            return;
        }
        let lh = theme::LINE_HEIGHT;
        self.unified = unified;
        if unified {
            self.ensure_unified(window, cx);
            let top = self
                .working
                .as_ref()
                .map_or(0., |editor| editor.read(cx).scroll.y / lh);
            if let (Some(editor), Some((_, text))) = (&self.unified_editor, &self.unified_text) {
                let line = text.line_of_new(top.floor() as u32);
                editor.update(cx, |editor, _| {
                    editor.scroll.y = line as f32 * lh;
                    editor.autoscroll = None;
                });
                window.focus(&editor.focus_handle(cx));
            }
        } else {
            let line = match (&self.unified_editor, &self.unified_text, &self.model) {
                (Some(editor), Some((_, text)), Some(model)) => {
                    let top = (editor.read(cx).scroll.y / lh).floor() as usize;
                    Some(text.new_line_of(top, &model.hunks))
                }
                _ => None,
            };
            if let Some(working) = &self.working {
                if let Some(line) = line {
                    working.update(cx, |editor, _| {
                        editor.scroll.y = line as f32 * lh;
                        editor.autoscroll = None;
                    });
                }
                window.focus(&working.focus_handle(cx));
            }
            self.synced = None;
            self.driver = Side::Right;
        }
        cx.notify();
    }

    /// The unified text's editor follows the latest model.
    fn ensure_unified(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(model) = self.model.clone() else {
            return;
        };
        if self
            .unified_text
            .as_ref()
            .is_some_and(|(built, _)| Rc::ptr_eq(built, &model))
        {
            return;
        }
        let unified = Rc::new(build_unified(&model.base, &model.text, &model.hunks));
        match &self.unified_editor {
            Some(editor) => replace_text(editor, &unified.text, cx),
            None => {
                let path = self.path.clone();
                let text = unified.text.clone();
                self.unified_editor = Some(cx.new(|cx| {
                    let mut editor = Editor::new(Document::from_text(&text), window, cx);
                    editor.set_highlight_path(&path, cx);
                    editor.read_only = true;
                    // No gutter of its own: the viewer draws both versions' line numbers.
                    editor.message = Some(SharedString::default());
                    editor
                }));
            }
        }
        self.unified_text = Some((model, unified));
    }

    // --- Blocks: revert, commit checkboxes ---

    /// The arrow of a block: its HEAD lines replace its working copy lines (one undo step).
    fn revert(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.can_revert() {
            return;
        }
        let (Some(model), Some(working)) = (self.fresh_model(), self.working.clone()) else {
            return;
        };
        let Some(hunk) = model.hunks.get(index).cloned() else {
            return;
        };
        let old = block(&model.base, &line_ranges(&model.base), &hunk.old).to_string();
        working.update(cx, |editor, cx| {
            let text = editor.document.text();
            // The model must be of this text: a block of a stale diff would land elsewhere.
            if *normalize(&text.to_string()) != *model.text {
                return;
            }
            let start = char_of_line(text, hunk.new.start as usize);
            let end = char_of_line(text, hunk.new.end as usize);
            let old = match editor.document.line_ending() {
                "\r\n" => old.replace('\n', "\r\n"),
                _ => old,
            };
            editor.replace_ranges(vec![(start..end, old)], cx);
        });
    }

    /// ⌃⌘→: reverts the block under the cursor of the focused side (side by side).
    fn revert_at_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_revert() {
            return;
        }
        let Some(model) = self.fresh_model() else {
            return;
        };
        if self.unified {
            return;
        }
        let left = self
            .base_editor
            .as_ref()
            .filter(|editor| editor.focus_handle(cx).is_focused(window));
        let (line, from_old) = match (left, &self.working) {
            (Some(base), _) => (cursor_line(base, cx) as u32, true),
            (None, Some(working)) => (cursor_line(working, cx) as u32, false),
            (None, None) => return,
        };
        let index = model.hunks.iter().position(|hunk| {
            let lines = if from_old { &hunk.old } else { &hunk.new };
            lines.contains(&line) || (lines.is_empty() && lines.start == line)
        });
        if let Some(index) = index {
            self.revert(index, cx);
        }
    }

    /// Whether the blocks get commit checkboxes: a changed tracked file (a new or a deleted one is
    /// committed whole).
    fn has_checkboxes(&self, cx: &App) -> bool {
        self.compare.is_none()
            && self.proposal.is_none()
            && self.working.is_some()
            && !self.new_file
            && matches!(
                self.git.read(cx).status_of(&self.key),
                Some(FileStatus::Modified | FileStatus::Renamed | FileStatus::TypeChanged)
            )
    }

    fn toggle_hunk(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(model) = self.model.clone() else {
            return;
        };
        let Some(hunk) = model.hunks.get(index) else {
            return;
        };
        let key = self.key.clone();
        self.git.update(cx, |git, cx| {
            let included = git.is_hunk_included(&key, hunk);
            git.set_hunk_included(&key, hunk, !included, &model.hunks, cx)
        });
    }

    // --- Scrolling together ---

    /// Side by side: the side that moved alone since the last frame leads, the other one follows,
    /// line by line through the blocks; horizontally, both scroll the same.
    fn sync_scroll(&mut self, cx: &mut Context<Self>) {
        let (Some(left), Some(right), Some(model)) = (
            self.base_editor.clone(),
            self.working.clone(),
            self.model.clone(),
        ) else {
            return;
        };
        let l = left.read(cx).scroll;
        let r = right.read(cx).scroll;
        let (left_moved, right_moved) = match self.synced {
            Some((last_l, last_r)) => (l.y != last_l.y, r.y != last_r.y),
            // The first frame: the HEAD side comes to where the working copy is.
            None => (false, true),
        };
        if left_moved && !right_moved {
            self.driver = Side::Left;
        } else if right_moved && !left_moved {
            self.driver = Side::Right;
        }
        let (x_left, x_right) = match self.synced {
            Some((last_l, last_r)) if l.x != last_l.x && r.x == last_r.x => (l.x, l.x),
            Some((last_l, last_r)) if r.x != last_r.x && l.x == last_l.x => (r.x, r.x),
            _ => (l.x, r.x),
        };
        if left_moved || right_moved {
            let (left_max, right_max) = (max_scroll(&left, cx), max_scroll(&right, cx));
            // Both sides are as tall; the leader's height as of the last frame.
            let height = |editor: &Entity<Editor>| {
                editor
                    .read(cx)
                    .layout
                    .as_ref()
                    .map_or(0., |layout| f32::from(layout.text_bounds.size.height))
            };
            match self.driver {
                Side::Left => {
                    let y = follow(&model.hunks, l.y, left_max, right_max, height(&left), true);
                    set_scroll(&right, point(x_right, y), cx);
                }
                Side::Right => {
                    let y = follow(
                        &model.hunks,
                        r.y,
                        right_max,
                        left_max,
                        height(&right),
                        false,
                    );
                    set_scroll(&left, point(x_left, y), cx);
                }
            }
        } else if x_left != l.x || x_right != r.x {
            set_scroll(&left, point(x_left, l.y), cx);
            set_scroll(&right, point(x_right, r.y), cx);
        }
        self.synced = Some((left.read(cx).scroll, right.read(cx).scroll));
    }

    // --- Colors ---

    /// Hands both sides their colors for this frame.
    fn decorate_sides(&self, cx: &mut Context<Self>) {
        let Some(model) = &self.model else {
            return;
        };
        let ui = Theme::ui(cx);
        let mut left = Decorations::default();
        let mut right = Decorations::default();
        let left_text = self
            .base_editor
            .as_ref()
            .map(|editor| editor.read(cx).document.text().clone());
        let right_text = self
            .working
            .as_ref()
            .map(|editor| editor.read(cx).document.text().clone());
        for (hunk, words) in model.hunks.iter().zip(&model.words) {
            let old = hunk.old.start as usize..hunk.old.end as usize;
            let new = hunk.new.start as usize..hunk.new.end as usize;
            match hunk.kind() {
                HunkKind::Added => {
                    right.lines.push((new, ui.diff_added_bg));
                    left.boundaries.push((old.start, ui.diff_added));
                }
                HunkKind::Deleted => {
                    left.lines.push((old, ui.diff_deleted_bg));
                    right.boundaries.push((new.start, ui.diff_deleted));
                }
                HunkKind::Modified => {
                    left.lines.push((old, ui.diff_modified_bg));
                    right.lines.push((new, ui.diff_modified_bg));
                    if let Some(text) = &left_text {
                        left.words.extend(word_ranges(
                            text,
                            hunk.old.start,
                            &words.old,
                            ui.diff_modified_word,
                        ));
                    }
                    if let Some(text) = &right_text {
                        right.words.extend(word_ranges(
                            text,
                            hunk.new.start,
                            &words.new,
                            ui.diff_modified_word,
                        ));
                    }
                }
            }
        }
        if let Some(editor) = &self.base_editor {
            editor.update(cx, |editor, _| {
                editor.frame_decorations = Some(Rc::new(left))
            });
        }
        if let Some(editor) = &self.working {
            editor.update(cx, |editor, _| {
                editor.frame_decorations = Some(Rc::new(right))
            });
        }
    }

    /// Hands the unified editor its colors: HEAD lines gray, new lines green, changed words stronger.
    fn decorate_unified(&self, cx: &mut Context<Self>) {
        let (Some(editor), Some((model, unified))) = (&self.unified_editor, &self.unified_text)
        else {
            return;
        };
        let ui = Theme::ui(cx);
        let text = editor.read(cx).document.text().clone();
        let mut decorations = Decorations::default();
        let mut run: Option<(usize, Hsla)> = None;
        let flush = |run: &mut Option<(usize, Hsla)>, end: usize, decorations: &mut Decorations| {
            if let Some((start, color)) = run.take() {
                decorations.lines.push((start..end, color));
            }
        };
        for (index, line) in unified.lines.iter().enumerate() {
            let color = match line {
                UnifiedLine::Removed { .. } => Some(ui.diff_deleted_bg),
                UnifiedLine::Added { .. } => Some(ui.diff_added_bg),
                UnifiedLine::Context { .. } => None,
            };
            match (color, run) {
                (Some(color), Some((_, current))) if current == color => {}
                (color, _) => {
                    flush(&mut run, index, &mut decorations);
                    run = color.map(|color| (index, color));
                }
            }
        }
        flush(&mut run, unified.lines.len(), &mut decorations);
        for ((hunk, words), start) in model
            .hunks
            .iter()
            .zip(&model.words)
            .zip(&unified.hunk_starts)
        {
            let old_start = *start as u32;
            let new_start = old_start + hunk.old.len() as u32;
            decorations.words.extend(word_ranges(
                &text,
                old_start,
                &words.old,
                ui.diff_deleted_word,
            ));
            decorations.words.extend(word_ranges(
                &text,
                new_start,
                &words.new,
                ui.diff_added_word,
            ));
        }
        editor.update(cx, |editor, _| {
            editor.frame_decorations = Some(Rc::new(decorations))
        });
    }

    // --- Rendering ---

    /// The banner of a Claude proposal: what it is, Reject, Accept (⌘↵).
    fn render_proposal_banner(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let proposal = self.proposal.as_ref()?;
        let ui = Theme::ui(cx);
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let changed = self
            .working
            .as_ref()
            .is_some_and(|right| *right.read(cx).document.text() != proposal.proposed);
        let accept_keys = ui::shortcut_in(&AcceptProposal, &self.focus_handle, window);
        Some(
            div()
                .flex_none()
                .mx(px(ui::GAP))
                .mt(px(ui::GAP))
                .px(px(10.))
                .py(px(6.))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(px(ui::RADIUS_MD))
                .bg(UiColors::tint(ui.accent, 0.12))
                .border_1()
                .border_color(UiColors::tint(ui.accent, 0.35))
                .child(icon(IconName::Claude, ui.accent_text).size(px(15.)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(trf("Claude proposes changes to {0}", &[&name])),
                        )
                        .child(
                            div()
                                .text_size(px(theme::TEXT_SM))
                                .text_color(ui.text_muted)
                                .child(if changed {
                                    tr("Accept applies your version of the right side")
                                } else {
                                    tr("Edit the right side or reject changes with the arrows")
                                }),
                        ),
                )
                .child(
                    ui::text_button("proposal-reject", tr("Reject"), true, ui)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.reject_proposal(cx))),
                )
                .child(
                    ui::primary_button("proposal-accept", tr("Accept"), true, ui)
                        .when_some(accept_keys, |button, keys| {
                            button.tooltip(ui::tooltip(tr("Accept"), Some(keys)))
                        })
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.accept_proposal(cx))),
                ),
        )
    }

    fn render_toolbar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let keys = |action: &dyn gpui::Action| ui::shortcut_in(action, &self.focus_handle, window);
        let count = self
            .model
            .as_ref()
            .filter(|_| self.comparable())
            .map(|model| model.hunks.len());
        let counter = match count {
            Some(0) => tr("No differences").to_string(),
            Some(count) => trf("Differences: {0}", &[&count]),
            None => String::new(),
        };
        let git = self.git.read(cx);
        let path = git
            .repo_for(&self.key)
            .and_then(|repo| repo.repo.relative(&self.key))
            .unwrap_or_else(|| self.title().to_string());
        let comparable = self.comparable();
        let unified = self.unified;
        div()
            .flex_none()
            .h(px(TOOLBAR_HEIGHT))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .child(
                ui::icon_button("diff-previous", IconName::ArrowUp, ui)
                    .tooltip(ui::tooltip(
                        tr("Previous Difference"),
                        keys(&PreviousDifference),
                    ))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.go_to_difference(false, window, cx)
                    })),
            )
            .child(
                ui::icon_button("diff-next", IconName::ArrowDown, ui)
                    .tooltip(ui::tooltip(tr("Next Difference"), keys(&NextDifference)))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.go_to_difference(true, window, cx)
                    })),
            )
            .child(
                div()
                    .ml_2()
                    .flex_none()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.text_muted)
                    .child(counter),
            )
            .child(div().flex_1())
            .child(
                div()
                    .min_w_0()
                    .mr_2()
                    .truncate()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(path),
            )
            .when(comparable, |bar| {
                bar.child(
                    ui::toggle_button("diff-side-by-side", IconName::SplitRight, !unified, ui)
                        .tooltip(ui::tooltip(tr("Side by Side"), None))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.set_unified(false, window, cx)
                        })),
                )
                .child(
                    ui::toggle_button("diff-unified", IconName::Unified, unified, ui)
                        .tooltip(ui::tooltip(tr("Unified"), None))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.set_unified(true, window, cx)
                        })),
                )
            })
            .when(self.can_jump(), |bar| {
                bar.child(
                    ui::icon_button("diff-jump", IconName::Pencil, ui)
                        .tooltip(ui::tooltip(tr("Jump to Source"), keys(&JumpToSource)))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.jump_to_source(window, cx)
                        })),
                )
            })
    }

    /// The captions over the sides: "HEAD · 4d7de13" and "Working copy" — or a comparison's labels.
    fn render_captions(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let (head, working) = match &self.compare {
            Some(compare) => (
                Self::side_caption(&compare.left),
                Self::side_caption(&compare.right),
            ),
            None if self.proposal.is_some() => (
                tr("Current version").to_string(),
                tr("Proposed by Claude").to_string(),
            ),
            None => {
                let head = self
                    .git
                    .read(cx)
                    .repo_for(&self.key)
                    .and_then(|repo| repo.status.branch.oid.clone())
                    .map(|oid| trf("HEAD · {0}", &[&oid.chars().take(7).collect::<String>()]))
                    .unwrap_or_else(|| tr("HEAD").to_string());
                let working = match &self.working {
                    Some(_) => tr("Working copy"),
                    None => tr("Deleted"),
                };
                (head, working.to_string())
            }
        };
        let caption = |text: String| {
            div()
                .flex_1()
                .min_w_0()
                .px_3()
                .truncate()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.dim)
                .child(text)
        };
        let row = div()
            .flex_none()
            .h(px(CAPTION_HEIGHT))
            .flex()
            .items_center();
        if self.unified {
            row.child(caption(format!("{head} → {working}")))
        } else {
            row.child(caption(head))
                .child(div().flex_none().w(px(DIVIDER_WIDTH)))
                .child(caption(working))
        }
    }

    fn render_side_by_side(&self, cx: &Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let left = match &self.base_editor {
            Some(editor) => editor.clone().into_any_element(),
            None if self.new_file => {
                let text = match &self.compare {
                    Some(compare) => missing_note(&compare.left),
                    None => tr("The file is new").to_string(),
                };
                note(IconName::Info, &text, ui).into_any_element()
            }
            None => div().into_any_element(),
        };
        let right = match &self.working {
            Some(editor) => editor.clone().into_any_element(),
            None => {
                let text = match &self.compare {
                    Some(compare) => missing_note(&compare.right),
                    None => tr("The file is deleted").to_string(),
                };
                note(IconName::Info, &text, ui).into_any_element()
            }
        };
        let pane = |side: Side| {
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .on_scroll_wheel(cx.listener(move |this, _: &ScrollWheelEvent, _, _| {
                    this.driver = side;
                }))
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_row()
            .child(pane(Side::Left).child(left))
            .child(self.render_divider(cx))
            .child(pane(Side::Right).child(right))
            .into_any_element()
    }

    /// The strip between the sides: a band from each block on the left to its block on the right,
    /// and on it the block's revert arrow and commit checkbox.
    fn render_divider(&self, cx: &Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let strip = div()
            .relative()
            .flex_none()
            .w(px(DIVIDER_WIDTH))
            .h_full()
            .overflow_hidden();
        let (Some(model), Some(left), Some(right)) = (
            self.model.clone(),
            self.base_editor.clone(),
            self.working.clone(),
        ) else {
            return strip
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(DIVIDER_WIDTH / 2.))
                        .w(px(1.))
                        .bg(ui.divider),
                )
                .into_any_element();
        };
        let bands = canvas(|_, _, _| (), {
            let model = model.clone();
            let (left, right) = (left.clone(), right.clone());
            move |bounds, (), window, cx| {
                // The final offsets of this frame: both editors are laid out by now.
                let ly = left.read(cx).scroll.y;
                let ry = right.read(cx).scroll.y;
                for hunk in &model.hunks {
                    let geometry = BandGeometry::new(hunk, ly, ry);
                    if !geometry.visible(f32::from(bounds.size.height)) {
                        continue;
                    }
                    let color = match hunk.kind() {
                        HunkKind::Added => ui.diff_added_bg,
                        HunkKind::Deleted => ui.diff_deleted_bg,
                        HunkKind::Modified => ui.diff_modified_bg,
                    };
                    paint_band(bounds, &geometry, color, window);
                }
            }
        })
        .absolute()
        .size_full();
        let ly = left.read(cx).scroll.y;
        let ry = right.read(cx).scroll.y;
        let checkboxes = self.has_checkboxes(cx);
        let arrows = self.can_revert();
        let git = self.git.read(cx);
        let mut buttons = Vec::new();
        for (index, hunk) in model.hunks.iter().enumerate() {
            let geometry = BandGeometry::new(hunk, ly, ry);
            // The buttons are laid out before the editors settle: a frame late while scrolling.
            if geometry.left_top.max(geometry.right_top) < -100.
                || geometry.left_top.min(geometry.right_top) > 4000.
            {
                continue;
            }
            let arrow_top = geometry.button_top(true);
            if arrows {
                buttons.push(
                    div()
                        .id(("diff-revert", index))
                        .absolute()
                        .top(px(arrow_top))
                        .left(px(2.))
                        .size(px(DIVIDER_BUTTON))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(ui::RADIUS_XS))
                        .cursor_pointer()
                        .hover(move |style| style.bg(ui.hover))
                        .tooltip(ui::tooltip(
                            if self.proposal.is_some() {
                                tr("Reject This Change")
                            } else {
                                tr("Revert")
                            },
                            None,
                        ))
                        .on_click(
                            cx.listener(move |this, _: &ClickEvent, _, cx| this.revert(index, cx)),
                        )
                        .child(icon(IconName::ArrowRight, ui.text_muted).size(px(11.)))
                        .into_any_element(),
                );
            }
            if checkboxes {
                let included = git.is_hunk_included(&self.key, hunk);
                buttons.push(
                    div()
                        .id(("diff-include", index))
                        .absolute()
                        .top(px(geometry.button_top(false)))
                        .right(px(3.))
                        .size(px(DIVIDER_BUTTON))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .tooltip(ui::tooltip(tr("Include in Commit"), None))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.toggle_hunk(index, cx)
                        }))
                        .child(ui::checkbox(
                            ("hunk-check", index),
                            ui::CheckState::from_bool(included),
                            ui,
                        ))
                        .into_any_element(),
                );
            }
        }
        strip.child(bands).children(buttons).into_any_element()
    }

    fn render_unified(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.ensure_unified(window, cx);
        self.decorate_unified(cx);
        let (Some(editor), Some((_, unified))) =
            (self.unified_editor.clone(), self.unified_text.clone())
        else {
            return div().flex_1().into_any_element();
        };
        let ui = Theme::ui(cx);
        let digits = unified
            .lines
            .iter()
            .map(|line| match line {
                UnifiedLine::Context { old, new } => (*old).max(*new),
                UnifiedLine::Removed { old, .. } => *old,
                UnifiedLine::Added { new, .. } => *new,
            })
            .max()
            .map_or(1, |max| (max + 1).to_string().len())
            .max(2);
        let gutter = unified_gutter(editor.clone(), unified.clone(), digits, ui);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_row()
            .child(gutter)
            .child(div().flex_1().min_w_0().h_full().child(editor))
            .into_any_element()
    }
}

/// Where a block is in the strip between the sides: the tops and bottoms of its lines on each side,
/// relative to the top of the text.
struct BandGeometry {
    left_top: f32,
    left_bottom: f32,
    right_top: f32,
    right_bottom: f32,
}

impl BandGeometry {
    fn new(hunk: &Hunk, left_scroll: f32, right_scroll: f32) -> Self {
        let lh = theme::LINE_HEIGHT;
        Self {
            left_top: hunk.old.start as f32 * lh - left_scroll,
            left_bottom: hunk.old.end as f32 * lh - left_scroll,
            right_top: hunk.new.start as f32 * lh - right_scroll,
            right_bottom: hunk.new.end as f32 * lh - right_scroll,
        }
    }

    fn visible(&self, height: f32) -> bool {
        self.left_bottom.max(self.right_bottom) >= 0. && self.left_top.min(self.right_top) <= height
    }

    /// The top of a button: at the block's first line on that side, or centered on its boundary
    /// when the side has no lines of the block.
    fn button_top(&self, left: bool) -> f32 {
        let (top, bottom) = if left {
            (self.left_top, self.left_bottom)
        } else {
            (self.right_top, self.right_bottom)
        };
        let lh = theme::LINE_HEIGHT;
        if bottom > top {
            top + (lh - DIVIDER_BUTTON) / 2.
        } else {
            top - DIVIDER_BUTTON / 2.
        }
    }
}

/// A band from a block on the left to its block on the right: S-curves along the top and the
/// bottom, as the connectors of JetBrains' diff.
fn paint_band(bounds: Bounds<Pixels>, band: &BandGeometry, color: Hsla, window: &mut Window) {
    let x0 = bounds.left();
    let x1 = bounds.right();
    let xm = (x0 + x1) / 2.;
    let y = |offset: f32| bounds.top() + px(offset);
    let (lt, lb, rt, rb) = (
        y(band.left_top),
        y(band.left_bottom),
        y(band.right_top),
        y(band.right_bottom),
    );
    // An empty side still gets a sliver, so the band shows where the lines went.
    let (lb, rb) = (lb.max(lt + px(1.)), rb.max(rt + px(1.)));
    let mut path = PathBuilder::fill();
    path.move_to(point(x0, lt));
    path.curve_to(point(xm, (lt + rt) / 2.), point(xm, lt));
    path.curve_to(point(x1, rt), point(xm, rt));
    path.line_to(point(x1, rb));
    path.curve_to(point(xm, (lb + rb) / 2.), point(xm, rb));
    path.curve_to(point(x0, lb), point(xm, lb));
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// The unified view's gutter: the HEAD and the working copy line numbers, and a − or + mark,
/// following the editor's scroll.
fn unified_gutter(
    editor: Entity<Editor>,
    unified: Rc<Unified>,
    digits: usize,
    ui: UiColors,
) -> impl IntoElement {
    let column = digits as f32 * 8.6 + 10.;
    let width = column * 2. + 14.;
    canvas(
        |_, _, _| (),
        move |bounds, (), window, cx| {
            let scroll = editor.read(cx).scroll.y;
            let lh = theme::LINE_HEIGHT;
            let font_size = px(theme::FONT_SIZE - 1.);
            let first = (scroll / lh).floor().max(0.) as usize;
            let visible = (f32::from(bounds.size.height) / lh).ceil() as usize + 1;
            let text_system = window.text_system().clone();
            let run = |len: usize, color: Hsla| TextRun {
                len,
                font: font(theme::code_font()),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                for (index, line) in unified.lines.iter().enumerate().skip(first).take(visible) {
                    let top = bounds.top() + px(index as f32 * lh - scroll);
                    let (old, new, mark, color) = match line {
                        UnifiedLine::Context { old, new } => (Some(*old), Some(*new), "", ui.dim),
                        UnifiedLine::Removed { old, .. } => {
                            (Some(*old), None, "−", ui.diff_deleted)
                        }
                        UnifiedLine::Added { new, .. } => (None, Some(*new), "+", ui.diff_added),
                    };
                    let mut paint_text = |text: String, right: f32, color: Hsla| {
                        let shaped = text_system.shape_line(
                            text.clone().into(),
                            font_size,
                            &[run(text.len(), color)],
                            None,
                        );
                        let x = bounds.left() + px(right) - shaped.width;
                        shaped.paint(point(x, top), px(lh), window, cx).ok();
                    };
                    if let Some(old) = old {
                        paint_text((old + 1).to_string(), column - 4., ui.dim);
                    }
                    if let Some(new) = new {
                        paint_text((new + 1).to_string(), column * 2. - 4., ui.dim);
                    }
                    if !mark.is_empty() {
                        paint_text(mark.to_string(), width - 3., color);
                    }
                }
            });
        },
    )
    .flex_none()
    .w(px(width))
    .h_full()
}

impl Focusable for DiffView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.unified
            && let Some(editor) = &self.unified_editor
        {
            return editor.focus_handle(cx);
        }
        match (&self.working, &self.base_editor) {
            (Some(editor), _) | (None, Some(editor)) => editor.focus_handle(cx),
            (None, None) => self.focus_handle.clone(),
        }
    }
}

impl Render for DiffView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let right_unreadable = self
            .compare
            .as_ref()
            .is_some_and(|compare| compare.right_state == RightState::Unreadable);
        let note_text = match (&self.base, self.binary) {
            (_, true) => Some(tr("Binary files differ")),
            (Base::Unreadable, _) => {
                Some(tr("Contents can't be compared: a binary or too large file"))
            }
            _ if right_unreadable => {
                Some(tr("Contents can't be compared: a binary or too large file"))
            }
            (Base::NoRepository, _) => Some(tr("Not in a Git repository")),
            (Base::Loading | Base::Text(_), false) => None,
        };
        let body = match note_text {
            Some(text) => note(IconName::Info, text, ui).into_any_element(),
            None if matches!(self.base, Base::Loading) || self.right_loading() => {
                div().flex_1().into_any_element()
            }
            None if self.unified => self.render_unified(window, cx),
            None => {
                self.sync_scroll(cx);
                self.decorate_sides(cx);
                self.render_side_by_side(cx)
            }
        };
        let comparable = self.comparable();
        div()
            .key_context("DiffView")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .on_action(cx.listener(|this, _: &NextDifference, window, cx| {
                this.go_to_difference(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &PreviousDifference, window, cx| {
                this.go_to_difference(false, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &RevertChange, window, cx| this.revert_at_cursor(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleUnified, window, cx| {
                this.set_unified(!this.unified, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &JumpToSource, window, cx| this.jump_to_source(window, cx)),
            )
            .when(self.proposal.is_some(), |view| {
                view.on_action(cx.listener(|this, _: &AcceptProposal, _, cx| this.accept_proposal(cx)))
                    .on_action(cx.listener(|this, _: &RejectProposal, _, cx| this.reject_proposal(cx)))
                    // The proposal isn't a file of its own: ⌘S must not ask where to save it.
                    .capture_action(cx.listener(|_, _: &editor::Save, _, cx| cx.stop_propagation()))
            })
            .children(self.render_proposal_banner(window, cx))
            .child(self.render_toolbar(window, cx))
            .child(ui::divider(ui).mx(px(ui::GAP)))
            .when(comparable, |view| view.child(self.render_captions(cx)))
            .child(body)
    }
}

// --- Helpers ---

/// A centered note in place of a side or the whole view.
fn note(icon_name: IconName, text: &str, ui: UiColors) -> gpui::Div {
    div()
        .flex_1()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .text_color(ui.dim)
        .child(icon(icon_name, ui.dim).size(px(14.)))
        .child(text.to_string())
}

/// The note in place of a side whose file isn't there: "The file doesn't exist in main".
fn missing_note(side: &DiffSide) -> String {
    match side {
        DiffSide::Revision { label, .. } => trf("The file doesn't exist in {0}", &[label]),
        DiffSide::WorkingCopy => tr("The file doesn't exist in the working tree").to_string(),
    }
}

/// A side for the tab's title: its label, at most `max_chars` long.
/// `<stash>^1` against `<stash>` (or its untracked files, `<stash>^3`).
fn is_stash_pair(left: &DiffSide, right: &DiffSide) -> bool {
    let (DiffSide::Revision { rev: base, .. }, DiffSide::Revision { rev, .. }) = (left, right)
    else {
        return false;
    };
    let Some(stash) = base.strip_suffix("^1") else {
        return false;
    };
    rev == stash || rev.strip_suffix("^3") == Some(stash)
}

fn short_label(side: &DiffSide, max_chars: usize) -> String {
    let label = match side {
        DiffSide::Revision { label, .. } => label.as_str(),
        DiffSide::WorkingCopy => tr("Working copy"),
    };
    if label.chars().count() <= max_chars {
        return label.to_string();
    }
    let mut short: String = label.chars().take(max_chars - 1).collect();
    short.push('…');
    short
}

/// Character ranges of a block's changed words in a document, the block starting at line `first`.
fn word_ranges(
    text: &Rope,
    first: u32,
    spans: &[(u32, Range<usize>)],
    color: Hsla,
) -> Vec<(Range<usize>, Hsla)> {
    let lines = text.len_lines();
    spans
        .iter()
        .filter_map(|(line, columns)| {
            let line = (first + line) as usize;
            if line >= lines {
                return None;
            }
            let start = line_start(text, line);
            let len = line_len(text, line);
            let range = start + columns.start.min(len)..start + columns.end.min(len);
            (!range.is_empty()).then_some((range, color))
        })
        .collect()
}

/// The first character of a line; past the last line, the end of the text.
fn char_of_line(text: &Rope, line: usize) -> usize {
    if line >= text.len_lines() {
        text.len_chars()
    } else {
        text.line_to_char(line)
    }
}

fn cursor_line(editor: &Entity<Editor>, cx: &App) -> usize {
    let editor = editor.read(cx);
    let text = editor.document.text();
    text.char_to_line(editor.document.selection().primary().head)
}

/// Puts the cursor at the start of a line and centers it, if it is out of view.
fn move_cursor(editor: &Entity<Editor>, line: usize, cx: &mut App) {
    editor.update(cx, |editor, cx| {
        let position = editor.position(line, 0);
        editor.select_range(position..position, cx);
    });
}

/// The furthest an editor scrolls: until its last line is at the top (as its element clamps).
fn max_scroll(editor: &Entity<Editor>, cx: &App) -> f32 {
    let lines = editor.read(cx).document.text().len_lines();
    lines.saturating_sub(1) as f32 * theme::LINE_HEIGHT
}

/// Scrolls a follower: no autoscroll of its own this frame, the offset clamped as the editor would.
fn set_scroll(editor: &Entity<Editor>, scroll: Point<f32>, cx: &mut App) {
    let max = max_scroll(editor, cx);
    editor.update(cx, |editor, cx| {
        let scroll = point(scroll.x.max(0.), scroll.y.clamp(0., max));
        if editor.scroll != scroll {
            editor.scroll = scroll;
            editor.autoscroll = None;
            cx.notify();
        }
    });
}

/// Replaces a read-only editor's whole text, keeping its scroll.
fn replace_text(editor: &Entity<Editor>, text: &str, cx: &mut App) {
    editor.update(cx, |editor, cx| {
        let scroll = editor.scroll;
        let read_only = editor.read_only;
        editor.read_only = false;
        let end = editor.document.text().len_chars();
        editor.replace_ranges(vec![(0..end, text.to_string())], cx);
        editor.read_only = read_only;
        editor.scroll = scroll;
        editor.autoscroll = None;
    });
}

/// The path as the git hub knows it: canonical, or, for a deleted file, its canonical directory and
/// its name.
fn canonical(path: &Path) -> PathBuf {
    if let Ok(path) = std::fs::canonicalize(path) {
        return path;
    }
    match (
        path.parent()
            .and_then(|dir| std::fs::canonicalize(dir).ok()),
        path.file_name(),
    ) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(old: Range<u32>, new: Range<u32>) -> Hunk {
        Hunk { old, new }
    }

    #[test]
    fn lines_map_through_the_blocks() {
        // Line 1 modified into two lines, line 4 deleted, two lines added after line 6.
        let hunks = vec![hunk(1..2, 1..3), hunk(4..5, 5..5), hunk(7..7, 7..9)];
        // Before the first block: the same line.
        assert_eq!(map_line(&hunks, 0.5, true), 0.5);
        // Inside a block: the same share of the other block.
        assert_eq!(map_line(&hunks, 1.5, true), 2.);
        assert_eq!(map_line(&hunks, 2.0, false), 1.5);
        // Between blocks: shifted by what the blocks above added.
        assert_eq!(map_line(&hunks, 3., true), 4.);
        assert_eq!(map_line(&hunks, 4., false), 3.);
        // A deleted line maps to where it was; the line after it, past it.
        assert_eq!(map_line(&hunks, 4.5, true), 5.);
        assert_eq!(map_line(&hunks, 5., true), 5.);
        // Added lines map to the place they were inserted at.
        assert_eq!(map_line(&hunks, 8., false), 7.);
        assert_eq!(map_line(&hunks, 7., true), 9.);
        assert_eq!(map_line(&hunks, 10., true), 12.);
        assert_eq!(map_line(&[], 3.25, false), 3.25);
    }

    #[test]
    fn the_follower_keeps_the_middle_and_the_edges() {
        let lh = theme::LINE_HEIGHT;
        // The whole file deleted: at the top, both sides are at the top.
        let deleted = vec![hunk(0..1, 0..0)];
        assert_eq!(follow(&deleted, 0., 0., lh, 10. * lh, false), 0.);
        // Ten lines inserted at line 10; a view ten lines tall.
        let inserted = vec![hunk(10..10, 10..20)];
        let (height, left_max, right_max) = (10. * lh, 99. * lh, 109. * lh);
        // Left line 10 in the middle: right line 20 in the middle.
        let y = follow(&inserted, 5. * lh, left_max, right_max, height, true);
        assert_eq!(y, 15. * lh);
        // Back the other way.
        assert_eq!(
            follow(&inserted, 15. * lh, right_max, left_max, height, false),
            5. * lh
        );
        // The bottom takes the follower to its bottom.
        assert_eq!(
            follow(&inserted, left_max, left_max, right_max, height, true),
            right_max
        );
    }

    #[test]
    fn changed_words_become_columns_of_lines() {
        let block = "let ab = 1;\nfoo(ab)\n";
        // "ab" in the first line, "foo(ab" spanning to the second, and the break between them.
        let spans = line_spans(block, &[4..6, 11..18]);
        assert_eq!(spans, vec![(0, 4..6), (1, 0..6)]);
        // Columns count characters, not bytes.
        let spans = line_spans("привет мир\n", std::slice::from_ref(&(13..19)));
        assert_eq!(spans, vec![(0, 7..10)]);
    }

    #[test]
    fn the_unified_text_shows_old_lines_then_new() {
        let base = "a\nb\nc\n";
        let text = "a\nB\nc\nd\n";
        let hunks = flux_git::diff_lines(base, text);
        assert_eq!(hunks, vec![hunk(1..2, 1..2), hunk(3..3, 3..4)]);
        let unified = build_unified(base, text, &hunks);
        assert_eq!(unified.text, "a\nb\nB\nc\nd");
        assert_eq!(
            unified.lines,
            vec![
                UnifiedLine::Context { old: 0, new: 0 },
                UnifiedLine::Removed { old: 1, hunk: 0 },
                UnifiedLine::Added { new: 1, hunk: 0 },
                UnifiedLine::Context { old: 2, new: 2 },
                UnifiedLine::Added { new: 3, hunk: 1 },
            ]
        );
        assert_eq!(unified.hunk_starts, vec![1, 4]);
        assert_eq!(unified.line_of_new(2), 3);
        assert_eq!(unified.new_line_of(1, &hunks), 1);
        assert_eq!(unified.new_line_of(4, &hunks), 3);
    }

    #[test]
    fn a_deleted_file_is_all_removed_lines() {
        let base = "x\ny";
        let hunks = flux_git::diff_lines(base, "");
        let unified = build_unified(base, "", &hunks);
        assert_eq!(unified.text, "x\ny");
        assert_eq!(unified.hunk_starts, vec![0]);
        assert!(
            unified
                .lines
                .iter()
                .all(|line| matches!(line, UnifiedLine::Removed { .. }))
        );
    }

    #[test]
    fn modified_blocks_get_changed_words() {
        let model = compute_model("a\nlet x = 1;\nz\n".into(), "a\nlet y = 1;\nz\n".into());
        assert_eq!(model.hunks, vec![hunk(1..2, 1..2)]);
        assert_eq!(
            model.words,
            vec![BlockWords {
                old: vec![(0, 4..5)],
                new: vec![(0, 4..5)],
            }]
        );
        // Line breaks don't matter: the working copy is compared without `\r`.
        assert_eq!(normalize("a\r\nb\r\n"), "a\nb\n");
    }

    #[test]
    fn word_ranges_are_clamped_to_their_lines() {
        let text = Rope::from_str("one\r\ntwo three\n");
        let ranges = word_ranges(
            &text,
            1,
            &[(0, 4..9), (0, 8..20), (5, 0..1)],
            Hsla::default(),
        );
        let ranges: Vec<Range<usize>> = ranges.into_iter().map(|(range, _)| range).collect();
        // Line 1 starts at character 5 (after "one\r\n").
        assert_eq!(ranges, vec![9..14, 13..14]);
        assert_eq!(char_of_line(&text, 1), 5);
        assert_eq!(char_of_line(&text, 9), text.len_chars());
    }
}
