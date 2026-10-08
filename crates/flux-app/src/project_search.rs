//! Поиск по проекту (cmd-shift-f): панель внизу окна — поле запроса, переключатели
//! (регистр, целое слово, регулярное выражение) и результаты по файлам.
//!
//! Поиск идёт в фоне (`flux_search::search_project`) после короткой паузы в наборе.
//! Результаты приходят потоком, по файлу, и встают в список по порядку путей. Новый
//! запрос отменяет прошлый поиск: выставляет его флаг отмены и бросает его задачу — поздние
//! результаты до панели не доходят. Открыть вхождение просит Workspace
//! ([`ProjectSearchEvent::Open`]): он открывает файл и выделяет место в нём.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use flux_search::{
    FileMatches, GrepOptions, GrepSummary, LineMatch, QueryError, SearchQuery, search_project,
};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, EventEmitter, Focusable, HighlightStyle,
    KeyBinding, Render, ScrollStrategy, SharedString, StyledText, Subscription, Task,
    UniformListScrollHandle, Window, actions, div, prelude::*, px, relative, uniform_list,
};

use crate::input::{InputEvent, TextInput};
use crate::theme::{Theme, UiColors};
use crate::workspace::Location;

/// Пауза после изменения запроса: слово, набранное подряд, — один поиск.
const SEARCH_DELAY: Duration = Duration::from_millis(150);
/// Доля высоты окна под панель и нижняя граница высоты.
const PANEL_HEIGHT: f32 = 0.4;
const PANEL_MIN_HEIGHT: f32 = 160.;
const ROW_HEIGHT: f32 = 22.;
const TEXT_SIZE: f32 = 13.;
/// Колонка номеров строк (номер выровнен по правому краю).
const LINE_NUMBER_WIDTH: f32 = 48.;
/// Статус шире не растягивается: длинную ошибку регулярного выражения видно частично.
const STATUS_MAX_WIDTH: f32 = 420.;

actions!(
    project_search,
    [
        Toggle,
        Close,
        SelectNextMatch,
        SelectPreviousMatch,
        OpenMatch,
        OpenMatchAndFocus,
        FocusEditor,
        ToggleCaseSensitive,
        ToggleWholeWord,
        ToggleRegex,
    ]
);

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-shift-f", Toggle, Some("Workspace"))]);
    // Контекст панели объемлет поле запроса: стрелки и Enter работают прямо во время набора.
    let context = Some("ProjectSearch");
    cx.bind_keys([
        KeyBinding::new("down", SelectNextMatch, context),
        KeyBinding::new("up", SelectPreviousMatch, context),
        KeyBinding::new("enter", OpenMatch, context),
        KeyBinding::new("cmd-enter", OpenMatchAndFocus, context),
        KeyBinding::new("escape", Close, context),
        KeyBinding::new("alt-cmd-c", ToggleCaseSensitive, context),
        KeyBinding::new("alt-cmd-w", ToggleWholeWord, context),
        KeyBinding::new("alt-cmd-r", ToggleRegex, context),
    ]);
}

/// Что панель просит у Workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectSearchEvent {
    /// Открыть файл и выделить вхождение; `focus` — перевести фокус в редактор (иначе он
    /// остаётся в панели).
    Open { location: Location, focus: bool },
    /// Вернуть фокус в редактор (Esc в панели, панель закрылась).
    FocusEditor,
}

/// Строка списка результатов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// Заголовок файла (индекс в `results`).
    File(usize),
    /// Строка с вхождениями: файл и строка в нём.
    Line { file: usize, line: usize },
}

/// Состояние поиска — для строки статуса.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    /// Запрос пуст.
    Idle,
    NoRoot,
    Searching,
    Done(GrepSummary),
    Error(String),
}

/// Панель поиска по проекту: одна на Workspace; результаты живут, пока панель скрыта.
pub struct ProjectSearch {
    root: Option<PathBuf>,
    open: bool,
    query: Entity<TextInput>,
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
    /// Файлы с вхождениями — по порядку путей.
    results: Vec<FileMatches>,
    /// Плоский список для отрисовки: заголовок файла, затем его строки.
    rows: Vec<Row>,
    /// Сколько вхождений и файлов уже пришло — для статуса во время поиска.
    found: (usize, usize),
    /// Выбранная строка совпадения (индекс в `rows`).
    selected: Option<usize>,
    /// Строку выбрал пользователь (стрелки, щелчок): при приходе новых результатов выбор
    /// остаётся на ней. Иначе выбрано первое совпадение списка.
    selection_pinned: bool,
    status: Status,
    /// Флаг отмены идущего поиска.
    cancel: Arc<AtomicBool>,
    /// Пауза, поиск и приём результатов; сброс задачи — отмена.
    search: Option<Task<()>>,
    scroll: UniformListScrollHandle,
    _subscription: Subscription,
}

impl EventEmitter<ProjectSearchEvent> for ProjectSearch {}

impl ProjectSearch {
    pub fn new(root: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| TextInput::new("Search in project", cx));
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
            rows: Vec::new(),
            found: (0, 0),
            selected: None,
            selection_pinned: false,
            status,
            cancel: Arc::new(AtomicBool::new(false)),
            search: None,
            scroll: UniformListScrollHandle::new(),
            _subscription: subscription,
        }
    }

    /// Новый корень проекта: результаты сбрасываются; открытая панель с запросом ищет заново.
    pub fn set_root(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        self.root = root;
        self.cancel_search();
        self.clear_results();
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

    /// cmd-shift-f: закрытую панель открыть и перевести фокус в поле запроса (`seed` — текст
    /// для него, например выделение); открытую без фокуса — сфокусировать; в фокусе — закрыть.
    pub fn toggle(&mut self, seed: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let input_focus = self.query.focus_handle(cx);
        if self.open && input_focus.is_focused(window) {
            return self.close(cx);
        }
        let reopened = !self.open;
        self.open = true;
        let current = self.query.read(cx).text();
        match seed {
            // Новый текст сам запустит поиск (`InputEvent::Changed`).
            Some(seed) if seed != current => {
                self.query.update(cx, |query, cx| query.set_text(&seed, cx));
            }
            // Файлы могли измениться, пока панель была скрыта.
            _ if reopened && !current.is_empty() => self.schedule_search(cx),
            _ => {}
        }
        self.query.update(cx, |query, cx| query.select_all(cx));
        window.focus(&input_focus);
        cx.notify();
    }

    /// Esc (в панели или в редакторе, когда там снимать нечего), ×, cmd-shift-f из поля:
    /// панель скрывается, фокус — в редактор.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        cx.emit(ProjectSearchEvent::FocusEditor);
        cx.notify();
    }

    // --- Поиск ---

    fn search_query(&self, cx: &App) -> SearchQuery {
        SearchQuery {
            text: self.query.read(cx).text(),
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            regex: self.regex,
        }
    }

    /// Запрос или переключатель изменились: прошлый поиск отменяется сразу, новый
    /// начинается после паузы.
    fn schedule_search(&mut self, cx: &mut Context<Self>) {
        self.cancel_search();
        let query = self.search_query(cx);
        let Some(root) = self.root.clone() else {
            self.clear_results();
            self.status = Status::NoRoot;
            return cx.notify();
        };
        if query.is_empty() {
            self.clear_results();
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
            // Канал закрывается, когда поиск закончился (отправитель уничтожен).
            while let Some(file) = receiver.next().await {
                let mut batch = vec![file];
                while let Ok(file) = receiver.try_recv() {
                    batch.push(file);
                }
                let added = this.update(cx, |this, cx| {
                    this.add_files(batch);
                    cx.notify();
                });
                if added.is_err() {
                    return;
                }
            }
            let summary = search.await;
            this.update(cx, |this, cx| {
                this.finish(summary);
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

    fn clear_results(&mut self) {
        self.results.clear();
        self.rows.clear();
        self.found = (0, 0);
        self.selected = None;
        self.selection_pinned = false;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    /// Пачка файлов от поиска: встают по порядку путей; выбор сохраняется на той же строке
    /// (если его делал пользователь) или переходит на первое совпадение.
    fn add_files(&mut self, files: Vec<FileMatches>) {
        let pinned = self
            .selected
            .filter(|_| self.selection_pinned)
            .and_then(|row| match self.rows[row] {
                Row::Line { file, line } => Some((self.results[file].path.clone(), line)),
                Row::File(_) => None,
            });
        for file in files {
            self.found.0 += match_count(&file);
            self.found.1 += 1;
            insert_sorted(&mut self.results, file);
        }
        self.rows = flatten(&self.results);
        self.selected = match pinned {
            Some((path, line)) => self.rows.iter().position(|row| {
                matches!(*row, Row::Line { file, line: l } if l == line && self.results[file].path == path)
            }),
            None => next_match_row(&self.rows, None, true),
        };
    }

    fn finish(&mut self, summary: Result<GrepSummary, QueryError>) {
        self.status = match summary {
            Ok(summary) => Status::Done(summary),
            Err(error) => Status::Error(error.message),
        };
    }

    // --- Выбор и открытие ---

    fn select_match(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(row) = next_match_row(&self.rows, self.selected, forward) else {
            return;
        };
        self.selected = Some(row);
        self.selection_pinned = true;
        // Прокрутка на минимум: вниз — строка у нижнего края, вверх — у верхнего, вместе
        // с заголовком файла, если строка в файле первая.
        if forward {
            self.scroll.scroll_to_item(row, ScrollStrategy::Bottom);
        } else {
            let top = match row.checked_sub(1).map(|header| self.rows[header]) {
                Some(Row::File(_)) => row - 1,
                _ => row,
            };
            self.scroll.scroll_to_item(top, ScrollStrategy::Top);
        }
        cx.notify();
    }

    /// Просит Workspace открыть выбранное вхождение; `focus` — перевести фокус в редактор.
    fn open_selected(&mut self, focus: bool, cx: &mut Context<Self>) {
        let (Some(root), Some(row)) = (&self.root, self.selected) else {
            return;
        };
        let Row::Line { file, line } = self.rows[row] else {
            return;
        };
        let location = location_for(root, &self.results[file], line);
        cx.emit(ProjectSearchEvent::Open { location, focus });
    }

    /// Щелчок по переключателю или его сочетание: искать заново, фокус — в поле.
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
    }

    // --- Отображение ---

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let (status, is_error) = status_text(&self.status, self.found);
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(ui.border)
            .child(div().flex_1().min_w_0().child(self.query.clone()))
            .child(toggle_button(
                "case",
                "Aa",
                self.case_sensitive,
                ui,
                cx,
                |this| &mut this.case_sensitive,
            ))
            .child(toggle_button(
                "word",
                "ab",
                self.whole_word,
                ui,
                cx,
                |this| &mut this.whole_word,
            ))
            .child(toggle_button("regex", ".*", self.regex, ui, cx, |this| {
                &mut this.regex
            }))
            .child(
                div()
                    .flex_none()
                    .max_w(px(STATUS_MAX_WIDTH))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(if is_error { ui.error } else { ui.dim })
                    .child(status),
            )
            .child(
                div()
                    .id("close")
                    .flex_none()
                    .px_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_color(ui.dim)
                    .hover(move |style| style.text_color(ui.foreground).bg(ui.list_hover))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close(cx)))
                    .child("×"),
            )
    }

    fn render_row(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let row = div()
            .id(index)
            .w_full()
            .h(px(ROW_HEIGHT))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .overflow_hidden()
            .whitespace_nowrap();
        match self.rows[index] {
            Row::File(file) => {
                let file = &self.results[file];
                let (name, dir) = split_path(&file.path);
                row.child(div().flex_none().text_color(ui.foreground).child(name))
                    .when(!dir.is_empty(), |row| {
                        row.child(div().flex_none().text_color(ui.dim).child(dir))
                    })
                    .child(
                        div()
                            .flex_none()
                            .text_color(ui.dim)
                            .child(match_count(file).to_string()),
                    )
                    .into_any_element()
            }
            Row::Line { file, line } => {
                let selected = self.selected == Some(index);
                let found = &self.results[file].lines[line];
                let (text, ranges) = display_text(found);
                let style = HighlightStyle {
                    background_color: Some(ui.search_match),
                    ..Default::default()
                };
                row.cursor_pointer()
                    .when(selected, |row| row.bg(ui.list_selected))
                    .when(!selected, |row| row.hover(|style| style.bg(ui.list_hover)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.selected = Some(index);
                        this.selection_pinned = true;
                        this.open_selected(true, cx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_none()
                            .w(px(LINE_NUMBER_WIDTH))
                            .flex()
                            .justify_end()
                            .text_color(ui.dim)
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
                    )
                    .into_any_element()
            }
        }
    }
}

impl Render for ProjectSearch {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let list = uniform_list(
            "project-search-results",
            self.rows.len(),
            cx.processor(|this, range: Range<usize>, _, cx| {
                range
                    .map(|index| this.render_row(index, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.scroll.clone())
        .size_full();
        div()
            .key_context("ProjectSearch")
            .flex_none()
            .h(relative(PANEL_HEIGHT))
            .min_h(px(PANEL_MIN_HEIGHT))
            .flex()
            .flex_col()
            .bg(ui.panel)
            .border_t_1()
            .border_color(ui.border)
            .text_size(px(TEXT_SIZE))
            .text_color(ui.foreground)
            .on_action(cx.listener(|this, _: &SelectNextMatch, _, cx| this.select_match(true, cx)))
            .on_action(
                cx.listener(|this, _: &SelectPreviousMatch, _, cx| this.select_match(false, cx)),
            )
            .on_action(cx.listener(|this, _: &OpenMatch, _, cx| this.open_selected(false, cx)))
            .on_action(
                cx.listener(|this, _: &OpenMatchAndFocus, _, cx| this.open_selected(true, cx)),
            )
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
            .child(self.render_header(cx))
            .child(div().flex_1().min_h_0().py_1().child(list))
    }
}

impl Drop for ProjectSearch {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Переключатель в шапке: `Aa`, `ab`, `.*`; включённый — с фоном `toggle_active`.
fn toggle_button(
    id: &'static str,
    label: &'static str,
    on: bool,
    ui: UiColors,
    cx: &mut Context<ProjectSearch>,
    option: fn(&mut ProjectSearch) -> &mut bool,
) -> impl IntoElement + use<> {
    div()
        .id(id)
        .flex_none()
        .px_1p5()
        .rounded_sm()
        .cursor_pointer()
        .text_color(if on { ui.foreground } else { ui.dim })
        .when(on, |button| button.bg(ui.toggle_active))
        .when(!on, |button| button.hover(|style| style.bg(ui.list_hover)))
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.toggle_option(option, window, cx)
        }))
        .child(label)
}

// --- Чистая логика ---

/// Вставляет файл по порядку путей; возвращает его место.
fn insert_sorted(results: &mut Vec<FileMatches>, file: FileMatches) -> usize {
    let index = results.partition_point(|other| other.path < file.path);
    results.insert(index, file);
    index
}

/// Плоский список: заголовок каждого файла, за ним его строки с вхождениями.
fn flatten(results: &[FileMatches]) -> Vec<Row> {
    results
        .iter()
        .enumerate()
        .flat_map(|(file, matches)| {
            std::iter::once(Row::File(file))
                .chain((0..matches.lines.len()).map(move |line| Row::Line { file, line }))
        })
        .collect()
}

/// Следующая (`forward`) или предыдущая строка совпадения после `from`; без выбора — первая
/// в списке. Заголовки файлов пропускаются; у края списка — `None` (выбор остаётся).
fn next_match_row(rows: &[Row], from: Option<usize>, forward: bool) -> Option<usize> {
    let is_match = |index: &usize| matches!(rows[*index], Row::Line { .. });
    match (from, forward) {
        (None, _) => (0..rows.len()).find(is_match),
        (Some(from), true) => (from + 1..rows.len()).find(is_match),
        (Some(from), false) => (0..from.min(rows.len())).rev().find(is_match),
    }
}

/// Сколько вхождений в файле.
fn match_count(file: &FileMatches) -> usize {
    file.lines.iter().map(|line| line.ranges.len().max(1)).sum()
}

/// Куда перейти по строке совпадения: на первое вхождение в ней.
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

/// Имя файла и каталог (относительный, через `/`) для заголовка.
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

/// Строка совпадения для показа: без отступа в начале, табы — пробелами, обрезанное окном
/// начало строки — с «…». Вхождения — байтовые диапазоны в этой строке.
fn display_text(found: &LineMatch) -> (String, Vec<Range<usize>>) {
    let indent = found.text.chars().take_while(|c| c.is_whitespace()).count();
    // Отступ режется только до первого вхождения: поиск мог найти и сами пробелы.
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

/// Символьные диапазоны → байтовые; за концом строки — обрезаются, пустые — отбрасываются.
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

/// Текст статуса и признак ошибки. `found` — вхождения и файлы, пришедшие во время поиска.
fn status_text(status: &Status, found: (usize, usize)) -> (SharedString, bool) {
    let text = match status {
        Status::Idle => String::new(),
        Status::NoRoot => "No project folder — open one with ⌘O".into(),
        Status::Searching if found.0 == 0 => "Searching…".into(),
        Status::Searching => format!("Searching… {}", results_label(found.0, found.1)),
        Status::Done(summary) if summary.matches == 0 => "No results".into(),
        Status::Done(summary) if summary.truncated => {
            format!("Stopped at {} results", summary.matches)
        }
        Status::Done(summary) => results_label(summary.matches, summary.files_matched),
        Status::Error(message) => return (message.clone().into(), true),
    };
    (text.into(), false)
}

/// «3 results in 2 files», «1 result in 1 file».
fn results_label(matches: usize, files: usize) -> String {
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    format!(
        "{matches} result{} in {files} file{}",
        plural(matches),
        plural(files)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Диапазон без литерала `a..b` в одноэлементных массивах (clippy принимает его за ошибку).
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

    #[test]
    fn files_are_kept_in_path_order() {
        let mut results = Vec::new();
        for path in ["src/b.rs", "Cargo.toml", "src/a.rs", "README.md"] {
            insert_sorted(&mut results, file(path, 1));
        }
        let paths: Vec<_> = results.iter().map(|f| f.path.to_str().unwrap()).collect();
        assert_eq!(paths, ["Cargo.toml", "README.md", "src/a.rs", "src/b.rs"]);
        // Каталог — по компонентам: `src/x` раньше `src.rs`.
        let mut results = Vec::new();
        insert_sorted(&mut results, file("src.rs", 1));
        assert_eq!(insert_sorted(&mut results, file("src/x.rs", 1)), 0);
    }

    #[test]
    fn rows_are_headers_followed_by_lines() {
        let rows = flatten(&[file("a.rs", 2), file("b.rs", 1)]);
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
    fn selection_skips_headers_and_stops_at_the_edges() {
        let rows = flatten(&[file("a.rs", 2), file("b.rs", 1)]);
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
        // «    let ы = foo;»: вхождения «ы» (символы 8..9) и «foo» (12..15).
        let (text, ranges) = display_text(&line(0, "    let ы = foo;", &[8..9, 12..15]));
        assert_eq!(text, "let ы = foo;");
        assert_eq!(ranges, [4..6, 9..12]);
        assert_eq!(&text[ranges[1].clone()], "foo");
        // Табы — пробелы, длина в символах та же.
        let (text, ranges) = display_text(&line(0, "\tx\ty", &[span(3, 4)]));
        assert_eq!(text, "x y");
        assert_eq!(&text[ranges[0].clone()], "y");
        // Искали сами пробелы: отступ режется только до вхождения.
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
