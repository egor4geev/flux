//! The merge tool: a tab with a conflicted file's three versions, as the merge window of JetBrains
//! IDEs — ours on the left and theirs on the right (read-only), the result in the middle
//! (editable).
//!
//! - **Blocks.** Both sides are merged against the base (`flux_git::merge3`): a change of one side
//!   that doesn't touch the other's goes into the result from the start (it is shown resolved, in a
//!   quiet color); changes that overlap are a conflict (red), and the result keeps the base lines
//!   there. Each block follows its lines in the result while the user types (the editor records its
//!   edits for us: `Editor::recorded_changes`), so the blocks stay where they are.
//! - **Taking sides.** On the strips between the panes each conflict gets an arrow that takes its
//!   left or right side into the result and a cross that ignores that side; taking both puts the
//!   second after the first. A conflict is resolved when both sides are taken or ignored. Undo and
//!   redo in the result are recognized: a block whose text goes back to the base (or to a side) gets
//!   that state back.
//! - **Keys** (context "MergeView", as in JetBrains IDEs): F7 / ⇧F7 — the next / previous conflict,
//!   ⌃⌘→ / ⌃⌘← — resolve the conflict at the cursor with its left / right side, ⌘↵ — Apply, F4 — the
//!   file in its own tab. The three panes scroll together through the blocks; the one the user
//!   scrolls leads.
//! - **Finishing.** Apply writes the result (with the file's line endings) and marks the file
//!   resolved (`git add`); with conflicts left it asks first. Accept Left / Accept Right take a whole
//!   side. A file one side deleted, or a binary one, gets only those two. When the file stops being
//!   conflicted from outside (a terminal, the Conflicts dialog), the tab closes.
//!
//! The editors draw the colors: before each frame the tool hands each of them its
//! [`Decorations`] (`Editor::frame_decorations`), as the diff viewer does.

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use flux_core::text::{line_len, line_start};
use flux_core::transaction::Operation as Op;
use flux_core::{Assoc, ChangeSet, Document, Rope, TextChange};
use flux_git::{Chunk, ChunkKind, ConflictKind, ConflictSide, ConflictVersions, RepoState};
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    Hsla, KeyBinding, PathBuilder, Pixels, Point, Render, ScrollWheelEvent, SharedString,
    Subscription, Task, Window, actions, canvas, div, point, prelude::*, px,
};

use crate::dialog::Dialog;
use crate::diff_view::Decorations;
use crate::editor::{self, Editor, EditorEvent};
use crate::git::GitStore;
use crate::i18n::{tr, trf, trn};
use crate::icons::{IconName, icon};
use crate::theme::{self, Theme, UiColors};
use crate::ui;
use crate::workspace::Location;

actions!(
    merge_tool,
    [
        /// F7: the next unresolved conflict.
        NextConflict,
        /// ⇧F7: the previous unresolved conflict.
        PreviousConflict,
        /// ⌃⌘→: the conflict at the cursor is resolved with its left side.
        AcceptLeftChange,
        /// ⌃⌘←: the conflict at the cursor is resolved with its right side.
        AcceptRightChange,
        /// ⌘↵: write the result and mark the file resolved.
        ApplyResult,
        /// F4: the file in its own tab.
        JumpToSource,
        /// The magic wand: conflicts whose sides changed different words are merged.
        ResolveSimpleConflicts,
    ]
);

pub fn init(cx: &mut App) {
    let context = Some("MergeView");
    cx.bind_keys([
        // As in the merge window of JetBrains IDEs.
        KeyBinding::new("f7", NextConflict, context),
        KeyBinding::new("shift-f7", PreviousConflict, context),
        // The same keys as "Accept Left Side" in the diff: the arrow points where the text goes.
        KeyBinding::new("cmd-ctrl-right", AcceptLeftChange, context),
        KeyBinding::new("cmd-ctrl-left", AcceptRightChange, context),
        KeyBinding::new("cmd-enter", ApplyResult, context),
        KeyBinding::new("f4", JumpToSource, context),
    ]);
}

/// The toolbar above the panes, the captions under it, the footer with the buttons.
const TOOLBAR_HEIGHT: f32 = 36.;
const CAPTION_HEIGHT: f32 = 24.;
const FOOTER_HEIGHT: f32 = 48.;
/// The strips between the panes: the bands, and on each conflict an arrow and a cross.
const STRIP_WIDTH: f32 = 44.;
const STRIP_BUTTON: f32 = 16.;
/// In a window narrower than this, the footer shows no key hints.
const HINTS_MIN_WINDOW: f32 = 1180.;
/// Changed words are looked for only in conflicts up to this many lines a side.
const WORD_DIFF_MAX_LINES: usize = 300;

// --- The model: blocks of the merge and what was taken from each side ---

/// A side of the merge: ours on the left, theirs on the right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    Left,
    Right,
}

impl Side {
    fn other(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }

    fn conflict_side(self) -> ConflictSide {
        match self {
            Side::Left => ConflictSide::Ours,
            Side::Right => ConflictSide::Theirs,
        }
    }
}

/// What happened to a side's change in a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Take {
    /// Not decided yet: a conflict's side shows its arrow and cross.
    Pending,
    /// In the result.
    Applied,
    /// Left out (or the side didn't change these lines).
    Ignored,
}

/// A changed region of the merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Block {
    pub chunk: Chunk,
    /// Its lines in the result, followed through edits.
    pub result: Range<u32>,
    pub left: Take,
    pub right: Take,
    /// The sides taken into the result, in order (the second goes after the first).
    pub order: Vec<Side>,
    /// Resolved by the magic wand: the merged text.
    pub merged: Option<String>,
}

impl Block {
    fn new(chunk: Chunk, result: Range<u32>) -> Self {
        // Non-conflicting changes are in the result from the start.
        let (left, right, order) = match chunk.kind {
            ChunkKind::Ours => (Take::Applied, Take::Ignored, vec![Side::Left]),
            ChunkKind::Theirs => (Take::Ignored, Take::Applied, vec![Side::Right]),
            ChunkKind::Both => (Take::Applied, Take::Applied, vec![Side::Left]),
            ChunkKind::Conflict => (Take::Pending, Take::Pending, Vec::new()),
        };
        Self {
            chunk,
            result,
            left,
            right,
            order,
            merged: None,
        }
    }

    pub fn is_conflict(&self) -> bool {
        self.chunk.kind == ChunkKind::Conflict
    }

    /// A conflict that still needs a decision.
    pub fn pending(&self) -> bool {
        self.is_conflict() && (self.left == Take::Pending || self.right == Take::Pending)
    }

    /// Whether the side changed these lines (a band joins them to the result).
    pub fn changed_on(&self, side: Side) -> bool {
        matches!(
            (side, self.chunk.kind),
            (
                Side::Left,
                ChunkKind::Ours | ChunkKind::Both | ChunkKind::Conflict
            ) | (
                Side::Right,
                ChunkKind::Theirs | ChunkKind::Both | ChunkKind::Conflict
            )
        )
    }

    pub fn take(&self, side: Side) -> Take {
        match side {
            Side::Left => self.left,
            Side::Right => self.right,
        }
    }

    fn set_take(&mut self, side: Side, take: Take) {
        match side {
            Side::Left => self.left = take,
            Side::Right => self.right = take,
        }
    }

    /// The block's lines on a side.
    pub fn lines(&self, side: Side) -> &Range<u32> {
        match side {
            Side::Left => &self.chunk.ours,
            Side::Right => &self.chunk.theirs,
        }
    }
}

/// The three texts (without `\r` before `\n`), their blocks, and the result as it was opened.
pub(crate) struct MergeModel {
    base: String,
    ours: String,
    theirs: String,
    base_lines: Vec<Range<usize>>,
    ours_lines: Vec<Range<usize>>,
    theirs_lines: Vec<Range<usize>>,
    pub blocks: Vec<Block>,
    /// The result as the tool opened it: every non-conflicting change applied.
    pub initial: String,
}

impl MergeModel {
    pub fn new(base: &str, ours: &str, theirs: &str) -> Self {
        let chunks = flux_git::merge3(base, ours, theirs);
        let (initial, ranges) = flux_git::initial_result(base, ours, theirs, &chunks);
        let blocks = chunks
            .into_iter()
            .zip(ranges)
            .map(|(chunk, range)| Block::new(chunk, range))
            .collect();
        Self {
            base_lines: line_ranges(base),
            ours_lines: line_ranges(ours),
            theirs_lines: line_ranges(theirs),
            base: base.to_string(),
            ours: ours.to_string(),
            theirs: theirs.to_string(),
            blocks,
            initial,
        }
    }

    /// The lines of a side's text.
    pub fn side_text(&self, block: &Block, side: Side) -> &str {
        match side {
            Side::Left => flux_git::merge3::lines(&self.ours, &self.ours_lines, &block.chunk.ours),
            Side::Right => {
                flux_git::merge3::lines(&self.theirs, &self.theirs_lines, &block.chunk.theirs)
            }
        }
    }

    pub fn base_text(&self, block: &Block) -> &str {
        flux_git::merge3::lines(&self.base, &self.base_lines, &block.chunk.base)
    }

    /// The text a block has in the result after taking `order` (the base when nothing is taken).
    pub fn expected(&self, block: &Block, order: &[Side]) -> String {
        if order.is_empty() {
            return self.base_text(block).to_string();
        }
        let mut text = String::new();
        for side in order {
            text = join(&text, self.side_text(block, *side));
        }
        text
    }

    /// How many conflicts still need a decision.
    pub fn unresolved(&self) -> usize {
        self.blocks.iter().filter(|block| block.pending()).count()
    }

    pub fn conflicts(&self) -> usize {
        self.blocks
            .iter()
            .filter(|block| block.is_conflict())
            .count()
    }

    /// The edit that takes `side` of block `index` into the result: the characters of `result` to
    /// replace, the new text, and the block's lines after it. `None` — the side is decided already.
    /// The first side taken replaces the block's lines; the second goes after the first.
    pub fn take_edit(
        &self,
        index: usize,
        side: Side,
        result: &Rope,
    ) -> Option<(Range<usize>, String, Range<u32>)> {
        let block = self.blocks.get(index)?;
        if block.take(side) != Take::Pending {
            return None;
        }
        let text = self.side_text(block, side);
        let start = char_of_line(result, block.result.start);
        let end = char_of_line(result, block.result.end);
        let first = block.result.start;
        if block.take(side.other()) == Take::Applied {
            let current = result.slice(start..end).to_string();
            let added = join(&current, text);
            let insert = added[current.len()..].to_string();
            let lines = first..first + line_count(&added);
            Some((end..end, insert, lines))
        } else {
            let lines = first..first + line_count(text);
            Some((start..end, text.to_string(), lines))
        }
    }

    /// The block took `side` (its text is in the result already: [`Self::take_edit`]).
    pub fn applied(&mut self, index: usize, side: Side, lines: Range<u32>) {
        if let Some(block) = self.blocks.get_mut(index) {
            block.set_take(side, Take::Applied);
            block.order.push(side);
            block.result = lines;
        }
    }

    /// The block leaves `side` out.
    pub fn ignore(&mut self, index: usize, side: Side) {
        if let Some(block) = self.blocks.get_mut(index)
            && block.take(side) == Take::Pending
        {
            block.set_take(side, Take::Ignored);
        }
    }

    /// The magic wand merged the block: its text is `merged` (in the result already).
    pub fn merged(&mut self, index: usize, merged: String, lines: Range<u32>) {
        if let Some(block) = self.blocks.get_mut(index) {
            block.left = Take::Applied;
            block.right = Take::Applied;
            block.order = vec![Side::Left, Side::Right];
            block.merged = Some(merged);
            block.result = lines;
        }
    }

    /// The blocks follow the edits of the result (`changes` in order; `text` — the result after the
    /// last one); blocks the edits touched are recognized again (undo, redo, typing a side's text).
    pub fn follow_edits(&mut self, changes: &[TextChange], text: &Rope) {
        for index in self.follow(changes, text) {
            self.recognize(index, text);
        }
    }

    /// Moves the blocks' lines through the edits; returns the blocks the edits touched.
    fn follow(&mut self, changes: &[TextChange], text: &Rope) -> BTreeSet<usize> {
        let mut touched = BTreeSet::new();
        for (index, change) in changes.iter().enumerate() {
            let after = changes.get(index + 1).map_or(text, |next| &next.old_text);
            for (at, block) in self.blocks.iter_mut().enumerate() {
                let old = &change.old_text;
                let start = char_of_line(old, block.result.start);
                let end = char_of_line(old, block.result.end);
                let empty = start == end;
                if touches(&change.changes, start, end) {
                    touched.insert(at);
                }
                let new_start = change.changes.map_pos(start, Assoc::Before);
                // An empty block takes what is typed at its place.
                let assoc = if empty { Assoc::After } else { Assoc::Before };
                let new_end = change.changes.map_pos(end, assoc);
                let first = start_line(after, new_start);
                let last = end_line(after, new_end).max(first);
                block.result = first..last;
            }
        }
        touched
    }

    /// A block's text matches a state it could be in: it takes that state (undo of a taken side
    /// brings the base back — the side is pending again; redo takes it again).
    fn recognize(&mut self, index: usize, text: &Rope) {
        let Some(block) = self.blocks.get(index) else {
            return;
        };
        if !block.is_conflict() {
            return;
        }
        let current = block_text(text, &block.result);
        let now = match &block.merged {
            Some(merged) => merged.clone(),
            None => self.expected(block, &block.order),
        };
        if current == now {
            return;
        }
        let candidates: [&[Side]; 5] = [
            &[],
            &[Side::Left],
            &[Side::Right],
            &[Side::Left, Side::Right],
            &[Side::Right, Side::Left],
        ];
        let found = candidates
            .iter()
            .find(|order| self.expected(block, order) == current)
            .map(|order| order.to_vec());
        let Some(order) = found else {
            return;
        };
        let block = &mut self.blocks[index];
        for side in [Side::Left, Side::Right] {
            let take = if order.contains(&side) {
                Take::Applied
            } else if block.take(side) == Take::Ignored {
                Take::Ignored
            } else {
                Take::Pending
            };
            block.set_take(side, take);
        }
        block.order = order;
        block.merged = None;
    }

    /// The block at a line of a pane (in the result, or on a side), if any; an empty block counts at
    /// the line it is before.
    pub fn block_at(&self, pane: Pane, line: u32) -> Option<usize> {
        self.blocks.iter().position(|block| {
            let range = match pane {
                Pane::Left => &block.chunk.ours,
                Pane::Result => &block.result,
                Pane::Right => &block.chunk.theirs,
            };
            range.contains(&line) || (range.is_empty() && range.start == line)
        })
    }

    /// Line pairs to scroll a side along the result: `old` — the result's lines, `new` — the side's.
    fn hunks(&self, side: Side) -> Vec<flux_git::Hunk> {
        self.blocks
            .iter()
            .map(|block| flux_git::Hunk {
                old: block.result.clone(),
                new: block.lines(side).clone(),
            })
            .collect()
    }
}

/// `second` after `first`, on a line of its own.
fn join(first: &str, second: &str) -> String {
    if !first.is_empty() && !first.ends_with('\n') && !second.is_empty() {
        format!("{first}\n{second}")
    } else {
        format!("{first}{second}")
    }
}

/// How many lines a text has (the last one may lack its line break).
fn line_count(text: &str) -> u32 {
    let breaks = text.matches('\n').count();
    let partial = !text.is_empty() && !text.ends_with('\n');
    (breaks + usize::from(partial)) as u32
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

/// The first character of a line; past the last line — the end of the text.
fn char_of_line(text: &Rope, line: u32) -> usize {
    let line = line as usize;
    if line >= text.len_lines() {
        text.len_chars()
    } else {
        text.line_to_char(line)
    }
}

/// The line a block starting at character `pos` starts on.
fn start_line(text: &Rope, pos: usize) -> u32 {
    text.char_to_line(pos.min(text.len_chars())) as u32
}

/// The line after a block ending at character `pos`: the line itself when `pos` starts it,
/// otherwise the next one (the block takes the line it ends inside).
fn end_line(text: &Rope, pos: usize) -> u32 {
    let len = text.len_chars();
    if pos >= len {
        let lines = text.len_lines();
        let terminated = len == 0 || text.char(len - 1) == '\n';
        return (if terminated { lines - 1 } else { lines }) as u32;
    }
    let line = text.char_to_line(pos);
    if text.line_to_char(line) == pos {
        line as u32
    } else {
        line as u32 + 1
    }
}

/// The text of lines `range` of a rope.
fn block_text(text: &Rope, range: &Range<u32>) -> String {
    let start = char_of_line(text, range.start);
    let end = char_of_line(text, range.end).max(start);
    text.slice(start..end).to_string()
}

/// Whether a change touches the characters `start..end` (an empty range: the point).
fn touches(changes: &ChangeSet, start: usize, end: usize) -> bool {
    let mut old = 0;
    for op in changes.ops() {
        match op {
            Op::Retain(n) => old += n,
            Op::Delete(n) => {
                let (from, to) = (old, old + n);
                let hit = if start == end {
                    from <= start && start <= to
                } else {
                    from < end && to > start
                };
                if hit {
                    return true;
                }
                old += n;
            }
            Op::Insert(_) => {
                let hit = if start == end {
                    old == start
                } else {
                    start <= old && old < end
                };
                if hit {
                    return true;
                }
            }
        }
        if old > end {
            break;
        }
    }
    false
}

/// The line of the other text that corresponds to (fractional) line `line` of one: the same
/// distance from the nearest block above; inside a block, the same share of it. `from_old` — `line`
/// is an `old` line.
fn map_line(hunks: &[flux_git::Hunk], line: f32, from_old: bool) -> f32 {
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
/// view maps through the blocks; at its top or its bottom, the leader takes the follower along.
fn follow(
    hunks: &[flux_git::Hunk],
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

/// The captions of the sides: what each one is in the operation in progress — in a rebase the left
/// side (stage 2) is the upstream and the right one the commit being replayed.
pub(crate) fn captions(state: RepoState, ours: &str, theirs: &str) -> (String, String) {
    let named = |label: &str, name: &str| {
        if name.is_empty() {
            label.to_string()
        } else {
            format!("{label} · {name}")
        }
    };
    match state {
        RepoState::Merging => (named(tr("Yours"), ours), named(tr("Theirs"), theirs)),
        RepoState::Rebasing => (named(tr("Upstream"), ours), named(tr("Yours"), theirs)),
        RepoState::CherryPicking => (
            named(tr("Yours"), ours),
            named(tr("Cherry-picking"), theirs),
        ),
        RepoState::Reverting => (named(tr("Yours"), ours), named(tr("Reverting"), theirs)),
        // An unstash that conflicted.
        RepoState::Normal | RepoState::Bisecting => {
            (named(tr("Current"), ours), tr("Stash").to_string())
        }
    }
}

/// What a file one side deleted (or both) says instead of the panes; `None` — both sides have it.
pub(crate) fn one_sided(kind: ConflictKind) -> Option<&'static str> {
    match kind {
        ConflictKind::DeletedByUs => Some("Deleted on the left side, changed on the right one"),
        ConflictKind::DeletedByThem => Some("Changed on the left side, deleted on the right one"),
        ConflictKind::BothDeleted => Some("Deleted on both sides"),
        _ => None,
    }
}

// --- The view ---

/// Changed words of a block: (line in the block, character columns).
type LineSpans = Vec<(u32, Range<usize>)>;

/// A pane of the tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pane {
    Left,
    Result,
    Right,
}

/// What the tab shows.
enum State {
    /// The versions are being read.
    Loading,
    /// Nothing to merge here: why.
    Note(SharedString),
    /// One side deleted the file, or it is binary: only a whole side can be taken.
    OneSided(SharedString),
    Ready(Box<Panes>),
}

/// The three panes and what is known about them.
struct Panes {
    model: MergeModel,
    left: Entity<Editor>,
    result: Entity<Editor>,
    right: Entity<Editor>,
    /// Changed words of each conflict on each side, by block index.
    words: Vec<(LineSpans, LineSpans)>,
    /// The file's lines end with `\r\n`: the result is written with them.
    crlf: bool,
    /// The pane that scrolled last leads the others.
    driver: Pane,
    /// The scroll offsets of the panes as of the last frame.
    synced: Option<[Point<f32>; 3]>,
    /// The captions of the sides.
    captions: (String, String),
}

/// What the merge tool asks the workspace to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeViewEvent {
    /// The file is resolved (or the conflict is gone): the tab closes.
    Close,
    /// Jump to Source: the file at a location, in its own tab.
    OpenFile(Location),
}

pub struct MergeView {
    repo: usize,
    path: PathBuf,
    /// The path as the git hub knows it (canonical).
    key: PathBuf,
    /// Relative to the repository's working tree.
    relative: Option<String>,
    git: Entity<GitStore>,
    state: State,
    /// The result differs from how the tool opened it.
    modified: bool,
    /// The file was seen conflicted: when it stops being so, the tab closes.
    was_conflicted: bool,
    /// Apply or Accept is running: the watcher leaves the tab alone meanwhile.
    finishing: bool,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<MergeViewEvent> for MergeView {}

impl MergeView {
    /// The merge tool for a conflicted file (`path` absolute) of repository `repo`.
    pub fn new(
        repo: usize,
        path: PathBuf,
        git: Entity<GitStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let key = canonical(&path);
        let relative = git
            .read(cx)
            .repos()
            .get(repo)
            .and_then(|entry| entry.repo.relative(&key));
        let subscriptions = vec![cx.observe_in(&git, window, |this, _, window, cx| {
            this.git_changed(window, cx)
        })];
        let mut view = Self {
            repo,
            path,
            key,
            relative,
            git,
            state: State::Loading,
            modified: false,
            was_conflicted: false,
            finishing: false,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        view.load(window, cx);
        view
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The tab's label: the file name.
    pub fn title(&self) -> SharedString {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into()
    }

    /// The result has changes that aren't applied: closing the tab asks.
    pub fn is_modified(&self, _cx: &App) -> bool {
        self.modified
    }

    /// Closing the tab: an unapplied result is asked about; `true` — close.
    pub fn confirm_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        if !self.modified {
            return Task::ready(true);
        }
        let answer = Dialog::warning(tr("Discard the merge result?"))
            .message(tr(
                "What you did in the merge tool is lost; the file stays conflicted.",
            ))
            .danger(tr("Discard"))
            .cancel(tr("Cancel"))
            .show(window, cx);
        cx.spawn(async move |_, _| answer.await == Some(0))
    }

    /// The conflict kind of the file, from the status; `None` — it isn't conflicted.
    fn conflict(&self, cx: &App) -> Option<Option<ConflictKind>> {
        self.git
            .read(cx)
            .conflicts()
            .into_iter()
            .find(|change| change.path == self.key)
            .map(|change| change.conflict)
    }

    /// Reads the three versions in the background.
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(relative) = self.relative.clone() else {
            self.state = State::Note(tr("The file is not in a Git repository").into());
            return;
        };
        let Some(kind) = self.conflict(cx) else {
            self.state = State::Note(tr("The file has no conflicts").into());
            return;
        };
        self.was_conflicted = true;
        if let Some(reason) = kind.and_then(one_sided) {
            self.state = State::OneSided(tr(reason).into());
            return;
        }
        let read = self.git.update(cx, |git, cx| {
            git.conflict_versions(self.repo, &relative, cx)
        });
        cx.spawn_in(window, async move |this, cx| {
            let versions = read.await;
            this.update_in(cx, |this, window, cx| this.loaded(versions, window, cx))
                .ok();
        })
        .detach();
    }

    fn loaded(
        &mut self,
        versions: Result<ConflictVersions, flux_git::GitError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let versions = match versions {
            Ok(versions) => versions,
            Err(err) => {
                self.state =
                    State::Note(trf("Can't read the file's versions: {0}", &[&err]).into());
                return cx.notify();
            }
        };
        let (Some(ours), Some(theirs)) = (&versions.ours, &versions.theirs) else {
            self.state = State::OneSided(tr("One side has no such file").into());
            return cx.notify();
        };
        let base = versions.base.as_deref().unwrap_or_default();
        if [base, ours.as_slice(), theirs.as_slice()]
            .into_iter()
            .any(flux_git::is_binary)
        {
            self.state = State::OneSided(tr("A binary file: take one side whole").into());
            return cx.notify();
        }
        let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).replace("\r\n", "\n");
        let crlf = String::from_utf8_lossy(ours).contains("\r\n");
        let (base, ours, theirs) = (text(base), text(ours), text(theirs));
        let model = MergeModel::new(&base, &ours, &theirs);
        let words = model
            .blocks
            .iter()
            .map(|block| {
                let small = block.chunk.ours.len() <= WORD_DIFF_MAX_LINES
                    && block.chunk.theirs.len() <= WORD_DIFF_MAX_LINES;
                if !block.is_conflict() || !small {
                    return (Vec::new(), Vec::new());
                }
                let left = model.side_text(block, Side::Left);
                let right = model.side_text(block, Side::Right);
                let (removed, added) = flux_git::diff_words(left, right);
                (line_spans(left, &removed), line_spans(right, &added))
            })
            .collect();
        let path = self.path.clone();
        let side = |text: &str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut editor = Editor::new(Document::from_text(text), window, cx);
                editor.set_highlight_path(&path, cx);
                editor.read_only = true;
                editor
            })
        };
        let left = side(&ours, window, cx);
        let right = side(&theirs, window, cx);
        let result = cx.new(|cx| {
            let mut editor = Editor::new(Document::from_text(&model.initial), window, cx);
            editor.set_highlight_path(&path, cx);
            editor.recorded_changes = Some(Vec::new());
            editor
        });
        self._subscriptions
            .push(cx.subscribe(&result, |this, _, event: &EditorEvent, cx| {
                if *event == EditorEvent::Edited {
                    this.result_edited(cx);
                }
            }));
        let (ours_name, theirs_name) = self.git.read(cx).conflict_sides(self.repo);
        let state = self.git.read(cx).operation(self.repo).state;
        let captions = captions(state, &ours_name, &theirs_name);
        let focus = result.focus_handle(cx);
        self.state = State::Ready(Box::new(Panes {
            model,
            left,
            result,
            right,
            words,
            crlf,
            driver: Pane::Result,
            synced: None,
            captions,
        }));
        // The first conflict is where the work is.
        self.go_to_conflict(true, window, cx);
        if self.focus_handle.contains_focused(window, cx) {
            window.focus(&focus);
        }
        cx.notify();
    }

    /// The git status changed: a file that stopped being conflicted closes the tab; one that
    /// became conflicted (the tab opened before the status knew) loads.
    fn git_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.finishing {
            return;
        }
        let conflicted = self.conflict(cx).is_some();
        if self.was_conflicted && !conflicted {
            self.modified = false;
            return cx.emit(MergeViewEvent::Close);
        }
        if !self.was_conflicted && conflicted && matches!(self.state, State::Note(_)) {
            self.load(window, cx);
        }
        cx.notify();
    }

    /// The user edited the result: the blocks follow.
    fn result_edited(&mut self, cx: &mut Context<Self>) {
        let State::Ready(panes) = &mut self.state else {
            return;
        };
        let (changes, text) = panes.result.update(cx, |editor, _| {
            let changes = editor
                .recorded_changes
                .as_mut()
                .map(std::mem::take)
                .unwrap_or_default();
            (changes, editor.document.text().clone())
        });
        if !changes.is_empty() {
            panes.model.follow_edits(&changes, &text);
        }
        self.update_modified(cx);
        cx.notify();
    }

    fn update_modified(&mut self, cx: &App) {
        let State::Ready(panes) = &self.state else {
            return;
        };
        let text = panes.result.read(cx).document.text();
        let decided = panes.model.blocks.iter().any(|block| {
            block.is_conflict() && (block.left != Take::Pending || block.right != Take::Pending)
        });
        self.modified = decided || *text != panes.model.initial.as_str();
    }

    // --- Taking sides ---

    /// Takes a side of a block into the result (one undoable edit).
    fn take(&mut self, index: usize, side: Side, cx: &mut Context<Self>) {
        let State::Ready(panes) = &mut self.state else {
            return;
        };
        let text = panes.result.read(cx).document.text().clone();
        let Some((range, insert, lines)) = panes.model.take_edit(index, side, &text) else {
            return;
        };
        let (changes, text) = panes.result.update(cx, |editor, cx| {
            editor.replace_ranges(vec![(range, insert)], cx);
            let changes = editor
                .recorded_changes
                .as_mut()
                .map(std::mem::take)
                .unwrap_or_default();
            (changes, editor.document.text().clone())
        });
        // The other blocks move with the edit; this one is exactly the taken text.
        panes.model.follow(&changes, &text);
        panes.model.applied(index, side, lines.clone());
        // The cursor stays on the block (the edit left it after the new text): the other side's
        // key works on the same conflict.
        let line = lines.start as usize;
        panes.result.update(cx, |editor, cx| {
            let position = editor.position(line, 0);
            editor.select_range(position..position, cx);
        });
        self.update_modified(cx);
        cx.notify();
    }

    fn ignore(&mut self, index: usize, side: Side, cx: &mut Context<Self>) {
        if let State::Ready(panes) = &mut self.state {
            panes.model.ignore(index, side);
        }
        self.update_modified(cx);
        cx.notify();
    }

    /// ⌃⌘→ / ⌃⌘←: the conflict at the cursor of the focused pane is resolved with one side.
    fn take_at_cursor(&mut self, side: Side, window: &mut Window, cx: &mut Context<Self>) {
        let Some((pane, line)) = self.cursor(window, cx) else {
            return;
        };
        let State::Ready(panes) = &self.state else {
            return;
        };
        let index = panes.model.block_at(pane, line).filter(|index| {
            let block = &panes.model.blocks[*index];
            block.is_conflict() && block.take(side) == Take::Pending
        });
        let Some(index) = index else {
            return;
        };
        self.take(index, side, cx);
        // From the keyboard, a side resolves the conflict (JetBrains' "Resolve using Left / Right"):
        // the other side is left out. Both sides go in with the strips' arrows.
        self.ignore(index, side.other(), cx);
        // A conflict decided from the keyboard: on to the next one, as F7 would.
        let next = match &self.state {
            State::Ready(panes) => {
                !panes.model.blocks[index].pending() && panes.model.unresolved() > 0
            }
            _ => false,
        };
        if next {
            self.go_to_conflict(true, window, cx);
        }
    }

    /// The focused pane and the line of its cursor (the result when none is focused).
    fn cursor(&self, window: &Window, cx: &App) -> Option<(Pane, u32)> {
        let State::Ready(panes) = &self.state else {
            return None;
        };
        let pane = if panes.left.focus_handle(cx).is_focused(window) {
            Pane::Left
        } else if panes.right.focus_handle(cx).is_focused(window) {
            Pane::Right
        } else {
            Pane::Result
        };
        let editor = match pane {
            Pane::Left => &panes.left,
            Pane::Result => &panes.result,
            Pane::Right => &panes.right,
        };
        Some((pane, cursor_line(editor, cx) as u32))
    }

    /// The magic wand: every pending conflict whose sides changed different words is merged.
    fn resolve_simple(&mut self, cx: &mut Context<Self>) {
        let State::Ready(panes) = &mut self.state else {
            return;
        };
        let mut merged = 0;
        for index in 0..panes.model.blocks.len() {
            let block = &panes.model.blocks[index];
            if !block.is_conflict() || block.left != Take::Pending || block.right != Take::Pending {
                continue;
            }
            let Some(text) = simple_merge(
                panes.model.base_text(block),
                panes.model.side_text(block, Side::Left),
                panes.model.side_text(block, Side::Right),
            ) else {
                continue;
            };
            let rope = panes.result.read(cx).document.text().clone();
            let start = char_of_line(&rope, block.result.start);
            let end = char_of_line(&rope, block.result.end);
            let lines = block.result.start..block.result.start + line_count(&text);
            let (changes, rope) = panes.result.update(cx, |editor, cx| {
                editor.replace_ranges(vec![(start..end, text.clone())], cx);
                let changes = editor
                    .recorded_changes
                    .as_mut()
                    .map(std::mem::take)
                    .unwrap_or_default();
                (changes, editor.document.text().clone())
            });
            panes.model.follow(&changes, &rope);
            panes.model.merged(index, text, lines);
            merged += 1;
        }
        let message = if merged == 0 {
            tr("No simple conflicts: each one needs a decision").to_string()
        } else {
            trn(merged, "{n} conflict resolved", "{n} conflicts resolved")
        };
        self.git.update(cx, |git, cx| {
            git.report(crate::git::GitEvent::Message(message.into()), cx)
        });
        self.update_modified(cx);
        cx.notify();
    }

    /// Whether the magic wand has something to do.
    fn has_simple(&self) -> bool {
        let State::Ready(panes) = &self.state else {
            return false;
        };
        panes.model.blocks.iter().any(|block| {
            block.is_conflict()
                && block.left == Take::Pending
                && block.right == Take::Pending
                && simple_merge(
                    panes.model.base_text(block),
                    panes.model.side_text(block, Side::Left),
                    panes.model.side_text(block, Side::Right),
                )
                .is_some()
        })
    }

    // --- Navigation ---

    /// F7 / ⇧F7: the next / previous unresolved conflict after (before) the cursor of the result;
    /// with no unresolved ones, any block.
    fn go_to_conflict(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let State::Ready(panes) = &mut self.state else {
            return;
        };
        let line = cursor_line(&panes.result, cx) as u32;
        let any_pending = panes.model.unresolved() > 0;
        let candidates: Vec<&Block> = panes
            .model
            .blocks
            .iter()
            .filter(|block| !any_pending || block.pending())
            .collect();
        let target = if forward {
            candidates
                .iter()
                .find(|block| block.result.start > line)
                .or(candidates.first())
        } else {
            candidates
                .iter()
                .rev()
                .find(|block| block.result.start < line)
                .or(candidates.last())
        };
        let Some(block) = target else {
            return;
        };
        let target = block.result.start as usize;
        panes.driver = Pane::Result;
        let result = panes.result.clone();
        result.update(cx, |editor, cx| {
            let position = editor.position(target, 0);
            editor.select_range(position..position, cx);
        });
        let _ = window;
        cx.notify();
    }

    /// F4: the file in its own tab, at the cursor's line of the result.
    fn jump_to_source(&mut self, cx: &mut Context<Self>) {
        let line = match &self.state {
            State::Ready(panes) => cursor_line(&panes.result, cx),
            _ => 0,
        };
        cx.emit(MergeViewEvent::OpenFile(Location {
            path: self.path.clone(),
            line,
            start: 0,
            end: 0,
        }));
    }

    // --- Finishing ---

    /// Apply: with conflicts left, asks first; then the result is written and the file marked
    /// resolved.
    fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let State::Ready(panes) = &self.state else {
            return;
        };
        let unresolved = panes.model.unresolved();
        if unresolved == 0 {
            return self.write(window, cx);
        }
        let answer = Dialog::warning(trn(
            unresolved,
            "{n} conflict is unresolved",
            "{n} conflicts are unresolved",
        ))
        .message(tr("Save the result and mark the file resolved anyway?"))
        .normal(tr("Apply"))
        .cancel(tr("Continue Merging"))
        .show(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Some(0) {
                this.update_in(cx, |this, window, cx| this.write(window, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Writes the result (with the file's line endings) and marks the file resolved.
    fn write(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (State::Ready(panes), Some(relative)) = (&self.state, self.relative.clone()) else {
            return;
        };
        let text = file_text(
            &panes.result.read(cx).document.text().to_string(),
            panes.crlf,
        );
        self.finish(
            relative.clone(),
            move |git, repo, cx| git.mark_resolved(repo, &relative, Some(text.into_bytes()), cx),
            window,
            cx,
        );
    }

    /// Accept Left / Accept Right: the whole file from one side.
    fn accept(&mut self, side: Side, window: &mut Window, cx: &mut Context<Self>) {
        let Some(relative) = self.relative.clone() else {
            return;
        };
        self.finish(
            relative.clone(),
            move |git, repo, cx| git.accept_side(repo, vec![relative], side.conflict_side(), cx),
            window,
            cx,
        );
    }

    /// Runs what resolves the file; on success the tab closes and, when it was the repository's
    /// last conflict, a notification says what concludes the operation.
    fn finish(
        &mut self,
        relative: String,
        resolve: impl FnOnce(
            &mut GitStore,
            usize,
            &mut Context<GitStore>,
        ) -> Task<Result<(), flux_git::GitError>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.finishing {
            return;
        }
        let repo = self.repo;
        let last = crate::conflicts_dialog::remaining_after(&self.git, repo, &[relative], cx) == 0;
        self.finishing = true;
        let task = self.git.update(cx, |git, cx| resolve(git, repo, cx));
        let git = self.git.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.finishing = false;
                match result {
                    Ok(()) => {
                        this.modified = false;
                        let back = crate::conflicts_dialog::opened_from_dialog(&this.key, cx);
                        if last {
                            crate::conflicts_dialog::notify_resolved(&git, repo, cx);
                        } else if back {
                            // Opened from the Conflicts dialog: back to it, for the next file.
                            window.dispatch_action(Box::new(crate::git::ResolveConflicts), cx);
                        }
                        cx.emit(MergeViewEvent::Close);
                    }
                    Err(err) => git.update(cx, |git, cx| {
                        git.notify_error(tr("Couldn't resolve the conflict"), &err, cx)
                    }),
                }
            })
            .ok();
        })
        .detach();
    }

    // --- Scrolling together ---

    /// The pane that moved alone since the last frame leads; the others follow through the blocks.
    /// Horizontally all three scroll the same.
    fn sync_scroll(&mut self, cx: &mut Context<Self>) {
        let State::Ready(panes) = &mut self.state else {
            return;
        };
        let editors = [
            panes.left.clone(),
            panes.result.clone(),
            panes.right.clone(),
        ];
        let scrolls = editors.clone().map(|editor| editor.read(cx).scroll);
        let moved = match panes.synced {
            Some(last) => [0, 1, 2].map(|i| scrolls[i].y != last[i].y),
            // The first frame: the sides come to where the result is.
            None => [false, true, false],
        };
        let alone = |i: usize| moved[i] && moved.iter().filter(|m| **m).count() == 1;
        if alone(0) {
            panes.driver = Pane::Left;
        } else if alone(1) {
            panes.driver = Pane::Result;
        } else if alone(2) {
            panes.driver = Pane::Right;
        }
        let x_moved: Vec<usize> = match panes.synced {
            Some(last) => (0..3).filter(|&i| scrolls[i].x != last[i].x).collect(),
            None => Vec::new(),
        };
        let x = (x_moved.len() == 1).then(|| scrolls[x_moved[0]].x);
        if moved.iter().any(|m| *m) {
            let max = editors.clone().map(|editor| max_scroll(&editor, cx));
            let height = |editor: &Entity<Editor>| {
                editor
                    .read(cx)
                    .layout
                    .as_ref()
                    .map_or(0., |layout| f32::from(layout.text_bounds.size.height))
            };
            let left_hunks = panes.model.hunks(Side::Left);
            let right_hunks = panes.model.hunks(Side::Right);
            // The result's offset: its own, or where the leading side takes it.
            let result_y = match panes.driver {
                Pane::Left => follow(
                    &left_hunks,
                    scrolls[0].y,
                    max[0],
                    max[1],
                    height(&editors[0]),
                    false,
                ),
                Pane::Right => follow(
                    &right_hunks,
                    scrolls[2].y,
                    max[2],
                    max[1],
                    height(&editors[2]),
                    false,
                ),
                Pane::Result => scrolls[1].y,
            };
            let result_height = height(&editors[1]);
            let left_y = match panes.driver {
                Pane::Left => scrolls[0].y,
                _ => follow(&left_hunks, result_y, max[1], max[0], result_height, true),
            };
            let right_y = match panes.driver {
                Pane::Right => scrolls[2].y,
                _ => follow(&right_hunks, result_y, max[1], max[2], result_height, true),
            };
            for (index, y) in [left_y, result_y, right_y].into_iter().enumerate() {
                let x = x.unwrap_or(scrolls[index].x);
                set_scroll(&editors[index], point(x, y), cx);
            }
        } else if let Some(x) = x {
            for (index, editor) in editors.iter().enumerate() {
                set_scroll(editor, point(x, scrolls[index].y), cx);
            }
        }
        panes.synced = Some(editors.map(|editor| editor.read(cx).scroll));
    }

    // --- Colors ---

    /// Hands the three editors their colors for this frame.
    fn decorate(&self, cx: &mut Context<Self>) {
        let State::Ready(panes) = &self.state else {
            return;
        };
        let ui = Theme::ui(cx);
        let mut left = Decorations::default();
        let mut middle = Decorations::default();
        let mut right = Decorations::default();
        let left_text = panes.left.read(cx).document.text().clone();
        let right_text = panes.right.read(cx).document.text().clone();
        for (index, block) in panes.model.blocks.iter().enumerate() {
            let pending = block.pending();
            let (background, line) = if pending {
                (ui.diff_conflict_bg, ui.diff_conflict)
            } else {
                (ui.diff_resolved_bg, ui.diff_deleted)
            };
            let range = |lines: &Range<u32>| lines.start as usize..lines.end as usize;
            if block.result.is_empty() {
                middle.boundaries.push((block.result.start as usize, line));
            } else {
                middle.lines.push((range(&block.result), background));
            }
            for (side, decorations, text) in [
                (Side::Left, &mut left, &left_text),
                (Side::Right, &mut right, &right_text),
            ] {
                if !block.changed_on(side) || block.take(side) == Take::Ignored && !pending {
                    continue;
                }
                let lines = block.lines(side);
                let side_pending = block.is_conflict() && block.take(side) == Take::Pending;
                let (background, line) = if side_pending {
                    (ui.diff_conflict_bg, ui.diff_conflict)
                } else {
                    (ui.diff_resolved_bg, ui.diff_deleted)
                };
                if lines.is_empty() {
                    decorations.boundaries.push((lines.start as usize, line));
                } else {
                    decorations.lines.push((range(lines), background));
                }
                if side_pending && let Some((left_words, right_words)) = panes.words.get(index) {
                    let words = match side {
                        Side::Left => left_words,
                        Side::Right => right_words,
                    };
                    decorations.words.extend(word_ranges(
                        text,
                        lines.start,
                        words,
                        ui.diff_conflict_word,
                    ));
                }
            }
        }
        for (editor, decorations) in [
            (&panes.left, left),
            (&panes.result, middle),
            (&panes.right, right),
        ] {
            editor.update(cx, |editor, _| {
                editor.frame_decorations = Some(Rc::new(decorations))
            });
        }
    }

    // --- Rendering ---

    fn render_toolbar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let keys = |action: &dyn gpui::Action| ui::shortcut_in(action, &self.focus_handle, window);
        let counter = match &self.state {
            State::Ready(panes) if panes.model.conflicts() == 0 => tr("No conflicts").to_string(),
            State::Ready(panes) => match panes.model.unresolved() {
                0 => tr("All conflicts resolved").to_string(),
                left => trn(left, "{n} conflict left", "{n} conflicts left"),
            },
            _ => String::new(),
        };
        let resolved = matches!(&self.state, State::Ready(panes) if panes.model.unresolved() == 0);
        let path = self
            .relative
            .clone()
            .unwrap_or_else(|| self.title().to_string());
        let ready = matches!(self.state, State::Ready(_));
        let wand = self.has_simple();
        div()
            .flex_none()
            .h(px(TOOLBAR_HEIGHT))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .when(ready, |bar| {
                bar.child(
                    ui::icon_button("merge-previous", IconName::ArrowUp, ui)
                        .tooltip(ui::tooltip(
                            tr("Previous Conflict"),
                            keys(&PreviousConflict),
                        ))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.go_to_conflict(false, window, cx)
                        })),
                )
                .child(
                    ui::icon_button("merge-next", IconName::ArrowDown, ui)
                        .tooltip(ui::tooltip(tr("Next Conflict"), keys(&NextConflict)))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.go_to_conflict(true, window, cx)
                        })),
                )
            })
            .child(
                div()
                    .ml_2()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(if resolved { ui.success } else { ui.text_muted })
                    .when(resolved, |label| {
                        label.child(icon(IconName::CheckCircle, ui.success).size(px(13.)))
                    })
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
            .when(wand, |bar| {
                bar.child(
                    ui::icon_button("merge-wand", IconName::Sparkle, ui)
                        .tooltip(ui::tooltip(tr("Resolve Simple Conflicts"), None))
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.resolve_simple(cx)),
                        ),
                )
            })
            .when(ready, |bar| {
                bar.child(
                    ui::icon_button("merge-jump", IconName::Pencil, ui)
                        .tooltip(ui::tooltip(tr("Jump to Source"), keys(&JumpToSource)))
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.jump_to_source(cx)),
                        ),
                )
            })
    }

    /// The captions over the panes: the sides and the result.
    fn render_captions(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let (left, right) = match &self.state {
            State::Ready(panes) => panes.captions.clone(),
            _ => Default::default(),
        };
        let caption = |text: String, center: bool| {
            div()
                .flex_1()
                .min_w_0()
                .px_3()
                .truncate()
                .when(center, |caption| caption.text_center())
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.dim)
                .child(text)
        };
        div()
            .flex_none()
            .h(px(CAPTION_HEIGHT))
            .flex()
            .items_center()
            .child(caption(left, false))
            .child(div().flex_none().w(px(STRIP_WIDTH)))
            .child(caption(tr("Result").to_string(), true))
            .child(div().flex_none().w(px(STRIP_WIDTH)))
            .child(caption(right, false))
    }

    fn render_panes(&self, cx: &Context<Self>) -> AnyElement {
        let State::Ready(panes) = &self.state else {
            return div().flex_1().into_any_element();
        };
        let pane = |which: Pane, editor: &Entity<Editor>| {
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .on_scroll_wheel(cx.listener(move |this, _: &ScrollWheelEvent, _, _| {
                    if let State::Ready(panes) = &mut this.state {
                        panes.driver = which;
                    }
                }))
                .child(editor.clone())
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_row()
            .child(pane(Pane::Left, &panes.left))
            .child(self.render_strip(Side::Left, cx))
            .child(pane(Pane::Result, &panes.result))
            .child(self.render_strip(Side::Right, cx))
            .child(pane(Pane::Right, &panes.right))
            .into_any_element()
    }

    /// A strip between a side and the result: a band from each block of the side to its lines in
    /// the result, and on each pending conflict the arrow that takes the side and the cross that
    /// ignores it.
    fn render_strip(&self, side: Side, cx: &Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let strip = div()
            .relative()
            .flex_none()
            .w(px(STRIP_WIDTH))
            .h_full()
            .overflow_hidden();
        let State::Ready(panes) = &self.state else {
            return strip.into_any_element();
        };
        let side_editor = match side {
            Side::Left => panes.left.clone(),
            Side::Right => panes.right.clone(),
        };
        let result_editor = panes.result.clone();
        // Bands: (side lines, result lines, color), drawn with the offsets of this frame.
        let bands: Vec<(Range<u32>, Range<u32>, Hsla)> = panes
            .model
            .blocks
            .iter()
            .filter(|block| block.changed_on(side) && block.take(side) != Take::Ignored)
            .map(|block| {
                let color = if block.is_conflict() && block.take(side) == Take::Pending {
                    ui.diff_conflict_bg
                } else {
                    ui.diff_resolved_bg
                };
                (block.lines(side).clone(), block.result.clone(), color)
            })
            .collect();
        let bands = canvas(|_, _, _| (), {
            let (side_editor, result_editor) = (side_editor.clone(), result_editor.clone());
            move |bounds, (), window, cx| {
                let side_scroll = side_editor.read(cx).scroll.y;
                let result_scroll = result_editor.read(cx).scroll.y;
                for (lines, result, color) in &bands {
                    let (from, to) = match side {
                        Side::Left => ((lines, side_scroll), (result, result_scroll)),
                        Side::Right => ((result, result_scroll), (lines, side_scroll)),
                    };
                    let band = Band::new(from.0, from.1, to.0, to.1);
                    if band.visible(f32::from(bounds.size.height)) {
                        paint_band(bounds, &band, *color, window);
                    }
                }
            }
        })
        .absolute()
        .size_full();
        let side_scroll = side_editor.read(cx).scroll.y;
        let result_scroll = result_editor.read(cx).scroll.y;
        let mut buttons = Vec::new();
        for (index, block) in panes.model.blocks.iter().enumerate() {
            if !block.is_conflict() || block.take(side) != Take::Pending {
                continue;
            }
            let lines = block.lines(side);
            let top = button_top(lines, side_scroll);
            let _ = result_scroll;
            // Laid out before the editors settle: a frame late while scrolling.
            if !(-100. ..=4000.).contains(&top) {
                continue;
            }
            let (arrow, arrow_x, cross_x) = match side {
                Side::Left => (IconName::ArrowRight, 4., 24.),
                Side::Right => (IconName::ArrowLeft, 24., 4.),
            };
            let id = index * 2 + usize::from(side == Side::Right);
            buttons.push(
                strip_button(("merge-take", id), arrow, ui.diff_conflict, ui)
                    .top(px(top))
                    .left(px(arrow_x))
                    .tooltip(ui::tooltip(tr("Accept"), None))
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.take(index, side, cx)),
                    )
                    .into_any_element(),
            );
            buttons.push(
                strip_button(("merge-ignore", id), IconName::Close, ui.text_muted, ui)
                    .top(px(top))
                    .left(px(cross_x))
                    .tooltip(ui::tooltip(tr("Ignore"), None))
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.ignore(index, side, cx)
                        }),
                    )
                    .into_any_element(),
            );
        }
        strip.child(bands).children(buttons).into_any_element()
    }

    /// Instead of the panes: why there is nothing to merge, or what can be taken.
    fn render_note(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let (text, one_sided) = match &self.state {
            State::Loading => return div().flex_1().into_any_element(),
            State::Note(text) => (text.clone(), false),
            State::OneSided(text) => (text.clone(), true),
            State::Ready(_) => return div().into_any_element(),
        };
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_color(ui.text_muted)
                    .child(
                        icon(
                            if one_sided {
                                IconName::Warning
                            } else {
                                IconName::Info
                            },
                            if one_sided { ui.warning } else { ui.dim },
                        )
                        .size(px(14.)),
                    )
                    .child(text),
            )
            .into_any_element()
    }

    fn render_footer(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let ready = matches!(self.state, State::Ready(_));
        let can_accept =
            matches!(self.state, State::Ready(_) | State::OneSided(_)) && !self.finishing;
        let apply_keys = ui::shortcut_in(&ApplyResult, &self.focus_handle, window);
        let (left, right) = match &self.state {
            State::Ready(panes) => panes.captions.clone(),
            _ => Default::default(),
        };
        let accept = |id: &'static str, label: &'static str, side: Side, caption: String| {
            ui::text_button(id, label, false, ui)
                .when(!caption.is_empty(), |button| {
                    button.tooltip(ui::tooltip(caption, None))
                })
                .when(!can_accept, |button| button.opacity(0.5))
                .when(can_accept, |button| {
                    button.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.accept(side, window, cx)
                    }))
                })
        };
        div()
            .flex_none()
            .h(px(FOOTER_HEIGHT))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .child(accept(
                "merge-accept-left",
                tr("Accept Left"),
                Side::Left,
                left,
            ))
            .child(accept(
                "merge-accept-right",
                tr("Accept Right"),
                Side::Right,
                right,
            ))
            .child(div().flex_1())
            // The hints go first when the window is narrow: the buttons stay.
            .when(
                ready && f32::from(window.viewport_size().width) >= HINTS_MIN_WINDOW,
                |footer| {
                    footer.child(
                        ui::hint_bar(&[("F7", tr("next conflict")), ("⌃⌘→", tr("accept"))], ui)
                            .gap_3()
                            .mr_2(),
                    )
                },
            )
            .child(
                ui::text_button("merge-cancel", tr("Cancel"), false, ui).on_click(cx.listener(
                    |this, _: &ClickEvent, window, cx| {
                        let confirm = this.confirm_close(window, cx);
                        cx.spawn(async move |this, cx| {
                            if confirm.await {
                                this.update(cx, |this, cx| {
                                    this.modified = false;
                                    cx.emit(MergeViewEvent::Close)
                                })
                                .ok();
                            }
                        })
                        .detach();
                    },
                )),
            )
            .child(
                ui::primary_button("merge-apply", tr("Apply"), ready && !self.finishing, ui)
                    .tooltip(ui::tooltip(tr("Apply"), apply_keys))
                    .when(ready && !self.finishing, |button| {
                        button.on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| this.apply(window, cx)),
                        )
                    }),
            )
    }
}

/// Where a block is in a strip: the tops and bottoms of its lines on each edge, relative to the top
/// of the text.
struct Band {
    left_top: f32,
    left_bottom: f32,
    right_top: f32,
    right_bottom: f32,
}

impl Band {
    fn new(left: &Range<u32>, left_scroll: f32, right: &Range<u32>, right_scroll: f32) -> Self {
        let lh = theme::LINE_HEIGHT;
        Self {
            left_top: left.start as f32 * lh - left_scroll,
            left_bottom: left.end as f32 * lh - left_scroll,
            right_top: right.start as f32 * lh - right_scroll,
            right_bottom: right.end as f32 * lh - right_scroll,
        }
    }

    fn visible(&self, height: f32) -> bool {
        self.left_bottom.max(self.right_bottom) >= 0. && self.left_top.min(self.right_top) <= height
    }
}

/// The top of a strip button: at the block's first line on the side, or centered on its boundary
/// when the side has no lines there.
fn button_top(lines: &Range<u32>, scroll: f32) -> f32 {
    let lh = theme::LINE_HEIGHT;
    let top = lines.start as f32 * lh - scroll;
    if lines.is_empty() {
        top - STRIP_BUTTON / 2.
    } else {
        top + (lh - STRIP_BUTTON) / 2.
    }
}

/// A band from a block on the left edge to its block on the right edge: S-curves along the top
/// and the bottom, as the connectors of JetBrains' merge window.
fn paint_band(bounds: Bounds<Pixels>, band: &Band, color: Hsla, window: &mut Window) {
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
    // An empty side still gets a sliver, so the band shows where the lines go.
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

/// A small button on a strip: an icon on hover backdrop.
fn strip_button(
    id: impl Into<gpui::ElementId>,
    name: IconName,
    color: Hsla,
    ui: UiColors,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .absolute()
        .size(px(STRIP_BUTTON))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(ui::RADIUS_XS))
        .cursor_pointer()
        .hover(move |style| style.bg(ui.hover))
        .child(icon(name, color).size(px(11.)))
}

impl Focusable for MergeView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.state {
            State::Ready(panes) => panes.result.focus_handle(cx),
            _ => self.focus_handle.clone(),
        }
    }
}

impl Render for MergeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let ready = matches!(self.state, State::Ready(_));
        if ready {
            self.sync_scroll(cx);
            self.decorate(cx);
        }
        let body = if ready {
            self.render_panes(cx)
        } else {
            self.render_note(cx)
        };
        div()
            .key_context("MergeView")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            // ⌘S in the result would ask where to save a document without a path: Apply writes it.
            .capture_action(cx.listener(|_, _: &editor::Save, _, cx| cx.stop_propagation()))
            .on_action(cx.listener(|this, _: &NextConflict, window, cx| {
                this.go_to_conflict(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &PreviousConflict, window, cx| {
                this.go_to_conflict(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &AcceptLeftChange, window, cx| {
                this.take_at_cursor(Side::Left, window, cx)
            }))
            .on_action(cx.listener(|this, _: &AcceptRightChange, window, cx| {
                this.take_at_cursor(Side::Right, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ApplyResult, window, cx| this.apply(window, cx)))
            .on_action(cx.listener(|this, _: &JumpToSource, _, cx| this.jump_to_source(cx)))
            .on_action(
                cx.listener(|this, _: &ResolveSimpleConflicts, _, cx| this.resolve_simple(cx)),
            )
            .child(self.render_toolbar(window, cx))
            .child(ui::divider(ui).mx(px(ui::GAP)))
            .when(ready, |view| view.child(self.render_captions(cx)))
            .child(body)
            .child(ui::divider(ui).mx(px(ui::GAP)))
            .child(self.render_footer(window, cx))
    }
}

// --- Helpers ---

/// The result as the file gets it: with `\r\n` when the file's lines end so (the tool compares and
/// edits without `\r`).
fn file_text(result: &str, crlf: bool) -> String {
    if crlf {
        result.replace('\n', "\r\n")
    } else {
        result.to_string()
    }
}

/// The magic wand's merge of one conflict: the sides' word changes merged when they don't touch.
fn simple_merge(base: &str, ours: &str, theirs: &str) -> Option<String> {
    flux_git::merge3::resolve_simple(base, ours, theirs)
}

/// Byte ranges of a block → (line in the block, character columns), split at line breaks.
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

fn cursor_line(editor: &Entity<Editor>, cx: &App) -> usize {
    let editor = editor.read(cx);
    let text = editor.document.text();
    text.char_to_line(editor.document.selection().primary().head)
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

/// The path as the git hub knows it: canonical, or, for a missing file, its canonical directory and
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
    use flux_core::ChangeSet;

    /// Replaces characters `from..to` of `text` with `insert`: the change and the text after it.
    fn edit(text: &Rope, from: usize, to: usize, insert: &str) -> (TextChange, Rope) {
        let changes = ChangeSet::from_changes(text.len_chars(), [(from, to, Some(insert.into()))]);
        let mut after = text.clone();
        changes.apply(&mut after);
        (
            TextChange {
                old_text: text.clone(),
                changes,
            },
            after,
        )
    }

    /// Takes a side through the model the way the view does.
    fn take(model: &mut MergeModel, text: &Rope, index: usize, side: Side) -> Rope {
        let (range, insert, lines) = model.take_edit(index, side, text).expect("pending");
        let (change, after) = edit(text, range.start, range.end, &insert);
        model.follow(&[change], &after);
        model.applied(index, side, lines);
        after
    }

    fn conflict() -> MergeModel {
        MergeModel::new("a\nb\nc\n", "a\nX\nc\n", "a\nY\nc\n")
    }

    #[test]
    fn taking_both_sides_puts_the_second_after_the_first() {
        let mut model = conflict();
        assert_eq!(model.initial, "a\nb\nc\n");
        assert_eq!(model.unresolved(), 1);
        let text = Rope::from_str(&model.initial);
        let text = take(&mut model, &text, 0, Side::Left);
        assert_eq!(text.to_string(), "a\nX\nc\n");
        assert_eq!(model.blocks[0].result, 1..2);
        assert!(model.blocks[0].pending());
        let text = take(&mut model, &text, 0, Side::Right);
        assert_eq!(text.to_string(), "a\nX\nY\nc\n");
        assert_eq!(model.blocks[0].result, 1..3);
        assert_eq!(model.blocks[0].order, [Side::Left, Side::Right]);
        assert_eq!(model.unresolved(), 0);
        assert!(model.take_edit(0, Side::Left, &text).is_none());
    }

    #[test]
    fn ignoring_a_side_resolves_with_the_other() {
        let mut model = conflict();
        let text = Rope::from_str(&model.initial);
        model.ignore(0, Side::Left);
        assert_eq!(model.unresolved(), 1);
        let text = take(&mut model, &text, 0, Side::Right);
        assert_eq!(text.to_string(), "a\nY\nc\n");
        assert_eq!(model.unresolved(), 0);
        // Ignoring both leaves the base.
        let mut model = conflict();
        model.ignore(0, Side::Left);
        model.ignore(0, Side::Right);
        assert_eq!(model.unresolved(), 0);
    }

    #[test]
    fn non_conflicting_changes_are_applied_from_the_start() {
        let model = MergeModel::new(
            "1\n2\n3\n4\n5\n",
            "0\n1\n2\n3\n4\n5\n",
            "1\n2\n3\n4\nFIVE\n",
        );
        assert_eq!(model.initial, "0\n1\n2\n3\n4\nFIVE\n");
        assert_eq!(model.unresolved(), 0);
        assert_eq!(model.conflicts(), 0);
        assert!(model.blocks.iter().all(|block| !block.pending()));
        assert!(model.blocks[0].changed_on(Side::Left) && !model.blocks[0].changed_on(Side::Right));
    }

    #[test]
    fn blocks_follow_the_lines_while_the_result_is_edited() {
        let mut model = MergeModel::new("a\nb\nc\nd\n", "a\nb\nX\nd\n", "a\nb\nY\nd\n");
        let text = Rope::from_str(&model.initial);
        assert_eq!(model.blocks[0].result, 2..3);
        // A line typed above moves the block down.
        let (change, text) = edit(&text, 0, 0, "new\n");
        model.follow_edits(&[change], &text);
        assert_eq!(model.blocks[0].result, 3..4);
        // A line typed at the start of the next line stays out of it.
        let start = text.line_to_char(4);
        let (change, text) = edit(&text, start, start, "after\n");
        model.follow_edits(&[change], &text);
        assert_eq!(model.blocks[0].result, 3..4);
        // A line break typed inside the block makes it longer.
        let inside = text.line_to_char(3) + 1;
        let (change, text) = edit(&text, inside, inside, "\n");
        model.follow_edits(&[change], &text);
        assert_eq!(model.blocks[0].result, 3..5);
        assert!(model.blocks[0].pending());
    }

    #[test]
    fn undo_and_redo_of_a_taken_side_are_recognized() {
        let mut model = conflict();
        let base = Rope::from_str(&model.initial);
        let taken = take(&mut model, &base, 0, Side::Left);
        assert_eq!(model.blocks[0].left, Take::Applied);
        // Undo: the block's text is the base again — the side is pending.
        let start = taken.line_to_char(1);
        let end = taken.line_to_char(2);
        let (change, undone) = edit(&taken, start, end, "b\n");
        model.follow_edits(&[change], &undone);
        assert_eq!(model.blocks[0].left, Take::Pending);
        assert!(model.blocks[0].order.is_empty());
        assert_eq!(model.blocks[0].result, 1..2);
        // Redo: the left side's text again.
        let start = undone.line_to_char(1);
        let end = undone.line_to_char(2);
        let (change, redone) = edit(&undone, start, end, "X\n");
        model.follow_edits(&[change], &redone);
        assert_eq!(model.blocks[0].left, Take::Applied);
        // Typing something else keeps the decision.
        let at = redone.line_to_char(1);
        let (change, typed) = edit(&redone, at, at, "// ");
        model.follow_edits(&[change], &typed);
        assert_eq!(model.blocks[0].left, Take::Applied);
    }

    #[test]
    fn an_empty_conflict_takes_what_is_typed_at_its_place() {
        // Both sides added different lines at the end: the base has none there.
        let mut model = MergeModel::new("a\n", "a\nours\n", "a\ntheirs\n");
        assert_eq!(model.blocks[0].result, 1..1);
        let text = Rope::from_str(&model.initial);
        let (change, text) = edit(&text, 2, 2, "mine\n");
        model.follow_edits(&[change], &text);
        assert_eq!(model.blocks[0].result, 1..2);
    }

    #[test]
    fn a_side_without_a_final_line_break_is_joined_on_a_line_of_its_own() {
        let mut model = MergeModel::new("a\nb", "a\nX", "a\nY");
        let text = Rope::from_str(&model.initial);
        let text = take(&mut model, &text, 0, Side::Left);
        assert_eq!(text.to_string(), "a\nX");
        let text = take(&mut model, &text, 0, Side::Right);
        assert_eq!(text.to_string(), "a\nX\nY");
        assert_eq!(model.blocks[0].result, 1..3);
    }

    #[test]
    fn the_result_keeps_the_files_line_endings() {
        // The sides come with CRLF; the tool merges them without `\r` and writes CRLF back.
        let text = |s: &str| s.replace("\r\n", "\n");
        let model = MergeModel::new(
            &text("a\r\nx\r\nb\r\n"),
            &text("a\r\nx\r\nB\r\n"),
            &text("A\r\nx\r\nb\r\n"),
        );
        assert_eq!(model.initial, "A\nx\nB\n");
        assert_eq!(file_text(&model.initial, true), "A\r\nx\r\nB\r\n");
        assert_eq!(file_text(&model.initial, false), "A\nx\nB\n");
    }

    #[test]
    fn line_helpers() {
        assert_eq!(line_count(""), 0);
        assert_eq!(line_count("a\n"), 1);
        assert_eq!(line_count("a\nb"), 2);
        assert_eq!(join("a", "b"), "a\nb");
        assert_eq!(join("a\n", "b\n"), "a\nb\n");
        assert_eq!(join("", "b"), "b");
        let text = Rope::from_str("a\nbc");
        assert_eq!(end_line(&text, text.len_chars()), 2);
        assert_eq!(end_line(&text, 2), 1);
        assert_eq!(end_line(&text, 3), 2);
        let text = Rope::from_str("a\n");
        assert_eq!(end_line(&text, 2), 1);
        assert_eq!(end_line(&Rope::from_str(""), 0), 0);
        assert_eq!(start_line(&text, 2), 1);
        assert_eq!(char_of_line(&text, 5), 2);
    }

    #[test]
    fn a_cursor_finds_its_block_on_each_pane() {
        let model = MergeModel::new("a\nb\nc\n", "a\nX\nX2\nc\n", "a\nc\n");
        assert_eq!(model.block_at(Pane::Left, 2), Some(0));
        assert_eq!(model.block_at(Pane::Result, 1), Some(0));
        // Theirs deleted the line: the empty block is before line 1.
        assert_eq!(model.block_at(Pane::Right, 1), Some(0));
        assert_eq!(model.block_at(Pane::Left, 3), None);
    }

    #[test]
    fn captions_name_the_sides_by_the_operation() {
        assert_eq!(
            captions(RepoState::Merging, "main", "feature/x"),
            ("Yours · main".into(), "Theirs · feature/x".into())
        );
        assert_eq!(
            captions(RepoState::Rebasing, "origin/main", "main · a1b2c3d"),
            (
                "Upstream · origin/main".into(),
                "Yours · main · a1b2c3d".into()
            )
        );
        assert_eq!(
            captions(RepoState::Normal, "main", "Stash"),
            ("Current · main".into(), "Stash".into())
        );
        assert_eq!(one_sided(ConflictKind::BothModified), None);
        assert!(one_sided(ConflictKind::DeletedByThem).is_some());
    }

    #[test]
    fn the_magic_wand_merges_conflicts_of_different_words() {
        // Both sides changed the same line, different words of it.
        let mut model = MergeModel::new(
            "let total = count + 1;\n",
            "let total = amount + 1;\n",
            "let total = count + 2;\n",
        );
        assert_eq!(model.unresolved(), 1);
        let block = &model.blocks[0];
        let merged = simple_merge(
            model.base_text(block),
            model.side_text(block, Side::Left),
            model.side_text(block, Side::Right),
        )
        .expect("different words");
        assert_eq!(merged, "let total = amount + 2;\n");
        model.merged(0, merged, 0..1);
        assert_eq!(model.unresolved(), 0);
        // The same word changed differently stays a conflict.
        assert_eq!(simple_merge("a = 1\n", "a = 2\n", "a = 3\n"), None);
    }

    #[test]
    fn changed_words_become_columns_of_lines() {
        let spans = line_spans("let a = 1;\nlet b = 2;\n", &[4..5, 15..16]);
        assert_eq!(spans, vec![(0, 4..5), (1, 4..5)]);
    }
}
