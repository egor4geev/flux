//! Project search (cmd-shift-f): an overlay window on top of the islands in the spirit of JetBrains
//! "Find in Files": a query field with toggles (case, whole word, regular expression), results
//! grouped by file (a group collapses when its header is clicked), and a preview of the selected
//! match.
//!
//! The search runs in the background (`flux_search::search_project`) after a short pause in typing.
//! Results arrive as a stream, one file at a time, and are inserted into the list in path order. A
//! new query cancels the previous search: it sets its cancellation flag and drops its task, so late
//! results never reach the window. The preview is a real read-only editor (`Editor::preview`): the
//! file is read from disk in the background, matches in it are highlighted, and the line of the
//! selected match is in the middle. The window asks the Workspace to open a match
//! ([`ProjectSearchEvent::Open`]): the Workspace opens the file and selects the spot in it, and the
//! window closes.

use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use flux_core::Document;
use flux_search::{
    FileMatches, GrepOptions, GrepSummary, LineMatch, QueryError, SearchQuery, search_project,
};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    Animation, AnimationExt, AnyElement, App, ClickEvent, Context, Entity, EventEmitter,
    FocusHandle, Focusable, FontWeight, HighlightStyle, KeyBinding, Render, ScrollStrategy,
    SharedString, StyledText, Subscription, Task, UniformListScrollHandle, Window, actions, div,
    prelude::*, px, relative, uniform_list,
};

use crate::editor::Editor;
use crate::i18n::{tr, trf, trn};
use crate::icons::{IconName, file_icon, icon};
use crate::input::{InputEvent, TextInput};
use crate::theme::{self, Theme, UiColors};
use crate::ui;
use crate::workspace::Location;

/// Pause after the query changes: a word typed in one go triggers a single search.
const SEARCH_DELAY: Duration = Duration::from_millis(150);
/// A list row: the file header and a line with matches have the same height (`uniform_list`).
const ROW_HEIGHT: f32 = 26.;
/// The line-number column (the number is right-aligned) and the indent before it: the number sits
/// under the file icon.
const LINE_NUMBER_WIDTH: f32 = 40.;
const LINE_INDENT: f32 = 10.;
/// The share of the window height given to the preview.
const PREVIEW_HEIGHT: f32 = 0.46;
const PREVIEW_HEADER_HEIGHT: f32 = 34.;
/// Larger files are not shown in the preview: reading and parsing them would take a noticeable
/// amount of time.
const MAX_PREVIEW_BYTES: u64 = 2 * 1024 * 1024;
/// PageUp/PageDown step: this many matches.
const PAGE_MATCHES: usize = 10;
/// The status does not stretch any wider: long text is only partly visible.
const STATUS_MAX_WIDTH: f32 = 420.;

actions!(
    project_search,
    [
        Toggle,
        Close,
        SelectNextMatch,
        SelectPreviousMatch,
        SelectNextPage,
        SelectPreviousPage,
        OpenMatch,
        FocusEditor,
        ToggleCaseSensitive,
        ToggleWholeWord,
        ToggleRegex,
    ]
);

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-shift-f", Toggle, Some("Workspace"))]);
    // The window context encloses the query field, so the arrow keys and Enter work right while
    // typing.
    let context = Some("ProjectSearch");
    cx.bind_keys([
        KeyBinding::new("down", SelectNextMatch, context),
        KeyBinding::new("up", SelectPreviousMatch, context),
        KeyBinding::new("pagedown", SelectNextPage, context),
        KeyBinding::new("pageup", SelectPreviousPage, context),
        KeyBinding::new("enter", OpenMatch, context),
        KeyBinding::new("cmd-enter", OpenMatch, context),
        KeyBinding::new("escape", Close, context),
        KeyBinding::new("alt-cmd-c", ToggleCaseSensitive, context),
        KeyBinding::new("alt-cmd-w", ToggleWholeWord, context),
        KeyBinding::new("alt-cmd-r", ToggleRegex, context),
    ]);
}

/// What the window asks of the Workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectSearchEvent {
    /// Open the file and select the match; `focus` moves focus to the editor.
    Open { location: Location, focus: bool },
    /// Return focus to the editor (Esc, the window has closed).
    FocusEditor,
}

/// A row of the results list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// A file header (index into `results`).
    File(usize),
    /// A line with matches: the file and the line within it.
    Line { file: usize, line: usize },
}

/// Search state, for the status in the header and for the empty list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    /// The query is empty.
    Idle,
    NoRoot,
    Searching,
    Done(GrepSummary),
    Error(String),
}

/// Preview of the selected match: its file in a read-only editor.
#[derive(Default)]
struct Preview {
    /// The file being shown (path relative to the root) and its editor.
    shown: Option<(PathBuf, Entity<Editor>)>,
    /// The file currently being read; while it is being read, the previous one stays visible.
    loading: Option<PathBuf>,
    /// A message in place of the file: it is too large or could not be read.
    message: Option<(PathBuf, SharedString)>,
    /// Background read; a new read replaces (cancels) the previous one.
    _task: Option<Task<()>>,
}

impl Preview {
    fn is_empty(&self) -> bool {
        self.shown.is_none() && self.loading.is_none() && self.message.is_none()
    }
}

/// The project search window: one per Workspace; the query and results persist while it is hidden.
pub struct ProjectSearch {
    root: Option<PathBuf>,
    open: bool,
    query: Entity<TextInput>,
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
    /// Files with matches, in path order.
    results: Vec<FileMatches>,
    /// Collapsed files (paths relative to the root): only their headers are visible.
    collapsed: HashSet<PathBuf>,
    /// Flat list for rendering: a file header, then its rows (for expanded files).
    rows: Vec<Row>,
    /// How many matches and files have arrived so far, for the status during the search.
    found: (usize, usize),
    /// The selected match row (index into `rows`).
    selected: Option<usize>,
    /// The user picked the row (arrows, click): when new results arrive, the selection stays on it.
    /// Otherwise the first match in the list is selected.
    selection_pinned: bool,
    status: Status,
    /// Cancellation flag of the search in progress.
    cancel: Arc<AtomicBool>,
    /// The pause, the search, and receiving the results; dropping the task cancels it.
    search: Option<Task<()>>,
    scroll: UniformListScrollHandle,
    preview: Preview,
    /// The query as a single line, for the explanations in the empty list (updated on render).
    last_query: String,
    _subscription: Subscription,
}

impl EventEmitter<ProjectSearchEvent> for ProjectSearch {}

impl ProjectSearch {
    pub fn new(root: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            TextInput::new(tr("Search in project"), cx)
                .code()
                .icon(IconName::Search)
        });
        let subscription = cx.subscribe_in(&query, window, |this, _, event, _, cx| match event {
            InputEvent::Changed => this.schedule_search(cx),
        });
        let status = if root.is_some() {
            Status::Idle
        } else {
            Status::NoRoot
        };
        Self {
            root,
            open: false,
            query,
            case_sensitive: false,
            whole_word: false,
            regex: false,
            results: Vec::new(),
            collapsed: HashSet::new(),
            rows: Vec::new(),
            found: (0, 0),
            selected: None,
            selection_pinned: false,
            status,
            cancel: Arc::new(AtomicBool::new(false)),
            search: None,
            scroll: UniformListScrollHandle::new(),
            preview: Preview::default(),
            last_query: String::new(),
            _subscription: subscription,
        }
    }

    /// New project root: the results are cleared; an open window with a query searches again.
    pub fn set_root(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        self.root = root;
        self.cancel_search();
        self.clear_results();
        self.preview = Preview::default();
        self.status = if self.root.is_some() {
            Status::Idle
        } else {
            Status::NoRoot
        };
        if self.open {
            self.schedule_search(cx);
        }
        cx.notify();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// cmd-shift-f: opens a closed window and moves focus to the query field (`seed` is the text
    /// for it, e.g. the selection); focuses an open window that does not have focus; closes a
    /// focused one.
    pub fn toggle(&mut self, seed: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let input_focus = self.query.focus_handle(cx);
        if self.open && input_focus.is_focused(window) {
            return self.close(cx);
        }
        let reopened = !self.open;
        self.open = true;
        let current = self.query.read(cx).text();
        match seed {
            // The new text starts the search by itself (`InputEvent::Changed`).
            Some(seed) if seed != current => {
                self.query.update(cx, |query, cx| query.set_text(&seed, cx));
            }
            // Files may have changed while the window was hidden.
            _ if reopened && !current.is_empty() => self.schedule_search(cx),
            _ => {}
        }
        self.query.update(cx, |query, cx| query.select_all(cx));
        window.focus(&input_focus);
        cx.notify();
    }

    /// Esc (in the window, or in the editor when there is nothing to clear there), ×, cmd-shift-f
    /// from the field, opening a match: the window hides and focus goes to the editor.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        cx.emit(ProjectSearchEvent::FocusEditor);
        cx.notify();
    }

    /// Hides the window without returning focus: a click outside the window, another window on top
    /// (the Workspace manages focus).
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        cx.notify();
    }

    // --- Search ---

    fn search_query(&self, cx: &App) -> SearchQuery {
        SearchQuery {
            text: self.query.read(cx).text(),
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            regex: self.regex,
        }
    }

    /// The query or a toggle changed: the previous search is canceled immediately, and the new one
    /// starts after a pause.
    fn schedule_search(&mut self, cx: &mut Context<Self>) {
        self.cancel_search();
        let query = self.search_query(cx);
        let Some(root) = self.root.clone() else {
            self.clear_results();
            self.preview = Preview::default();
            self.status = Status::NoRoot;
            return cx.notify();
        };
        if query.is_empty() {
            self.clear_results();
            self.preview = Preview::default();
            self.status = Status::Idle;
            return cx.notify();
        }
        let cancel = self.cancel.clone();
        self.search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DELAY).await;
            let started = this.update(cx, |this, cx| {
                this.clear_results();
                this.status = Status::Searching;
                cx.notify();
            });
            if started.is_err() {
                return;
            }
            let (sender, mut receiver) = mpsc::unbounded();
            let search = cx.background_executor().spawn(async move {
                search_project(&root, &query, &GrepOptions::default(), &cancel, |file| {
                    sender.unbounded_send(file).ok();
                })
            });
            // The channel closes when the search finishes (the sender is dropped).
            while let Some(file) = receiver.next().await {
                let mut batch = vec![file];
                while let Ok(file) = receiver.try_recv() {
                    batch.push(file);
                }
                let added = this.update(cx, |this, cx| {
                    this.add_files(batch, cx);
                    cx.notify();
                });
                if added.is_err() {
                    return;
                }
            }
            let summary = search.await;
            this.update(cx, |this, cx| {
                this.finish(summary, cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn cancel_search(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.search = None;
    }

    /// Clears the results. The preview stays until new results arrive, so it does not flicker while
    /// the search is running.
    fn clear_results(&mut self) {
        self.results.clear();
        self.collapsed.clear();
        self.rows.clear();
        self.found = (0, 0);
        self.selected = None;
        self.selection_pinned = false;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    /// A batch of files from the search: they are inserted in path order; the selection stays on
    /// the same row (if the user made it) or moves to the first match.
    fn add_files(&mut self, files: Vec<FileMatches>, cx: &mut Context<Self>) {
        let pinned = self
            .selected_match()
            .filter(|_| self.selection_pinned)
            .map(|(file, line)| (self.results[file].path.clone(), line));
        for file in files {
            self.found.0 += match_count(&file);
            self.found.1 += 1;
            insert_sorted(&mut self.results, file);
        }
        self.rows = flatten(&self.results, &self.collapsed);
        self.selected = match pinned {
            Some((path, line)) => self
                .results
                .iter()
                .position(|file| file.path == path)
                .and_then(|file| row_of(&self.rows, file, line)),
            None => next_match_row(&self.rows, None, true),
        };
        self.sync_preview(cx);
    }

    fn finish(&mut self, summary: Result<GrepSummary, QueryError>, cx: &mut Context<Self>) {
        self.status = match summary {
            Ok(summary) => Status::Done(summary),
            Err(error) => Status::Error(error.message),
        };
        self.sync_preview(cx);
    }

    // --- Selection, collapsing and opening ---

    fn selected_match(&self) -> Option<(usize, usize)> {
        match self.rows.get(self.selected?)? {
            Row::Line { file, line } => Some((*file, *line)),
            Row::File(_) => None,
        }
    }

    /// Selects the match `steps` steps forward or backward (at the edge of the list, the outermost
    /// one).
    fn select_match(&mut self, forward: bool, steps: usize, cx: &mut Context<Self>) {
        let Some(row) = step_match_rows(&self.rows, self.selected, forward, steps) else {
            return;
        };
        self.select_row(row, cx);
        // Scrolls by the minimum amount: down, the row ends up at the bottom edge; up, at the top
        // edge, together with the file header if the row is the first one in its file.
        if forward {
            self.scroll.scroll_to_item(row, ScrollStrategy::Bottom);
        } else {
            let top = match row.checked_sub(1).map(|header| self.rows[header]) {
                Some(Row::File(_)) => row - 1,
                _ => row,
            };
            self.scroll.scroll_to_item(top, ScrollStrategy::Top);
        }
    }

    /// The user selected a row (arrows, click): the selection is pinned and the preview follows it.
    fn select_row(&mut self, row: usize, cx: &mut Context<Self>) {
        self.selected = Some(row);
        self.selection_pinned = true;
        self.sync_preview(cx);
        cx.notify();
    }

    /// A click on a file header: collapses or expands its rows. A selection in a collapsed file
    /// moves to the nearest visible match.
    fn toggle_file(&mut self, file: usize, cx: &mut Context<Self>) {
        let selected = self.selected_match();
        let path = self.results[file].path.clone();
        if !self.collapsed.remove(&path) {
            self.collapsed.insert(path);
        }
        self.rows = flatten(&self.results, &self.collapsed);
        self.selected = selected.and_then(|(selected_file, line)| {
            row_of(&self.rows, selected_file, line).or_else(|| fallback_row(&self.rows, file))
        });
        self.sync_preview(cx);
        cx.notify();
    }

    /// Opens the selected match in the editor and closes the window.
    fn open_selected(&mut self, cx: &mut Context<Self>) {
        let (Some(root), Some((file, line))) = (&self.root, self.selected_match()) else {
            return;
        };
        let location = location_for(root, &self.results[file], line);
        cx.emit(ProjectSearchEvent::Open {
            location,
            focus: true,
        });
        self.close(cx);
    }

    /// A click on a toggle or its shortcut: searches again and moves focus to the field.
    fn toggle_option(
        &mut self,
        option: fn(&mut Self) -> &mut bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = option(self);
        *value = !*value;
        self.schedule_search(cx);
        window.focus(&self.query.focus_handle(cx));
        cx.notify();
    }

    // --- Preview ---

    /// The preview follows the selection: for the same file, only the matches and the position are
    /// updated; for another file, it is read in the background (the previous one stays visible
    /// while it is being read). With no selection the preview is removed, except during a search in
    /// progress, when a selection is about to appear.
    fn sync_preview(&mut self, cx: &mut Context<Self>) {
        let Some((file, line)) = self.selected_match() else {
            if self.status != Status::Searching {
                self.preview = Preview::default();
            }
            return;
        };
        let path = self.results[file].path.clone();
        if let Some((shown, editor)) = &self.preview.shown
            && *shown == path
        {
            self.preview.loading = None;
            self.preview.message = None;
            let (matches, active) = preview_matches(&self.results[file], line);
            let document_line = self.results[file].lines[line].line;
            editor.update(cx, |editor, cx| {
                let ranges: Vec<_> = matches
                    .iter()
                    .map(|(line, columns)| {
                        editor.position(*line, columns.start)..editor.position(*line, columns.end)
                    })
                    .collect();
                // A line may have no matches (the window of a long line cut them off).
                let position = active
                    .and_then(|active| ranges.get(active))
                    .map_or_else(|| editor.position(document_line, 0), |r| r.start);
                editor.set_search_highlights(ranges, active, cx);
                editor.show_position(position, cx);
            });
            return;
        }
        let already = self.preview.loading.as_ref() == Some(&path)
            || self
                .preview
                .message
                .as_ref()
                .is_some_and(|(failed, _)| *failed == path);
        let Some(root) = self.root.clone().filter(|_| !already) else {
            return;
        };
        self.preview.loading = Some(path.clone());
        let absolute = root.join(&path);
        let read = cx
            .background_executor()
            .spawn(async move { read_preview(&absolute) });
        self.preview._task = Some(cx.spawn(async move |this, cx| {
            let result = read.await;
            this.update(cx, |this, cx| this.preview_loaded(path, result, cx))
                .ok();
        }));
    }

    fn preview_loaded(
        &mut self,
        path: PathBuf,
        result: Result<Document, SharedString>,
        cx: &mut Context<Self>,
    ) {
        // The selection has already moved to another file, so this read is no longer needed.
        if self.preview.loading.as_ref() != Some(&path) {
            return;
        }
        self.preview.loading = None;
        match result {
            Ok(document) => {
                let editor = cx.new(|cx| Editor::preview(document, cx));
                self.preview.message = None;
                self.preview.shown = Some((path, editor));
                self.sync_preview(cx);
            }
            Err(message) => self.preview.message = Some((path, message)),
        }
        cx.notify();
    }

    // --- Rendering ---

    fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let toggle = |id: &'static str,
                      name: IconName,
                      on: bool,
                      label: &'static str,
                      action: &dyn gpui::Action,
                      option: fn(&mut ProjectSearch) -> &mut bool,
                      cx: &mut Context<Self>| {
            ui::toggle_button(id, name, on, ui)
                .tooltip(ui::tooltip(label, ui::shortcut_for(action, window)))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.toggle_option(option, window, cx)
                }))
        };
        let title = div()
            .flex()
            .items_center()
            .gap_2()
            .child(icon(IconName::FindInFiles, ui.accent_text))
            .child(
                div()
                    .text_size(px(theme::TEXT_LG))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr("Find in Files")),
            );
        let close = ui::icon_button("close", IconName::Close, ui)
            .tooltip(ui::tooltip(tr("Close"), Some("⎋".into())))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close(cx)));
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap_3()
            .px_4()
            .pt_3()
            .pb_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(title)
                    .child(div().flex_1())
                    .child(self.render_status(ui))
                    .child(close),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().flex_1().min_w_0().mr_1().child(self.query.clone()))
                    .child(toggle(
                        "case",
                        IconName::CaseSensitive,
                        self.case_sensitive,
                        tr("Match Case"),
                        &ToggleCaseSensitive,
                        |this| &mut this.case_sensitive,
                        cx,
                    ))
                    .child(toggle(
                        "word",
                        IconName::WholeWord,
                        self.whole_word,
                        tr("Words"),
                        &ToggleWholeWord,
                        |this| &mut this.whole_word,
                        cx,
                    ))
                    .child(toggle(
                        "regex",
                        IconName::Regex,
                        self.regex,
                        tr("Regex"),
                        &ToggleRegex,
                        |this| &mut this.regex,
                        cx,
                    )),
            )
    }

    /// Status in the header: search in progress, how many matches were found, stopped at the limit.
    /// Errors and "no results" are shown large in place of the list.
    fn render_status(&self, ui: UiColors) -> AnyElement {
        let row = || {
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .max_w(px(STATUS_MAX_WIDTH))
                .whitespace_nowrap()
                .text_size(px(theme::TEXT_SM))
        };
        match &self.status {
            Status::Searching => {
                let (text, _) = status_text(&self.status, self.found);
                row()
                    .text_color(ui.text_muted)
                    .child(pulse(ui))
                    .child(text)
                    .into_any_element()
            }
            Status::Done(summary) if summary.truncated => row()
                .text_color(ui.warning)
                .child(icon(IconName::Warning, ui.warning).size(px(14.)))
                .child(status_text(&self.status, self.found).0)
                .into_any_element(),
            Status::Done(summary) if summary.matches > 0 => row()
                .text_color(ui.text_muted)
                .child(ui::badge(
                    trn(summary.matches, "{n} result", "{n} results"),
                    ui.accent_text,
                ))
                .child(trn(summary.files_matched, "in {n} file", "in {n} files"))
                .into_any_element(),
            _ => div().into_any_element(),
        }
    }

    /// The results list or, if there is nothing to show, a large explanation in the center.
    fn render_results(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        if let Some((name, title, detail, color)) = self.empty_state(ui) {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .px_8()
                .child(
                    div()
                        .size(px(44.))
                        .mb_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(ui::RADIUS_LG))
                        .bg(UiColors::tint(color, 0.12))
                        .child(icon(name, color).size(px(22.))),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_LG))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(ui.foreground)
                        .child(title),
                )
                .child(
                    div()
                        .max_w(px(560.))
                        .text_center()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.text_muted)
                        .child(detail),
                )
                .into_any_element();
        }
        uniform_list(
            "project-search-results",
            self.rows.len(),
            cx.processor(|this, range: Range<usize>, _, cx| {
                range
                    .map(|index| this.render_row(index, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.scroll.clone())
        .size_full()
        .into_any_element()
    }

    /// Icon, title, explanation, and color of the empty list; `None` means show the list.
    fn empty_state(&self, ui: UiColors) -> Option<(IconName, String, String, gpui::Hsla)> {
        if !self.rows.is_empty() {
            return None;
        }
        let query = &self.last_query;
        Some(match &self.status {
            Status::NoRoot => (
                IconName::Folder,
                tr("No project folder").into(),
                tr("Open a folder with ⌘O to search across its files.").into(),
                ui.folder,
            ),
            Status::Idle => (
                IconName::FindInFiles,
                tr("Search across the project").into(),
                format!(
                    "{} {}",
                    tr("Type text to find it in every file."),
                    tr(
                        "Match Case ⌥⌘C, Words ⌥⌘W and Regex ⌥⌘R narrow the search; ↑↓ pick a result, ↵ opens it."
                    )
                ),
                ui.accent_text,
            ),
            Status::Searching => (
                IconName::Search,
                tr("Searching…").into(),
                trf("Looking for “{0}” in the project files.", &[query]),
                ui.text_muted,
            ),
            Status::Done(_) => (
                IconName::Search,
                trf("No results for “{0}”", &[query]),
                tr("Check the spelling or turn off Match Case, Words and Regex.").into(),
                ui.text_muted,
            ),
            Status::Error(message) => (
                IconName::Error,
                tr("Invalid regular expression").into(),
                message.clone(),
                ui.error,
            ),
        })
    }

    fn render_row(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let row = div().id(index).w_full().h(px(ROW_HEIGHT)).px_1p5();
        match self.rows[index] {
            Row::File(file) => {
                let matches = &self.results[file];
                let collapsed = self.collapsed.contains(&matches.path);
                let (name, dir) = split_path(&matches.path);
                let chevron = if collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                };
                row.cursor_pointer()
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_file(file, cx)),
                    )
                    .child(
                        div()
                            .id(("file", index))
                            .size_full()
                            .px_1p5()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .rounded(px(ui::RADIUS_SM))
                            .whitespace_nowrap()
                            .hover(move |style| style.bg(ui.hover))
                            .child(icon(chevron, ui.dim).size(px(12.)))
                            .child(file_icon(&name, &ui).render().size(px(14.)))
                            .child(
                                div()
                                    .flex_none()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(ui.foreground)
                                    .child(name),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(theme::TEXT_SM))
                                    .text_color(ui.dim)
                                    .child(dir),
                            )
                            .child(div().flex_1())
                            .child(ui::badge(match_count(matches).to_string(), ui.text_muted)),
                    )
                    .into_any_element()
            }
            Row::Line { file, line } => {
                let selected = self.selected == Some(index);
                let found = &self.results[file].lines[line];
                let (text, ranges) = display_text(found);
                // In the selected row, matches use the color of the current match, as in the
                // preview.
                let style = HighlightStyle {
                    background_color: Some(if selected {
                        ui.search_match_active
                    } else {
                        ui.search_match
                    }),
                    ..Default::default()
                };
                row.on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    this.select_row(index, cx);
                    if event.click_count() >= 2 {
                        this.open_selected(cx);
                    }
                }))
                .child(
                    div()
                        .id(("line", index))
                        .size_full()
                        .pl(px(LINE_INDENT))
                        .pr_2()
                        .flex()
                        .items_center()
                        .gap_3()
                        .rounded(px(ui::RADIUS_SM))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .font_family(theme::code_font())
                        .text_size(px(theme::TEXT_MD))
                        .when(selected, |line| line.bg(ui.list_selected))
                        .when(!selected, |line| {
                            line.hover(move |style| style.bg(ui.hover))
                        })
                        .child(
                            div()
                                .flex_none()
                                .w(px(LINE_NUMBER_WIDTH))
                                .flex()
                                .justify_end()
                                .text_size(px(theme::TEXT_SM))
                                .text_color(if selected { ui.text_muted } else { ui.dim })
                                .child((found.line + 1).to_string()),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_color(ui.foreground)
                                .child(
                                    StyledText::new(text)
                                        .with_highlights(ranges.into_iter().map(|r| (r, style))),
                                ),
                        ),
                )
                .into_any_element()
            }
        }
    }

    /// Preview below the list: a header (file, line, match number) and a read-only editor.
    fn render_preview(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.preview.is_empty() {
            return None;
        }
        let ui = Theme::ui(cx);
        let selected = self.selected_match();
        let path = match (selected, &self.preview.shown) {
            (Some((file, _)), _) => self.results[file].path.clone(),
            (None, Some((path, _))) => path.clone(),
            (None, None) => self.preview.loading.clone().unwrap_or_default(),
        };
        let (name, dir) = split_path(&path);
        let line = selected.map(|(file, line)| self.results[file].lines[line].line + 1);
        let ordinal = selected.map(|(file, line)| match_ordinal(&self.results, file, line));
        let message = |text: SharedString, color| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(theme::TEXT_SM))
                .text_color(color)
                .child(text)
                .into_any_element()
        };
        let body = match (&self.preview.message, &self.preview.shown) {
            (Some((failed, text)), _) if *failed == path => message(text.clone(), ui.text_muted),
            (_, Some((shown, editor))) => div()
                .size_full()
                // While another file is being read, the previous one is shown dimmed.
                .when(*shown != path, |body| body.opacity(0.45))
                .child(editor.clone())
                .into_any_element(),
            _ => message(tr("Loading…").into(), ui.dim),
        };
        let header = div()
            .flex_none()
            .h(px(PREVIEW_HEADER_HEIGHT))
            .px_4()
            .flex()
            .items_center()
            .gap_2()
            .whitespace_nowrap()
            .text_size(px(theme::TEXT_SM))
            .child(file_icon(&name, &ui).render().size(px(14.)))
            .child(
                // The path on one line: the directory is dimmed, the name is brighter, the line
                // number is in the accent color.
                div()
                    .min_w_0()
                    .flex()
                    .overflow_hidden()
                    .when(!dir.is_empty(), |path| {
                        path.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(ui.dim)
                                .child(format!("{dir}/")),
                        )
                    })
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(ui.foreground)
                            .child(name),
                    )
                    .children(line.map(|line| {
                        div()
                            .flex_none()
                            .text_color(ui.accent_text)
                            .child(format!(":{line}"))
                    })),
            )
            .child(div().flex_1())
            .children(ordinal.map(|ordinal| {
                div()
                    .flex_none()
                    .text_color(ui.dim)
                    .child(trf("{0} of {1}", &[&ordinal, &self.found.0]))
            }));
        Some(
            div()
                .flex_none()
                .h(relative(PREVIEW_HEIGHT))
                .flex()
                .flex_col()
                .border_t_1()
                .border_color(ui.divider)
                .bg(ui.input_background)
                .child(header)
                .child(
                    div()
                        .id("preview")
                        .flex_1()
                        .min_h_0()
                        .pt_0p5()
                        .pb_1()
                        // Double-clicking the preview opens the match in the editor.
                        .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                            if event.click_count() >= 2 {
                                this.open_selected(cx);
                            }
                        }))
                        .child(body),
                )
                .into_any_element(),
        )
    }

    fn render_footer(&self, ui: UiColors) -> impl IntoElement + use<> {
        let root = self
            .root
            .as_deref()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned());
        div()
            .flex_none()
            .h(px(40.))
            .px_4()
            .flex()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(ui.divider)
            .child(ui::hint_bar(
                &[
                    ("↑↓", tr("select")),
                    ("↵", tr("open")),
                    ("PgUp PgDn", tr("page")),
                    ("esc", tr("close")),
                ],
                ui,
            ))
            .children(root.map(|root| {
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(tr("in"))
                    .child(icon(IconName::Folder, ui.folder).size(px(13.)))
                    .child(div().text_color(ui.text_muted).child(root))
            }))
    }
}

impl Render for ProjectSearch {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        self.last_query = single_line_label(&self.query.read(cx).text());
        let header = self.render_header(window, cx);
        let results = self.render_results(cx);
        let preview = self.render_preview(cx);
        ui::popover(ui)
            .key_context("ProjectSearch")
            .size_full()
            .flex()
            .flex_col()
            .on_action(
                cx.listener(|this, _: &SelectNextMatch, _, cx| this.select_match(true, 1, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectPreviousMatch, _, cx| this.select_match(false, 1, cx)),
            )
            .on_action(cx.listener(|this, _: &SelectNextPage, _, cx| {
                this.select_match(true, PAGE_MATCHES, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPreviousPage, _, cx| {
                this.select_match(false, PAGE_MATCHES, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenMatch, _, cx| this.open_selected(cx)))
            .on_action(
                cx.listener(|_, _: &FocusEditor, _, cx| cx.emit(ProjectSearchEvent::FocusEditor)),
            )
            .on_action(cx.listener(|this, _: &Close, _, cx| this.close(cx)))
            .on_action(cx.listener(|this, _: &ToggleCaseSensitive, window, cx| {
                this.toggle_option(|this| &mut this.case_sensitive, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleWholeWord, window, cx| {
                this.toggle_option(|this| &mut this.whole_word, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleRegex, window, cx| {
                this.toggle_option(|this| &mut this.regex, window, cx)
            }))
            .child(header)
            .child(ui::divider(ui))
            .child(div().flex_1().min_h_0().py_1p5().child(results))
            .children(preview)
            .child(self.render_footer(ui))
    }
}

impl Focusable for ProjectSearch {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl Drop for ProjectSearch {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// A pulsing "search in progress" dot.
fn pulse(ui: UiColors) -> impl IntoElement {
    div()
        .flex_none()
        .size(px(7.))
        .rounded(px(4.))
        .bg(ui.accent)
        .with_animation(
            "searching",
            Animation::new(Duration::from_millis(900)).repeat(),
            |dot, delta| dot.opacity(0.3 + 0.7 * (1. - (2. * delta - 1.).abs())),
        )
}

/// Reads a file for the preview (on a background thread); a file that is too large yields a message
/// instead of text.
fn read_preview(path: &Path) -> Result<Document, SharedString> {
    let size = std::fs::metadata(path)
        .map_err(|err| SharedString::from(err.to_string()))?
        .len();
    if size > MAX_PREVIEW_BYTES {
        let megabytes = size as f64 / (1024. * 1024.);
        return Err(trf(
            "{0} MB — too large to preview",
            &[&format!("{megabytes:.1}")],
        )
        .into());
    }
    Document::open(path).map_err(|err| err.to_string().into())
}

// --- Pure logic ---

/// Inserts a file in path order; returns its position.
fn insert_sorted(results: &mut Vec<FileMatches>, file: FileMatches) -> usize {
    let index = results.partition_point(|other| other.path < file.path);
    results.insert(index, file);
    index
}

/// A flat list: each file's header followed by its lines with matches, unless the file is
/// collapsed.
fn flatten(results: &[FileMatches], collapsed: &HashSet<PathBuf>) -> Vec<Row> {
    results
        .iter()
        .enumerate()
        .flat_map(|(file, matches)| {
            let lines = if collapsed.contains(&matches.path) {
                0
            } else {
                matches.lines.len()
            };
            std::iter::once(Row::File(file))
                .chain((0..lines).map(move |line| Row::Line { file, line }))
        })
        .collect()
}

/// The next (`forward`) or previous match row relative to `from`; with no selection, the first one
/// in the list. File headers are skipped; at the edge of the list, `None` (the selection stays).
fn next_match_row(rows: &[Row], from: Option<usize>, forward: bool) -> Option<usize> {
    let is_match = |index: &usize| matches!(rows[*index], Row::Line { .. });
    match (from, forward) {
        (None, _) => (0..rows.len()).find(is_match),
        (Some(from), true) => (from + 1..rows.len()).find(is_match),
        (Some(from), false) => (0..from.min(rows.len())).rev().find(is_match),
    }
}

/// Up to `steps` steps through the matches from `from`; at the edge of the list, the outermost
/// match. `None` means there is nowhere to step (the selection stays).
fn step_match_rows(
    rows: &[Row],
    from: Option<usize>,
    forward: bool,
    steps: usize,
) -> Option<usize> {
    let mut current = from;
    let mut moved = None;
    for _ in 0..steps.max(1) {
        match next_match_row(rows, current, forward) {
            Some(next) => {
                current = Some(next);
                moved = Some(next);
            }
            None => break,
        }
        if from.is_none() {
            break;
        }
    }
    moved
}

/// The list row with the matches of line `line` of file `file`, if it is visible.
fn row_of(rows: &[Row], file: usize, line: usize) -> Option<usize> {
    rows.iter().position(|row| *row == Row::Line { file, line })
}

/// Where the selection goes from a collapsed file `file`: to the first match after its header, or,
/// if there is nothing below, to the last one before it.
fn fallback_row(rows: &[Row], file: usize) -> Option<usize> {
    let header = rows.iter().position(|row| *row == Row::File(file))?;
    next_match_row(rows, Some(header), true).or_else(|| next_match_row(rows, Some(header), false))
}

/// The number of matches in a file.
fn match_count(file: &FileMatches) -> usize {
    file.lines.iter().map(|line| line.ranges.len().max(1)).sum()
}

/// The number (from one) of the first match in line `line` of file `file` among all those found: "3
/// of 72" in the preview; counted the same way as the total counter (`match_count`).
fn match_ordinal(results: &[FileMatches], file: usize, line: usize) -> usize {
    let before_files: usize = results[..file].iter().map(match_count).sum();
    let before_lines: usize = results[file].lines[..line]
        .iter()
        .map(|line| line.ranges.len().max(1))
        .sum();
    before_files + before_lines + 1
}

/// The file's matches for the preview, in order, as (line, columns in characters of the real line),
/// and the number of the current one: the first match in line `selected`.
fn preview_matches(
    file: &FileMatches,
    selected: usize,
) -> (Vec<(usize, Range<usize>)>, Option<usize>) {
    let mut matches = Vec::new();
    let mut active = None;
    for (index, found) in file.lines.iter().enumerate() {
        for (i, range) in found.ranges.iter().enumerate() {
            if index == selected && i == 0 {
                active = Some(matches.len());
            }
            let start = found.column_offset + range.start;
            let end = found.column_offset + range.end;
            matches.push((found.line, start..end));
        }
    }
    (matches, active)
}

/// Where to jump for a match row: to the first match in it.
fn location_for(root: &Path, file: &FileMatches, line: usize) -> Location {
    let found = &file.lines[line];
    let first = found.ranges.first().cloned().unwrap_or(0..0);
    Location {
        path: root.join(&file.path),
        line: found.line,
        start: found.column_offset + first.start,
        end: found.column_offset + first.end,
    }
}

/// The file name and directory (relative, `/`-separated) for the header.
fn split_path(path: &Path) -> (String, String) {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let dir = path
        .parent()
        .map(|dir| {
            dir.components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_default();
    (name, dir)
}

/// The match line for display: no leading indentation, tabs replaced with spaces, and the start of
/// the line cut off by the window prefixed with "…". Matches are byte ranges in this line.
fn display_text(found: &LineMatch) -> (String, Vec<Range<usize>>) {
    let indent = found.text.chars().take_while(|c| c.is_whitespace()).count();
    // Indentation is trimmed only up to the first match: the search may have matched the spaces
    // themselves.
    let skip = found.ranges.first().map_or(indent, |r| indent.min(r.start));
    let ellipsis = found.column_offset > 0;
    let mut text = String::with_capacity(found.text.len() + 3);
    if ellipsis {
        text.push('…');
    }
    let shift = usize::from(ellipsis);
    text.extend(
        found
            .text
            .chars()
            .skip(skip)
            .map(|c| if c == '\t' { ' ' } else { c }),
    );
    let ranges = found.ranges.iter().filter_map(|range| {
        let start = range.start.saturating_sub(skip) + shift;
        let end = range.end.saturating_sub(skip) + shift;
        (start < end).then_some(start..end)
    });
    let ranges = char_ranges_to_bytes(&text, ranges);
    (text, ranges)
}

/// Character ranges → byte ranges; ranges past the end of the line are clipped, empty ones are
/// dropped.
fn char_ranges_to_bytes(
    text: &str,
    ranges: impl IntoIterator<Item = Range<usize>>,
) -> Vec<Range<usize>> {
    let offsets: Vec<usize> = text
        .char_indices()
        .map(|(byte, _)| byte)
        .chain([text.len()])
        .collect();
    let last = offsets.len() - 1;
    ranges
        .into_iter()
        .map(|range| offsets[range.start.min(last)]..offsets[range.end.min(last)])
        .filter(|range| range.start < range.end)
        .collect()
}

/// The status text and the error flag. `found` holds the matches and files that arrived during the
/// search.
fn status_text(status: &Status, found: (usize, usize)) -> (SharedString, bool) {
    let text = match status {
        Status::Idle => String::new(),
        Status::NoRoot => tr("No project folder — open one with ⌘O").into(),
        Status::Searching if found.0 == 0 => tr("Searching…").into(),
        Status::Searching => trf("Searching… {0}", &[&results_label(found.0, found.1)]),
        Status::Done(summary) if summary.matches == 0 => tr("No results").into(),
        Status::Done(summary) if summary.truncated => trn(
            summary.matches,
            "Stopped at {n} result",
            "Stopped at {n} results",
        ),
        Status::Done(summary) => results_label(summary.matches, summary.files_matched),
        Status::Error(message) => return (message.clone().into(), true),
    };
    (text.into(), false)
}

/// «3 results in 2 files», «1 result in 1 file».
fn results_label(matches: usize, files: usize) -> String {
    format!(
        "{} {}",
        trn(matches, "{n} result", "{n} results"),
        trn(files, "in {n} file", "in {n} files")
    )
}

/// The query for the explanations: a single line, shortened if it is long.
fn single_line_label(query: &str) -> String {
    const MAX_CHARS: usize = 60;
    let line = query.lines().next().unwrap_or_default();
    if line.chars().count() <= MAX_CHARS {
        return line.to_string();
    }
    let mut short: String = line.chars().take(MAX_CHARS - 1).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A range without the `a..b` literal, for single-element arrays (clippy takes that for a
    /// mistake).
    fn span(start: usize, end: usize) -> Range<usize> {
        start..end
    }

    fn line(line: usize, text: &str, ranges: &[Range<usize>]) -> LineMatch {
        LineMatch {
            line,
            text: text.into(),
            column_offset: 0,
            ranges: ranges.to_vec(),
        }
    }

    fn file(path: &str, lines: usize) -> FileMatches {
        FileMatches {
            path: PathBuf::from(path),
            lines: (0..lines).map(|n| line(n, "x", &[span(0, 1)])).collect(),
        }
    }

    fn expanded() -> HashSet<PathBuf> {
        HashSet::new()
    }

    #[test]
    fn files_are_kept_in_path_order() {
        let mut results = Vec::new();
        for path in ["src/b.rs", "Cargo.toml", "src/a.rs", "README.md"] {
            insert_sorted(&mut results, file(path, 1));
        }
        let paths: Vec<_> = results.iter().map(|f| f.path.to_str().unwrap()).collect();
        assert_eq!(paths, ["Cargo.toml", "README.md", "src/a.rs", "src/b.rs"]);
        // A directory is compared by components: `src/x` comes before `src.rs`.
        let mut results = Vec::new();
        insert_sorted(&mut results, file("src.rs", 1));
        assert_eq!(insert_sorted(&mut results, file("src/x.rs", 1)), 0);
    }

    #[test]
    fn rows_are_headers_followed_by_lines() {
        let rows = flatten(&[file("a.rs", 2), file("b.rs", 1)], &expanded());
        assert_eq!(
            rows,
            [
                Row::File(0),
                Row::Line { file: 0, line: 0 },
                Row::Line { file: 0, line: 1 },
                Row::File(1),
                Row::Line { file: 1, line: 0 },
            ]
        );
    }

    #[test]
    fn collapsed_files_show_only_their_headers() {
        let collapsed = HashSet::from([PathBuf::from("a.rs")]);
        let rows = flatten(&[file("a.rs", 2), file("b.rs", 1)], &collapsed);
        assert_eq!(
            rows,
            [Row::File(0), Row::File(1), Row::Line { file: 1, line: 0 }]
        );
        // The arrows skip past a collapsed file.
        assert_eq!(next_match_row(&rows, None, true), Some(2));
        assert_eq!(next_match_row(&rows, Some(2), false), None);
    }

    #[test]
    fn selection_skips_headers_and_stops_at_the_edges() {
        let rows = flatten(&[file("a.rs", 2), file("b.rs", 1)], &expanded());
        assert_eq!(next_match_row(&rows, None, true), Some(1));
        assert_eq!(next_match_row(&rows, None, false), Some(1));
        assert_eq!(next_match_row(&rows, Some(1), true), Some(2));
        assert_eq!(next_match_row(&rows, Some(2), true), Some(4));
        assert_eq!(next_match_row(&rows, Some(4), true), None);
        assert_eq!(next_match_row(&rows, Some(4), false), Some(2));
        assert_eq!(next_match_row(&rows, Some(1), false), None);
        assert_eq!(next_match_row(&[], None, true), None);
    }

    #[test]
    fn pages_step_over_matches_and_stop_at_the_edges() {
        // Rows: 0 is a.rs, 1..=4 are its lines, 5 is b.rs, 6..=8 are its lines.
        let rows = flatten(&[file("a.rs", 4), file("b.rs", 3)], &expanded());
        assert_eq!(step_match_rows(&rows, Some(1), true, 3), Some(4));
        assert_eq!(step_match_rows(&rows, Some(1), true, 4), Some(6));
        assert_eq!(step_match_rows(&rows, Some(2), true, 10), Some(8));
        assert_eq!(step_match_rows(&rows, Some(7), false, 10), Some(1));
        assert_eq!(step_match_rows(&rows, Some(8), true, 10), None);
        // With no selection, the first match, however many steps are requested.
        assert_eq!(step_match_rows(&rows, None, true, 10), Some(1));
        assert_eq!(step_match_rows(&[], None, true, 10), None);
    }

    #[test]
    fn collapsing_the_selected_file_moves_the_selection() {
        let results = [file("a.rs", 2), file("b.rs", 1), file("c.rs", 1)];
        // b.rs is collapsed: the selection moves to the first match below, in c.rs.
        let collapsed = HashSet::from([PathBuf::from("b.rs")]);
        let rows = flatten(&results, &collapsed);
        assert_eq!(fallback_row(&rows, 1), row_of(&rows, 2, 0));
        // The last file is collapsed: there is nothing below, so the selection goes to the last
        // match above.
        let collapsed = HashSet::from([PathBuf::from("c.rs")]);
        let rows = flatten(&results, &collapsed);
        assert_eq!(fallback_row(&rows, 2), row_of(&rows, 1, 0));
        // Everything is collapsed: there is no selection.
        let collapsed: HashSet<_> = ["a.rs", "b.rs", "c.rs"].map(PathBuf::from).into();
        let rows = flatten(&results, &collapsed);
        assert_eq!(fallback_row(&rows, 0), None);
        assert_eq!(row_of(&rows, 0, 0), None);
    }

    #[test]
    fn preview_marks_every_match_and_the_selected_one() {
        let mut found = file("a.rs", 0);
        found.lines = vec![
            line(3, "foo foo", &[span(0, 3), span(4, 7)]),
            LineMatch {
                line: 9,
                text: "foo".into(),
                column_offset: 100,
                ranges: vec![span(0, 3)],
            },
        ];
        let (matches, active) = preview_matches(&found, 1);
        assert_eq!(
            matches,
            [(3, span(0, 3)), (3, span(4, 7)), (9, span(100, 103))]
        );
        assert_eq!(active, Some(2));
        // The current one is the first match of the selected line.
        assert_eq!(preview_matches(&found, 0).1, Some(0));
    }

    #[test]
    fn ordinal_counts_matches_before_the_line() {
        let mut first = file("a.rs", 0);
        first.lines = vec![
            line(0, "aa", &[span(0, 1), span(1, 2)]),
            line(5, "a", &[span(0, 1)]),
        ];
        let results = [first, file("b.rs", 3)];
        assert_eq!(match_ordinal(&results, 0, 0), 1);
        assert_eq!(match_ordinal(&results, 0, 1), 3);
        assert_eq!(match_ordinal(&results, 1, 2), 6);
    }

    #[test]
    fn location_points_at_the_first_match_in_the_real_line() {
        let mut found = file("src/main.rs", 0);
        found.lines.push(LineMatch {
            line: 41,
            text: "abc".into(),
            column_offset: 100,
            ranges: vec![3..5, 7..8],
        });
        let location = location_for(Path::new("/p"), &found, 0);
        assert_eq!(
            location,
            Location {
                path: PathBuf::from("/p/src/main.rs"),
                line: 41,
                start: 103,
                end: 105,
            }
        );
    }

    #[test]
    fn display_text_drops_indent_and_keeps_matches_aligned() {
        // "    let ы = foo;": matches "ы" (characters 8..9) and "foo" (12..15).
        let (text, ranges) = display_text(&line(0, "    let ы = foo;", &[8..9, 12..15]));
        assert_eq!(text, "let ы = foo;");
        assert_eq!(ranges, [4..6, 9..12]);
        assert_eq!(&text[ranges[1].clone()], "foo");
        // Tabs become spaces; the length in characters is the same.
        let (text, ranges) = display_text(&line(0, "\tx\ty", &[span(3, 4)]));
        assert_eq!(text, "x y");
        assert_eq!(&text[ranges[0].clone()], "y");
        // The search was for the spaces themselves: the indentation is trimmed only up to the
        // match.
        let (text, ranges) = display_text(&line(0, "    x", &[span(2, 4)]));
        assert_eq!(text, "  x");
        assert_eq!(ranges, [span(0, 2)]);
    }

    #[test]
    fn cut_line_start_gets_an_ellipsis() {
        let mut found = line(0, "abc foo", &[span(4, 7)]);
        found.column_offset = 120;
        let (text, ranges) = display_text(&found);
        assert_eq!(text, "…abc foo");
        assert_eq!(&text[ranges[0].clone()], "foo");
    }

    #[test]
    fn char_ranges_are_clipped_to_the_text() {
        assert_eq!(char_ranges_to_bytes("ab", [1..9, 5..6, 0..0]), [span(1, 2)]);
        assert_eq!(char_ranges_to_bytes("ыы", [span(1, 2)]), [span(2, 4)]);
    }

    #[test]
    fn status_describes_the_search() {
        let done = |matches, files_matched, truncated| {
            Status::Done(GrepSummary {
                matches,
                files_matched,
                truncated,
                ..Default::default()
            })
        };
        let text = |status: &Status, found| status_text(status, found).0.to_string();
        assert_eq!(text(&Status::Idle, (0, 0)), "");
        assert_eq!(text(&Status::Searching, (0, 0)), "Searching…");
        assert_eq!(
            text(&Status::Searching, (5, 2)),
            "Searching… 5 results in 2 files"
        );
        assert_eq!(text(&done(0, 0, false), (0, 0)), "No results");
        assert_eq!(text(&done(1, 1, false), (1, 1)), "1 result in 1 file");
        assert_eq!(text(&done(7, 3, false), (7, 3)), "7 results in 3 files");
        assert_eq!(
            text(&done(10_000, 40, true), (10_000, 40)),
            "Stopped at 10000 results"
        );
        let (message, is_error) = status_text(&Status::Error("unclosed group".into()), (0, 0));
        assert_eq!((message.as_ref(), is_error), ("unclosed group", true));
        assert!(!status_text(&Status::NoRoot, (0, 0)).1);
    }

    #[test]
    fn long_queries_are_shortened_for_messages() {
        assert_eq!(single_line_label("fn render"), "fn render");
        assert_eq!(single_line_label("a\nb"), "a");
        let long = "x".repeat(80);
        let short = single_line_label(&long);
        assert_eq!(short.chars().count(), 60);
        assert!(short.ends_with('…'));
    }

    #[test]
    fn paths_split_into_name_and_directory() {
        assert_eq!(
            split_path(Path::new("crates/app/src/main.rs")),
            ("main.rs".into(), "crates/app/src".into())
        );
        assert_eq!(
            split_path(Path::new("Cargo.toml")),
            ("Cargo.toml".into(), String::new())
        );
    }
}
