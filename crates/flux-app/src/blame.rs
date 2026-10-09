//! Annotations (part E of stage 6.3), as JetBrains' "Annotate with Git Blame": a column left of the
//! line numbers with the date and the author of the last change of every line, its background
//! brighter for newer commits. A run of lines from one commit is labelled once. Hovering shows the
//! commit (hash, author, date, the whole message), a click shows it in the Git window's log, the
//! right button opens the gutter's menu (Copy Revision Number, Show Diff, Show in Git Log, Close
//! Annotations; on a gutter without annotations — Annotate with Git Blame).
//!
//! ⌥⌘A (`git::Annotate`) turns them on and off for the active file. git annotates the editor's text
//! (`--contents`): unsaved lines are "not committed yet" and have no label. While the user types,
//! the labels follow their lines through the edit and are read again a moment after the last
//! keystroke; after a commit or a checkout (HEAD moved) — right away.
//!
//! Also here: Show History (the active file) and Show History for Selection (its selected lines) —
//! tabs of the Git window, which `git::ShowHistory` opens.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use flux_core::text::line_start;
use flux_core::transaction::Operation;
use flux_core::{Rope, TextChange};
use flux_git::{BlameCommit, Repo};
use gpui::{
    AnyElement, App, Bounds, ClickEvent, ClipboardItem, ContentMask, Context, Corner, CursorStyle,
    DismissEvent, Div, Entity, Focusable, FontWeight, Hitbox, HitboxBehavior, MouseDownEvent,
    MouseMoveEvent, PaintQuad, Pixels, Point, ShapedLine, SharedString, Subscription, Task,
    TextRun, Window, actions, anchored, deferred, div, fill, font, point, prelude::*, px, size,
};

use crate::context_menu::ContextMenu;
use crate::diff_view::DiffSide;
use crate::editor::Editor;
use crate::element::LayoutCache;
use crate::git;
use crate::i18n::{tr, trf};
use crate::popup::{POPUP_GAP, WINDOW_MARGIN};
use crate::theme::{self, Theme, UiColors};
use crate::workspace::Workspace;

actions!(
    git,
    [
        /// Annotations off (the gutter's menu).
        CloseAnnotations,
        /// The gutter's menu over an annotation: its commit's hash to the clipboard…
        CopyAnnotationRevision,
        /// …the diff of the file in that commit…
        ShowAnnotationDiff,
        /// …the commit in the log.
        ShowAnnotationInLog,
    ]
);

/// The labels are read again this long after the last keystroke.
const REBLAME_DELAY: Duration = Duration::from_millis(600);
/// The commit popup shows after the mouse rests on an annotation this long, and hides this long
/// after it left (time to move onto the popup).
const POPUP_SHOW_DELAY: Duration = Duration::from_millis(350);
const POPUP_HIDE_DELAY: Duration = Duration::from_millis(250);
const POPUP_WIDTH: f32 = 460.;
/// The author's name in a label is cut to this many characters.
const AUTHOR_MAX_CHARS: usize = 16;
/// "2026-10-09".
const DATE_CHARS: usize = 10;
/// The oldest commit of a file gets this share of the newest one's shade.
const OLDEST_SHADE: f32 = 0.12;
/// Labels are a little smaller than the code.
const LABEL_FONT_SIZE: f32 = theme::FONT_SIZE - 2.;

/// The editor's annotations and the gutter's menu.
#[derive(Default)]
pub struct BlameState {
    annotations: Option<Annotations>,
    menu: Option<GutterMenu>,
    /// Where the annotation column was drawn last (the mouse is matched against it).
    pub(crate) column: Option<Bounds<Pixels>>,
}

impl BlameState {
    /// The annotations are shown (or being read): the gutter has their column.
    pub fn is_on(&self) -> bool {
        self.annotations.is_some()
    }
}

struct Annotations {
    repo_index: usize,
    repo: Repo,
    /// The file, relative to the working tree.
    relative: String,
    commits: Arc<Vec<BlameCommit>>,
    /// Per commit: its label (date, author) and its shade, 0 (the oldest) to 1 (the newest).
    labels: Vec<(SharedString, SharedString)>,
    shades: Vec<f32>,
    /// The commit of each line of the text; `None` — not committed yet.
    lines: Vec<Option<usize>>,
    /// The longest author label, in characters (the column's width).
    author_chars: usize,
    loaded: bool,
    version: u64,
    task: Option<Task<()>>,
    /// The commit under the mouse (its lines are highlighted), and the popup.
    hovered: Option<usize>,
    popup: Option<CommitPopup>,
    popup_hovered: bool,
    popup_task: Option<Task<()>>,
    /// Whole messages read for the popup, by commit.
    messages: HashMap<usize, String>,
    /// The commit the gutter's menu was opened on.
    menu_commit: Option<usize>,
}

struct CommitPopup {
    commit: usize,
    /// The line the mouse was on: the popup stands next to it.
    line: usize,
}

struct GutterMenu {
    menu: Entity<ContextMenu>,
    position: Point<Pixels>,
    _subscriptions: [Subscription; 2],
}

// --- Turning on and off ---

/// ⌥⌘A: annotations of the editor's file on, or off.
pub fn toggle(editor: &mut Editor, cx: &mut Context<Editor>) {
    if editor.blame.annotations.take().is_some() {
        editor.blame.column = None;
        return cx.notify();
    }
    let Some(path) = editor.document.path().map(PathBuf::from) else {
        return editor.show_status(tr("The file isn’t saved: nothing to annotate").into(), cx);
    };
    let Some(store) = editor.git.store.as_ref().and_then(|store| store.upgrade()) else {
        return editor.show_status(tr("The file isn’t in a Git repository").into(), cx);
    };
    let found = {
        let store = store.read(cx);
        store.repo_index(&path).and_then(|index| {
            let repo = store.repos()[index].repo.clone();
            let relative = repo.relative(&path)?;
            Some((index, repo, relative))
        })
    };
    let Some((repo_index, repo, relative)) = found else {
        return editor.show_status(tr("The file isn’t in a Git repository").into(), cx);
    };
    editor.blame.annotations = Some(Annotations {
        repo_index,
        repo,
        relative,
        commits: Arc::default(),
        labels: Vec::new(),
        shades: Vec::new(),
        lines: Vec::new(),
        author_chars: 0,
        loaded: false,
        version: 0,
        task: None,
        hovered: None,
        popup: None,
        popup_hovered: false,
        popup_task: None,
        messages: HashMap::new(),
        menu_commit: None,
    });
    editor.show_status(tr("Annotating…").into(), cx);
    request(editor, Duration::ZERO, cx);
}

/// Reads the annotations of the current text in the background after `delay`; a newer request
/// replaces this one.
fn request(editor: &mut Editor, delay: Duration, cx: &mut Context<Editor>) {
    let text = editor.document.text().clone();
    let Some(annotations) = editor.blame.annotations.as_mut() else {
        return;
    };
    annotations.version += 1;
    let version = annotations.version;
    let repo = annotations.repo.clone();
    let relative = annotations.relative.clone();
    annotations.task = Some(cx.spawn(async move |this, cx| {
        if !delay.is_zero() {
            cx.background_executor().timer(delay).await;
        }
        let result = cx
            .background_executor()
            .spawn(async move { flux_git::blame(&repo, &relative, Some(&text.to_string())) })
            .await;
        this.update(cx, |editor, cx| {
            let Some(annotations) = editor.blame.annotations.as_mut() else {
                return;
            };
            if annotations.version != version {
                return;
            }
            match result {
                Ok(blame) => {
                    let first = !annotations.loaded;
                    annotations.set(blame);
                    if first {
                        editor.show_status(SharedString::default(), cx);
                    }
                }
                // A file that isn't in HEAD (new, untracked) has nothing to annotate; a failure
                // after the annotations were shown keeps the old ones.
                Err(_) if !annotations.loaded => {
                    editor.blame.annotations = None;
                    editor.blame.column = None;
                    editor.show_status(
                        tr("No annotations: the file isn’t committed yet").into(),
                        cx,
                    );
                }
                Err(_) => {}
            }
            cx.notify();
        })
        .ok();
    }));
}

impl Annotations {
    fn set(&mut self, blame: flux_git::Blame) {
        self.labels = blame
            .commits
            .iter()
            .map(|commit| {
                (
                    date(commit.author_time).into(),
                    short_author(&commit.author).into(),
                )
            })
            .collect();
        self.author_chars = self
            .labels
            .iter()
            .map(|(_, author)| author.chars().count())
            .max()
            .unwrap_or(0);
        self.shades = shades(&blame.commits);
        self.commits = Arc::new(blame.commits);
        self.lines = blame.lines;
        self.loaded = true;
        self.messages.clear();
        self.popup = None;
        self.hovered = None;
    }
}

/// The document's text changed: the labels follow their lines now and are read again a moment
/// later.
pub fn text_changed(editor: &mut Editor, changes: &[TextChange], cx: &mut Context<Editor>) {
    let Some(annotations) = editor.blame.annotations.as_mut() else {
        return;
    };
    for change in changes {
        annotations.lines = shift_lines(&annotations.lines, &change.old_text, change.changes.ops());
    }
    annotations.popup = None;
    request(editor, REBLAME_DELAY, cx);
}

/// HEAD moved (a commit, a checkout): lines may have been committed — read again now.
pub fn head_moved(editor: &mut Editor, cx: &mut Context<Editor>) {
    if editor.blame.annotations.is_some() {
        request(editor, Duration::ZERO, cx);
    }
}

/// The document's path changed (Save As, a rename): the annotations were another file's.
pub fn path_changed(editor: &mut Editor, cx: &mut Context<Editor>) {
    if editor.blame.annotations.take().is_some() {
        editor.blame.column = None;
        cx.notify();
    }
}

/// The commit of each line after an edit (`ops` over `old`): lines that the edit touched are not
/// committed (until git says otherwise), inserted lines are new, deleted lines go.
fn shift_lines(lines: &[Option<usize>], old: &Rope, ops: &[Operation]) -> Vec<Option<usize>> {
    let mut shifted = Vec::with_capacity(lines.len() + 8);
    let at = |line: usize| lines.get(line).copied().flatten();
    let mut pos = 0;
    let mut line = 0;
    // The commit of the line being put together in the new text.
    let mut current = at(0);
    for op in ops {
        match op {
            Operation::Retain(n) => {
                let end = (pos + n).min(old.len_chars());
                let breaks = old.char_to_line(end) - old.char_to_line(pos);
                for _ in 0..breaks {
                    shifted.push(current);
                    line += 1;
                    current = at(line);
                }
                pos = end;
            }
            Operation::Delete(n) => {
                let end = (pos + n).min(old.len_chars());
                line += old.char_to_line(end) - old.char_to_line(pos);
                // Whole lines deleted (from a line start to a line start): the line after them is
                // untouched.
                let at_start = |at: usize| at == 0 || old.char(at - 1) == '\n';
                current = match at_start(pos) && at_start(end) && end < old.len_chars() {
                    true => at(line),
                    false => None,
                };
                pos = end;
            }
            Operation::Insert(text) => {
                current = None;
                for _ in text.matches('\n') {
                    shifted.push(None);
                }
            }
        }
    }
    // What the operations didn't cover is kept as it was.
    let rest = old.len_chars().saturating_sub(pos);
    if rest > 0 {
        let breaks = old.char_to_line(old.len_chars()) - old.char_to_line(pos);
        for _ in 0..breaks {
            shifted.push(current);
            line += 1;
            current = at(line);
        }
    }
    shifted.push(current);
    shifted
}

/// Each commit's shade by how new it is among the file's commits: the newest 1, the oldest
/// [`OLDEST_SHADE`].
fn shades(commits: &[BlameCommit]) -> Vec<f32> {
    let mut times: Vec<i64> = commits.iter().map(|commit| commit.author_time).collect();
    times.sort_unstable();
    times.dedup();
    let steps = times.len().saturating_sub(1).max(1) as f32;
    commits
        .iter()
        .map(|commit| {
            let rank = times.partition_point(|&time| time < commit.author_time) as f32;
            OLDEST_SHADE + (1. - OLDEST_SHADE) * rank / steps
        })
        .collect()
}

/// "2026-10-09" (UTC) from seconds since the epoch.
pub(crate) fn date(seconds: i64) -> String {
    let (year, month, day) = civil_date(seconds.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

/// "2026-10-09 14:05" (UTC).
fn date_time(seconds: i64) -> String {
    let minutes = seconds.rem_euclid(86_400) / 60;
    format!("{} {:02}:{:02}", date(seconds), minutes / 60, minutes % 60)
}

/// Year, month, day of a day number since 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_date(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The author as the column shows them: the name, cut with "…" when long.
fn short_author(author: &str) -> String {
    let author = author.trim();
    if author.chars().count() <= AUTHOR_MAX_CHARS {
        return author.to_string();
    }
    let mut short: String = author.chars().take(AUTHOR_MAX_CHARS - 1).collect();
    short.push('…');
    short
}

// --- The column ---

/// The width of the annotation column (0 without annotations): date, author, padding.
pub fn column_width(state: &BlameState, em: Pixels) -> Pixels {
    match &state.annotations {
        Some(annotations) => {
            let chars = DATE_CHARS + 2 + annotations.author_chars.max(6);
            // The labels are smaller than the code: their characters are narrower.
            em * (chars as f32 * LABEL_FONT_SIZE / theme::FONT_SIZE) + px(12.)
        }
        None => px(0.),
    }
}

/// The column of the visible lines.
#[derive(Default)]
pub struct BlamePaint {
    quads: Vec<PaintQuad>,
    labels: Vec<(ShapedLine, Point<Pixels>)>,
    hitbox: Option<Hitbox>,
    /// The column: a line scrolled half out of view is cut at its edge.
    bounds: Option<Bounds<Pixels>>,
}

impl BlamePaint {
    pub fn paint(&mut self, line_height: Pixels, window: &mut Window, cx: &mut App) {
        let Some(bounds) = self.bounds else {
            return;
        };
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for quad in self.quads.drain(..) {
                window.paint_quad(quad);
            }
            for (label, origin) in &self.labels {
                label.paint(*origin, line_height, window, cx).ok();
            }
        });
        if let Some(hitbox) = &self.hitbox {
            window.set_cursor_style(CursorStyle::PointingHand, hitbox);
        }
    }
}

/// Lays out the column for the visible lines `first..last` in `column`.
pub fn prepaint(
    state: &BlameState,
    layout: &LayoutCache,
    last_line: usize,
    column: Bounds<Pixels>,
    ui: &UiColors,
    window: &mut Window,
) -> BlamePaint {
    let mut paint = BlamePaint::default();
    let Some(annotations) = state.annotations.as_ref().filter(|a| a.loaded) else {
        return paint;
    };
    if column.size.width <= px(0.) {
        return paint;
    }
    let text_system = window.text_system().clone();
    let font = font(theme::code_font());
    let font_size = px(LABEL_FONT_SIZE);
    let run = |len: usize, color| TextRun {
        len,
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let first = layout.first_line;
    let line_height = layout.line_height;
    let date_x = column.left() + px(6.);
    let author_x = date_x
        + (column.size.width - px(12.))
            * ((DATE_CHARS + 2) as f32 / (DATE_CHARS + 2 + annotations.author_chars.max(6)) as f32);
    for line in first..last_line {
        let Some(commit) = annotations.lines.get(line).copied().flatten() else {
            continue;
        };
        let top = layout.line_top(line);
        let shade = annotations
            .shades
            .get(commit)
            .copied()
            .unwrap_or(OLDEST_SHADE);
        let background = if annotations.hovered == Some(commit) {
            ui.hover
        } else {
            UiColors::tint(ui.blame_recent, ui.blame_recent.a * shade)
        };
        paint.quads.push(fill(
            Bounds::new(
                point(column.left(), top),
                size(column.size.width, line_height),
            ),
            background,
        ));
        let starts_run =
            line == first || annotations.lines.get(line - 1).copied().flatten() != Some(commit);
        if !starts_run {
            continue;
        }
        let Some((date, author)) = annotations.labels.get(commit) else {
            continue;
        };
        let color = if annotations.hovered == Some(commit) {
            ui.foreground
        } else {
            ui.text_muted
        };
        let y = top + (line_height - font_size * 1.3) / 2.;
        for (text, x) in [(date, date_x), (author, author_x)] {
            let runs = [run(text.len(), color)];
            let shaped = text_system.shape_line(text.clone(), font_size, &runs, None);
            paint.labels.push((shaped, point(x, y)));
        }
    }
    paint.hitbox = Some(window.insert_hitbox(column, HitboxBehavior::Normal));
    paint.bounds = Some(column);
    paint
}

/// The line under a point of the column, and its commit.
fn hit(editor: &Editor, position: Point<Pixels>) -> Option<(usize, Option<usize>)> {
    let column = editor.blame.column?;
    if !column.contains(&position) {
        return None;
    }
    let layout = editor.layout.as_ref()?;
    let annotations = editor.blame.annotations.as_ref()?;
    let line = ((position.y - layout.origin.y) / layout.line_height).floor();
    if line < 0. {
        return None;
    }
    let line = line as usize;
    Some((line, annotations.lines.get(line).copied().flatten()))
}

// --- Mouse ---

/// A left click on an annotation: its commit in the log. `true` — the click was taken.
pub fn mouse_down(
    editor: &mut Editor,
    event: &MouseDownEvent,
    window: &mut Window,
    cx: &mut Context<Editor>,
) -> bool {
    let Some((_, commit)) = hit(editor, event.position) else {
        return false;
    };
    if let Some(commit) = commit {
        show_in_log(editor, commit, window, cx);
    }
    true
}

/// The mouse moved over the editor: the commit under it is highlighted, its popup comes after a
/// moment; off the column, the popup goes a moment later (unless the mouse is on it).
pub fn mouse_moved(editor: &mut Editor, event: &MouseMoveEvent, cx: &mut Context<Editor>) {
    let hit = hit(editor, event.position);
    let Some(annotations) = editor.blame.annotations.as_mut() else {
        return;
    };
    let (line, commit) = match hit {
        Some((line, Some(commit))) => (line, commit),
        _ => {
            if annotations.hovered.take().is_some() {
                cx.notify();
            }
            if annotations.popup.is_some() && annotations.popup_task.is_none() {
                annotations.popup_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(POPUP_HIDE_DELAY).await;
                    this.update(cx, |editor, cx| {
                        if let Some(annotations) = editor.blame.annotations.as_mut() {
                            annotations.popup_task = None;
                            if !annotations.popup_hovered && annotations.hovered.is_none() {
                                annotations.popup = None;
                                cx.notify();
                            }
                        }
                    })
                    .ok();
                }));
            }
            return;
        }
    };
    if annotations.hovered == Some(commit) {
        return;
    }
    annotations.hovered = Some(commit);
    cx.notify();
    if annotations
        .popup
        .as_ref()
        .is_some_and(|popup| popup.commit == commit)
    {
        return;
    }
    // Another commit: its popup replaces the shown one at once, or comes after a moment.
    let immediate = annotations.popup.is_some();
    annotations.popup = None;
    annotations.popup_task = Some(cx.spawn(async move |this, cx| {
        if !immediate {
            cx.background_executor().timer(POPUP_SHOW_DELAY).await;
        }
        this.update(cx, |editor, cx| {
            if let Some(annotations) = editor.blame.annotations.as_mut() {
                annotations.popup_task = None;
                if annotations.hovered == Some(commit) {
                    annotations.popup = Some(CommitPopup { commit, line });
                    load_message(editor, commit, cx);
                    cx.notify();
                }
            }
        })
        .ok();
    }));
}

/// Reads a commit's whole message for its popup (once).
fn load_message(editor: &mut Editor, commit: usize, cx: &mut Context<Editor>) {
    let Some(annotations) = editor.blame.annotations.as_ref() else {
        return;
    };
    if annotations.messages.contains_key(&commit) {
        return;
    }
    let Some(oid) = annotations.commits.get(commit).map(|c| c.oid.clone()) else {
        return;
    };
    let repo = annotations.repo.clone();
    let version = annotations.version;
    cx.spawn(async move |this, cx| {
        let details = cx
            .background_executor()
            .spawn(async move { flux_git::log::commit_details(&repo, &oid) })
            .await;
        let Ok(details) = details else {
            return;
        };
        this.update(cx, |editor, cx| {
            if let Some(annotations) = editor.blame.annotations.as_mut()
                && annotations.version == version
            {
                annotations.messages.insert(commit, details.message);
                cx.notify();
            }
        })
        .ok();
    })
    .detach();
}

/// The right button in the gutter: its menu at the mouse.
pub fn secondary_click(
    editor: &mut Editor,
    event: &MouseDownEvent,
    window: &mut Window,
    cx: &mut Context<Editor>,
) {
    if editor.message.is_some() || editor.read_only {
        return;
    }
    let Some(layout) = editor.layout.as_ref() else {
        return;
    };
    if event.position.x >= layout.text_bounds.left() {
        return;
    }
    let commit = hit(editor, event.position).and_then(|(_, commit)| commit);
    if let Some(annotations) = editor.blame.annotations.as_mut() {
        annotations.menu_commit = commit;
        annotations.popup = None;
    }
    let on = editor.blame.annotations.is_some();
    window.focus(&editor.focus_handle);
    let menu = cx.new(|cx| {
        let menu = ContextMenu::new(window, cx);
        match (on, commit) {
            (true, Some(_)) => menu
                .entry(tr("Copy Revision Number"), CopyAnnotationRevision)
                .entry(tr("Show Diff"), ShowAnnotationDiff)
                .entry(tr("Show in Git Log"), ShowAnnotationInLog)
                .separator()
                .entry(tr("Close Annotations"), CloseAnnotations),
            (true, None) => menu.entry(tr("Close Annotations"), CloseAnnotations),
            (false, _) => menu.entry(tr("Annotate with Git Blame"), git::Annotate),
        }
    });
    let focus = menu.focus_handle(cx);
    let subscriptions = [
        cx.subscribe_in(
            &menu,
            window,
            |editor, menu, _: &DismissEvent, window, cx| close_menu(editor, menu, window, cx),
        ),
        cx.on_focus_out(&focus, window, {
            let menu = menu.clone();
            move |editor, _, window, cx| close_menu(editor, &menu, window, cx)
        }),
    ];
    window.focus(&focus);
    editor.blame.menu = Some(GutterMenu {
        menu,
        position: event.position,
        _subscriptions: subscriptions,
    });
    cx.notify();
}

fn close_menu(
    editor: &mut Editor,
    menu: &Entity<ContextMenu>,
    window: &mut Window,
    cx: &mut Context<Editor>,
) {
    if editor
        .blame
        .menu
        .as_ref()
        .is_none_or(|open| open.menu != *menu)
    {
        return;
    }
    let had_focus = menu.focus_handle(cx).contains_focused(window, cx);
    editor.blame.menu = None;
    if had_focus {
        window.focus(&editor.focus_handle);
    }
    cx.notify();
}

// --- Commands on a commit ---

fn commit_oid(editor: &Editor, commit: usize) -> Option<(usize, String)> {
    let annotations = editor.blame.annotations.as_ref()?;
    let oid = annotations.commits.get(commit)?.oid.clone();
    Some((annotations.repo_index, oid))
}

fn show_in_log(editor: &mut Editor, commit: usize, window: &mut Window, cx: &mut Context<Editor>) {
    if let Some((repo, oid)) = commit_oid(editor, commit) {
        if let Some(annotations) = editor.blame.annotations.as_mut() {
            annotations.popup = None;
        }
        window.dispatch_action(Box::new(git::ShowCommitInLog { repo, oid }), cx);
    }
}

fn copy_revision(editor: &mut Editor, commit: usize, cx: &mut Context<Editor>) {
    if let Some((_, oid)) = commit_oid(editor, commit) {
        cx.write_to_clipboard(ClipboardItem::new_string(oid));
        editor.show_status(tr("Revision number copied").into(), cx);
    }
}

/// The diff of the file in a commit: its parent ↔ the commit.
fn show_diff(editor: &mut Editor, commit: usize, window: &mut Window, cx: &mut Context<Editor>) {
    let Some(annotations) = editor.blame.annotations.as_ref() else {
        return;
    };
    let Some(blamed) = annotations.commits.get(commit) else {
        return;
    };
    let Some(path) = editor.document.path().map(PathBuf::from) else {
        return;
    };
    let short: String = blamed.oid.chars().take(8).collect();
    let action = git::OpenCompareDiff {
        repo: annotations.repo_index,
        path,
        left: DiffSide::Revision {
            rev: format!("{}^", blamed.oid),
            path: blamed.path.clone(),
            label: format!("{short}^"),
        },
        right: DiffSide::Revision {
            rev: blamed.oid.clone(),
            path: blamed.path.clone(),
            label: short,
        },
    };
    window.dispatch_action(Box::new(action), cx);
}

/// The editor's annotation actions: those of the gutter's menu (they act on the commit it was
/// opened on).
pub fn actions(root: Div, editor: &Editor, cx: &mut Context<Editor>) -> Div {
    let Some(annotations) = &editor.blame.annotations else {
        return root;
    };
    let root = root.on_action(cx.listener(|editor, _: &CloseAnnotations, _, cx| {
        if editor.blame.annotations.is_some() {
            toggle(editor, cx);
        }
    }));
    if annotations.menu_commit.is_none() {
        return root;
    }
    let commit = move |editor: &Editor| editor.blame.annotations.as_ref()?.menu_commit;
    root.on_action(
        cx.listener(move |editor, _: &CopyAnnotationRevision, _, cx| {
            if let Some(commit) = commit(editor) {
                copy_revision(editor, commit, cx);
            }
        }),
    )
    .on_action(
        cx.listener(move |editor, _: &ShowAnnotationDiff, window, cx| {
            if let Some(commit) = commit(editor) {
                show_diff(editor, commit, window, cx);
            }
        }),
    )
    .on_action(
        cx.listener(move |editor, _: &ShowAnnotationInLog, window, cx| {
            if let Some(commit) = commit(editor) {
                show_in_log(editor, commit, window, cx);
            }
        }),
    )
}

// --- Popups ---

/// The gutter's menu and the commit popup, over the editor.
pub fn render(editor: &Editor, window: &mut Window, cx: &mut Context<Editor>) -> Vec<AnyElement> {
    let mut elements = Vec::new();
    if let Some(menu) = &editor.blame.menu {
        elements.push(ContextMenu::overlay(&menu.menu, menu.position));
    }
    if let Some(popup) = render_popup(editor, window, cx) {
        elements.push(popup);
    }
    elements
}

fn render_popup(
    editor: &Editor,
    window: &mut Window,
    cx: &mut Context<Editor>,
) -> Option<AnyElement> {
    let annotations = editor.blame.annotations.as_ref()?;
    let popup = annotations.popup.as_ref()?;
    let column = editor.blame.column?;
    let layout = editor.layout.as_ref()?;
    let commit = annotations.commits.get(popup.commit)?;
    let ui = Theme::ui(cx);
    let top = layout.line_top(popup.line);
    if top < column.top() || top > column.bottom() {
        return None;
    }
    let index = popup.commit;
    let message = annotations
        .messages
        .get(&index)
        .cloned()
        .unwrap_or_else(|| commit.summary.clone());
    let short: String = commit.oid.chars().take(8).collect();
    let link = |id: &'static str, label: &'static str| {
        div()
            .id(id)
            .cursor_pointer()
            .text_color(ui.accent_text)
            .hover(|style| style.underline())
            .child(label)
    };
    let header = div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .font_family(theme::code_font())
                .text_color(ui.accent_text)
                .child(short),
        )
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(ui.foreground)
                .child(commit.author.clone()),
        )
        .child(
            div()
                .flex_1()
                .text_color(ui.dim)
                .child(date_time(commit.author_time)),
        );
    let email = (!commit.author_email.is_empty()).then(|| {
        div()
            .text_size(px(theme::TEXT_SM))
            .text_color(ui.dim)
            .child(commit.author_email.clone())
    });
    let path_note = (commit.path != annotations.relative).then(|| {
        div()
            .text_size(px(theme::TEXT_SM))
            .text_color(ui.dim)
            .child(trf("In {0}", &[&commit.path]))
    });
    let body = div()
        .id("blame-message")
        .max_h(px(220.))
        .overflow_y_scroll()
        .text_color(ui.foreground)
        .whitespace_normal()
        .child(message);
    let footer = div()
        .flex()
        .gap_3()
        .text_size(px(theme::TEXT_SM))
        .child(
            link("blame-show-in-log", tr("Show in Git Log")).on_click(cx.listener(
                move |editor, _: &ClickEvent, window, cx| show_in_log(editor, index, window, cx),
            )),
        )
        .child(
            link("blame-show-diff", tr("Show Diff")).on_click(cx.listener(
                move |editor, _: &ClickEvent, window, cx| {
                    if let Some(annotations) = editor.blame.annotations.as_mut() {
                        annotations.popup = None;
                    }
                    show_diff(editor, index, window, cx)
                },
            )),
        )
        .child(link("blame-copy", tr("Copy Revision Number")).on_click(
            cx.listener(move |editor, _: &ClickEvent, _, cx| copy_revision(editor, index, cx)),
        ));
    let panel = crate::popup::panel(ui)
        .id("blame-popup")
        .occlude()
        .w(px(POPUP_WIDTH))
        .p_3()
        .flex()
        .flex_col()
        .gap_1p5()
        .on_hover(cx.listener(|editor, hovered: &bool, _, cx| {
            if let Some(annotations) = editor.blame.annotations.as_mut() {
                annotations.popup_hovered = *hovered;
                if !*hovered && annotations.hovered.is_none() {
                    annotations.popup = None;
                    cx.notify();
                }
            }
        }))
        .on_mouse_down_out(cx.listener(|editor, _: &MouseDownEvent, _, cx| {
            if let Some(annotations) = editor.blame.annotations.as_mut()
                && annotations.popup.take().is_some()
            {
                cx.notify();
            }
        }))
        .child(header)
        .children(email)
        .children(path_note)
        .child(div().h(px(1.)).bg(ui.divider))
        .child(body)
        .child(footer);
    let _ = window;
    Some(
        deferred(
            anchored()
                .anchor(Corner::TopLeft)
                .position(point(column.right() + px(POPUP_GAP), top))
                .snap_to_window_with_margin(px(WINDOW_MARGIN))
                .child(panel),
        )
        .with_priority(1)
        .into_any_element(),
    )
}

// --- The window's actions ---

/// The window-level actions: `Annotate`, `ShowFileHistory`, `ShowSelectionHistory`.
pub fn workspace_actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(cx.listener(
        |this, _: &git::Annotate, _, cx| match this.active_editor() {
            Some(editor) => editor.update(cx, toggle),
            None => this.show_message(tr("Open a file to annotate it").into(), cx),
        },
    ))
    .on_action(cx.listener(
        |this, _: &git::ShowFileHistory, window, cx| match this.active_path(cx) {
            Some(path) => {
                window.dispatch_action(Box::new(git::ShowHistory { path, lines: None }), cx)
            }
            None => this.show_message(tr("Open a file to see its history").into(), cx),
        },
    ))
    .on_action(
        cx.listener(|this, _: &git::ShowSelectionHistory, window, cx| {
            let Some(editor) = this.active_editor() else {
                return this.show_message(tr("Open a file to see its history").into(), cx);
            };
            match selection_history(&editor, cx) {
                Ok(action) => window.dispatch_action(Box::new(action), cx),
                Err(message) => this.show_message(message.into(), cx),
            }
        }),
    )
}

/// The history of the lines the primary selection covers (1-based, within the file as HEAD has it).
fn selection_history(editor: &Entity<Editor>, cx: &App) -> Result<git::ShowHistory, &'static str> {
    let editor = editor.read(cx);
    let path = editor
        .document
        .path()
        .map(PathBuf::from)
        .ok_or_else(|| tr("Open a file to see its history"))?;
    let text = editor.document.text();
    let range = editor.document.selection().primary();
    let (from, to) = (range.from(), range.to());
    let start = text.char_to_line(from);
    let mut end = text.char_to_line(to);
    // A selection that ends at the start of a line doesn't take that line.
    if end > start && line_start(text, end) == to {
        end -= 1;
    }
    let (start, end) = clamp_lines(start as u32 + 1, end as u32 + 1, editor.git.base.as_deref())
        .ok_or_else(|| tr("The selected lines aren’t in the last commit"))?;
    Ok(git::ShowHistory {
        path,
        lines: Some((start, end)),
    })
}

/// Lines `start..=end` (1-based) cut to the file as HEAD has it (`base`; unknown — as they are);
/// `None` — none of them is there.
fn clamp_lines(start: u32, end: u32, base: Option<&str>) -> Option<(u32, u32)> {
    let Some(base) = base else {
        return Some((start, end));
    };
    let count = base.lines().count().max(1) as u32;
    (start <= count).then(|| (start, end.min(count)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::ChangeSet;

    fn shifted(
        lines: &[Option<usize>],
        text: &str,
        change: (usize, usize, Option<&str>),
    ) -> Vec<Option<usize>> {
        let old = Rope::from_str(text);
        let (from, to, insert) = change;
        let changes =
            ChangeSet::from_changes(old.len_chars(), [(from, to, insert.map(str::to_string))]);
        shift_lines(lines, &old, changes.ops())
    }

    #[test]
    fn labels_follow_their_lines_through_edits() {
        let lines = [Some(0), Some(1), Some(2)];
        let text = "aa\nbb\ncc";
        // A new line typed at the end of the first one: it is new, the first is touched.
        assert_eq!(
            shifted(&lines, text, (2, 2, Some("\nxx"))),
            [None, None, Some(1), Some(2)]
        );
        // The second line deleted with its break.
        assert_eq!(shifted(&lines, text, (3, 6, None)), [Some(0), Some(2)]);
        // A character typed in the last line.
        assert_eq!(
            shifted(&lines, text, (7, 7, Some("z"))),
            [Some(0), Some(1), None]
        );
        // Two lines joined.
        assert_eq!(shifted(&lines, text, (2, 3, None)), [None, Some(2)]);
    }

    #[test]
    fn dates_are_civil() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_791_504_000), "2026-10-09");
        assert_eq!(date(951_782_400), "2000-02-29");
        assert_eq!(
            date_time(1_791_504_000 + 14 * 3600 + 5 * 60),
            "2026-10-09 14:05"
        );
    }

    #[test]
    fn newer_commits_are_brighter() {
        let commit = |time| BlameCommit {
            oid: String::new(),
            author: String::new(),
            author_email: String::new(),
            author_time: time,
            summary: String::new(),
            path: String::new(),
        };
        let shades = shades(&[commit(30), commit(10), commit(20)]);
        assert_eq!(shades[0], 1.);
        assert_eq!(shades[1], OLDEST_SHADE);
        assert!(shades[2] > shades[1] && shades[2] < shades[0]);
    }

    #[test]
    fn long_authors_are_cut() {
        assert_eq!(short_author("Egor"), "Egor");
        assert_eq!(
            short_author("Someone With A Long Name").chars().count(),
            AUTHOR_MAX_CHARS
        );
    }

    #[test]
    fn selection_history_stays_within_the_committed_file() {
        assert_eq!(clamp_lines(2, 9, Some("a\nb\nc\n")), Some((2, 3)));
        assert_eq!(clamp_lines(5, 9, Some("a\nb\n")), None);
        assert_eq!(clamp_lines(5, 9, None), Some((5, 9)));
    }
}
