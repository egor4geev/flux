//! Строка поиска и замены в активном документе (cmd-f, cmd-alt-f).
//!
//! Одна на окно (её держит Workspace), работает с активной вкладкой. Ищет
//! `flux_search::find_all` в фоне по снимку текста; найденное хранит и рисует редактор
//! (`Editor::set_search_highlights`) — он же сдвигает вхождения своими правками, пока не придут
//! свежие результаты. Строка помнит запрос, переключатели и итог последнего поиска.
//!
//! Когда ищем и что выделяем ([`SearchMode`]):
//! - запрос или переключатель изменился — поиск «по мере набора»: текущим становится первое
//!   вхождение, которое кончается после начала выделения, и оно выделяется в редакторе;
//! - документ изменился (пока строка открыта) или сменилась вкладка — выделение не трогаем;
//! - после «заменить» — выделяется следующее вхождение от конца вставки.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use flux_search::{BufferMatches, QueryError, SearchQuery, find_all, replacement_for};
use gpui::{
    Action, App, AppContext, ClickEvent, Context, Entity, FocusHandle, Focusable, KeyBinding,
    Render, SharedString, Subscription, Task, WeakEntity, Window, actions, div, prelude::*, px,
};

use crate::editor::{Editor, EditorEvent};
use crate::icons::{IconName, icon};
use crate::input::{InputEvent, TextInput};
use crate::theme::{self, Theme};
use crate::ui::{self, GAP, ICON_BUTTON_SIZE};

/// Ширина полей запроса и замены.
const INPUT_WIDTH: f32 = 380.;
/// Сообщение об ошибке в запросе — не шире этого.
const COUNTER_MAX_WIDTH: f32 = 320.;

actions!(
    find_bar,
    [
        Deploy,
        DeployReplace,
        FindNext,
        FindPrevious,
        SelectNextMatch,
        SelectPreviousMatch,
        SelectAllMatches,
        Dismiss,
        FocusNextField,
        ToggleCaseSensitive,
        ToggleWholeWord,
        ToggleRegex,
        ReplaceNext,
        ReplaceAll,
    ]
);

pub fn init(cx: &mut App) {
    let workspace = Some("Workspace");
    cx.bind_keys([
        KeyBinding::new("cmd-f", Deploy, workspace),
        KeyBinding::new("cmd-alt-f", DeployReplace, workspace),
        // Как в JetBrains.
        KeyBinding::new("cmd-r", DeployReplace, workspace),
        KeyBinding::new("cmd-g", FindNext, workspace),
        KeyBinding::new("cmd-shift-g", FindPrevious, workspace),
    ]);
    let bar = Some("FindBar");
    cx.bind_keys([
        KeyBinding::new("enter", SelectNextMatch, bar),
        KeyBinding::new("shift-enter", SelectPreviousMatch, bar),
        KeyBinding::new("alt-enter", SelectAllMatches, bar),
        KeyBinding::new("escape", Dismiss, bar),
        KeyBinding::new("tab", FocusNextField, bar),
        KeyBinding::new("shift-tab", FocusNextField, bar),
        KeyBinding::new("alt-cmd-c", ToggleCaseSensitive, bar),
        KeyBinding::new("alt-cmd-w", ToggleWholeWord, bar),
        KeyBinding::new("alt-cmd-r", ToggleRegex, bar),
    ]);
    // Поле замены вложено в строку: его контекст глубже, и Enter здесь — «заменить».
    let replace = Some("ReplaceField");
    cx.bind_keys([
        KeyBinding::new("enter", ReplaceNext, replace),
        KeyBinding::new("cmd-enter", ReplaceAll, replace),
    ]);
}

/// Что делать с выделением, когда придут результаты поиска.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchMode {
    /// Запрос или переключатель изменился: текущее — первое вхождение, которое кончается после
    /// начала выделения (курсор внутри слова находит само слово); оно выделяется.
    Incremental,
    /// Документ изменился или сменилась вкладка: выделение не трогать; текущее — вхождение,
    /// совпадающее с выделением.
    KeepSelection,
    /// Cmd+G при закрытой строке: найти и перейти к следующему (`backward` — предыдущему).
    Step { backward: bool },
    /// После «заменить»: выделить первое вхождение, которое начинается с этой позиции.
    SelectFrom(usize),
}

/// Итог последнего поиска — для счётчика.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    /// Запрос пуст (или поиск в новой вкладке ещё не закончился).
    Idle,
    /// Вхождения и текущее — в редакторе; `truncated` — найдены не все.
    Found { truncated: bool },
    /// Запрос не собрался (ошибка в регулярном выражении).
    Error(SharedString),
}

/// Строка поиска окна: одна на Workspace, работает с активной вкладкой.
pub struct FindBar {
    open: bool,
    replace_open: bool,
    query: Entity<TextInput>,
    replacement: Entity<TextInput>,
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
    editor: Option<WeakEntity<Editor>>,
    editor_subscription: Option<Subscription>,
    status: Status,
    /// Запрос последнего запущенного поиска: повторный Changed с тем же текстом не ищет снова.
    searched: Option<SearchQuery>,
    /// Номер последнего поиска: результаты прежних отбрасываются.
    generation: u64,
    cancel: Arc<AtomicBool>,
    search_task: Option<Task<()>>,
    replace_task: Option<Task<()>>,
    /// Правок активного документа с тех пор, как он стал активным: «заменить всё» применяется,
    /// только если за время фонового расчёта их не прибавилось.
    edits: u64,
    /// После «заменить»: от какой позиции выделить вхождение, когда придут свежие результаты.
    select_from: Option<usize>,
    _subscriptions: Vec<Subscription>,
}

impl FindBar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| TextInput::new("Find", cx).code().icon(IconName::Search));
        let replacement = cx.new(|cx| TextInput::new("Replace", cx).code().icon(IconName::Replace));
        let subscriptions = vec![cx.subscribe_in(
            &query,
            window,
            |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::Changed => {
                    if this.searched.as_ref() != Some(&this.search_query(cx)) {
                        this.search(SearchMode::Incremental, cx);
                    }
                }
            },
        )];
        Self {
            open: false,
            replace_open: false,
            query,
            replacement,
            case_sensitive: false,
            whole_word: false,
            regex: false,
            editor: None,
            editor_subscription: None,
            status: Status::Idle,
            searched: None,
            generation: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            search_task: None,
            replace_task: None,
            edits: 0,
            select_from: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Открыть строку поиска (с заменой — `replace`) для `editor` и перевести в неё фокус.
    /// Запрос берётся из выделения или слова под курсором; если фокус уже в строке — только
    /// выделяется текст поля.
    pub fn deploy(
        &mut self,
        replace: bool,
        editor: Option<Entity<Editor>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = editor else {
            return;
        };
        self.set_active_editor(Some(editor.clone()), window, cx);
        let was_open = self.open;
        let focused_here = was_open && self.contains_focus(window, cx);
        self.open = true;
        if replace {
            self.replace_open = true;
        } else if !was_open {
            self.replace_open = false;
        }

        let seed = (!focused_here).then(|| self.seed(&editor, cx)).flatten();
        let current = self.query.read(cx).text();
        match seed {
            // Changed запустит поиск по мере набора.
            Some(seed) if seed != current => {
                self.query.update(cx, |query, cx| query.set_text(&seed, cx));
            }
            _ if !was_open => self.search(SearchMode::KeepSelection, cx),
            _ => {}
        }

        let target = if replace && !self.query.read(cx).is_empty() {
            self.replacement.clone()
        } else {
            self.query.clone()
        };
        target.update(cx, |input, cx| input.select_all(cx));
        window.focus(&target.focus_handle(cx));
        cx.notify();
    }

    /// Следующее (`backward` — предыдущее) вхождение в активном документе — cmd-g. При закрытой
    /// строке открывает её, не трогая фокус; пустой запрос берётся из выделения.
    pub fn select_next(&mut self, backward: bool, _: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor() else {
            return;
        };
        if self.open {
            return self.step(backward, cx);
        }
        if self.query.read(cx).is_empty() {
            let Some(seed) = self.seed(&editor, cx) else {
                return;
            };
            // Changed с этим текстом поиск не повторит: `searched` уже будет он.
            self.query.update(cx, |query, cx| query.set_text(&seed, cx));
        }
        self.open = true;
        self.search(SearchMode::Step { backward }, cx);
        cx.notify();
    }

    /// Сменилась активная вкладка или вкладок не осталось.
    pub fn set_active_editor(
        &mut self,
        editor: Option<Entity<Editor>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.editor.as_ref().map(|editor| editor.entity_id());
        if current == editor.as_ref().map(Entity::entity_id) {
            return;
        }
        if let Some(old) = self.editor() {
            old.update(cx, |old, cx| old.clear_search_highlights(cx));
        }
        self.cancel_search();
        self.editor = editor.as_ref().map(Entity::downgrade);
        self.editor_subscription = editor
            .as_ref()
            .map(|editor| cx.subscribe_in(editor, window, Self::on_editor_event));
        self.edits = 0;
        self.status = Status::Idle;
        if self.open && !self.query.read(cx).is_empty() {
            self.search(SearchMode::KeepSelection, cx);
        }
        cx.notify();
    }

    fn editor(&self) -> Option<Entity<Editor>> {
        self.editor.as_ref()?.upgrade()
    }

    /// Запрос из выделения или слова под курсором; в режиме regex — экранированный, чтобы
    /// искался буквально.
    fn seed(&self, editor: &Entity<Editor>, cx: &App) -> Option<String> {
        let seed = editor.read(cx).search_seed()?;
        Some(if self.regex {
            escape_regex(&seed)
        } else {
            seed
        })
    }

    fn search_query(&self, cx: &App) -> SearchQuery {
        SearchQuery {
            text: self.query.read(cx).text(),
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            regex: self.regex,
        }
    }

    /// Фокус в одном из полей строки.
    fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.query.focus_handle(cx).is_focused(window)
            || self.replacement.focus_handle(cx).is_focused(window)
    }

    fn on_editor_event(
        &mut self,
        _: &Entity<Editor>,
        event: &EditorEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            EditorEvent::Edited => {
                self.edits += 1;
                if self.open {
                    let mode = self
                        .select_from
                        .take()
                        .map_or(SearchMode::KeepSelection, SearchMode::SelectFrom);
                    self.search(mode, cx);
                }
            }
        }
    }

    // --- Поиск ---

    /// Ищет запрос в активном документе в фоне; прошлый поиск отменяется.
    fn search(&mut self, mode: SearchMode, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Relaxed);
        self.search_task = None;
        self.generation += 1;
        let Some(editor) = self.editor() else {
            return;
        };
        let query = self.search_query(cx);
        self.searched = Some(query.clone());
        if query.is_empty() {
            self.status = Status::Idle;
            editor.update(cx, |editor, cx| editor.clear_search_highlights(cx));
            return cx.notify();
        }
        let text = editor.read(cx).document.text().clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        let generation = self.generation;
        let found = cx.background_spawn(async move { find_all(&text, &query, &cancel) });
        self.search_task = Some(cx.spawn(async move |this, cx| {
            let found = found.await;
            this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.show_results(found, mode, cx);
                }
            })
            .ok();
        }));
    }

    fn show_results(
        &mut self,
        found: Result<BufferMatches, QueryError>,
        mode: SearchMode,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.editor() else {
            return;
        };
        let found = match found {
            Ok(found) => found,
            Err(error) => {
                self.status = Status::Error(error.message.into());
                editor.update(cx, |editor, cx| editor.clear_search_highlights(cx));
                return cx.notify();
            }
        };
        self.status = Status::Found {
            truncated: found.truncated,
        };
        let matches = found.ranges;
        editor.update(cx, |editor, cx| {
            let selection = editor.document.selection().primary();
            let (from, to) = (selection.from(), selection.to());
            let target = match mode {
                SearchMode::Incremental => match_around(&matches, from),
                SearchMode::KeepSelection => None,
                SearchMode::Step { backward } => step_index(&matches, from, to, backward),
                SearchMode::SelectFrom(pos) => next_match(&matches, pos),
            };
            let active = target.or_else(|| exact_match(&matches, from, to));
            let range = target.map(|i| matches[i].clone());
            editor.set_search_highlights(matches, active, cx);
            if let Some(range) = range {
                editor.select_range(range, cx);
            }
        });
        cx.notify();
    }

    fn cancel_search(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.generation += 1;
        self.search_task = None;
        self.replace_task = None;
        self.select_from = None;
    }

    /// Следующее или предыдущее вхождение от выделения (по уже найденному).
    fn step(&mut self, backward: bool, cx: &mut Context<Self>) {
        let Some(editor) = self.editor() else {
            return;
        };
        editor.update(cx, |editor, cx| {
            let selection = editor.document.selection().primary();
            let matches = &editor.search_highlights().matches;
            let Some(index) = step_index(matches, selection.from(), selection.to(), backward)
            else {
                return;
            };
            let range = matches[index].clone();
            editor.set_active_match(Some(index), cx);
            editor.select_range(range, cx);
        });
        cx.notify();
    }

    /// Alt+Enter: все вхождения — выделениями (мультикурсор), фокус в редактор.
    fn select_all_matches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor() else {
            return;
        };
        let (ranges, primary) = {
            let highlights = editor.read(cx).search_highlights();
            (highlights.matches.clone(), highlights.active.unwrap_or(0))
        };
        if ranges.is_empty() {
            return;
        }
        editor.update(cx, |editor, cx| editor.select_ranges(ranges, primary, cx));
        window.focus(&editor.read(cx).focus_handle);
    }

    // --- Замена ---

    /// Заменяет текущее вхождение, если выделено ровно оно, и переходит к следующему; иначе —
    /// просто к следующему.
    fn replace_next(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor() else {
            return;
        };
        let query = self.search_query(cx);
        if query.is_empty() {
            return;
        }
        let replacement = self.replacement.read(cx).text();
        let edit = {
            let editor = editor.read(cx);
            let selection = editor.document.selection().primary();
            let highlights = editor.search_highlights();
            highlights
                .active
                .and_then(|i| highlights.matches.get(i))
                .filter(|m| m.start == selection.from() && m.end == selection.to())
                .and_then(|m| {
                    let text =
                        replacement_for(editor.document.text(), &query, m.clone(), &replacement)?;
                    Some((m.clone(), text))
                })
        };
        match edit {
            Some((range, text)) => {
                // Следующее вхождение выделится, когда пересчёт после правки закончится.
                self.select_from = Some(range.start + text.chars().count());
                editor.update(cx, |editor, cx| {
                    editor.replace_ranges(vec![(range, text)], cx)
                });
            }
            None => self.step(false, cx),
        }
    }

    /// Заменяет все вхождения одной правкой (один шаг undo). Считается в фоне; если документ
    /// за это время изменился или сменилась вкладка — ничего не делает.
    fn replace_all(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor() else {
            return;
        };
        let query = self.search_query(cx);
        if query.is_empty() {
            return;
        }
        let replacement = self.replacement.read(cx).text();
        let text = editor.read(cx).document.text().clone();
        let editor_id = editor.entity_id();
        let edits_before = self.edits;
        let job = cx.background_spawn(async move {
            flux_search::replace_all(&text, &query, &replacement, &AtomicBool::new(false))
        });
        self.replace_task = Some(cx.spawn(async move |this, cx| {
            let edits = job.await;
            this.update(cx, |this, cx| {
                let Some(editor) = this.editor().filter(|e| e.entity_id() == editor_id) else {
                    return;
                };
                if this.edits != edits_before {
                    return;
                }
                match edits {
                    Ok(edits) if edits.is_empty() => {}
                    Ok(edits) => {
                        let message = replaced_label(edits.len());
                        editor.update(cx, |editor, cx| {
                            editor.replace_ranges(edits, cx);
                            editor.show_status(message.into(), cx);
                        });
                    }
                    Err(error) => {
                        this.status = Status::Error(error.message.into());
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }

    // --- Строка ---

    /// Escape (в строке или в редакторе), ×: убрать подсветку и вернуть фокус в редактор
    /// (выделение остаётся на последнем текущем вхождении).
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
        self.cancel_search();
        if let Some(editor) = self.editor() {
            editor.update(cx, |editor, cx| editor.clear_search_highlights(cx));
            window.focus(&editor.read(cx).focus_handle);
        }
        cx.notify();
    }

    fn toggle_replace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_open = !self.replace_open;
        if !self.replace_open && self.replacement.focus_handle(cx).is_focused(window) {
            window.focus(&self.query.focus_handle(cx));
        }
        cx.notify();
    }

    /// Tab: между полями запроса и замены (если замена открыта).
    fn focus_next_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.replace_open {
            return;
        }
        let target = if self.query.focus_handle(cx).is_focused(window) {
            self.replacement.clone()
        } else {
            self.query.clone()
        };
        target.update(cx, |input, cx| input.select_all(cx));
        window.focus(&target.focus_handle(cx));
    }

    fn toggle_option(&mut self, option: fn(&mut Self) -> &mut bool, cx: &mut Context<Self>) {
        let flag = option(self);
        *flag = !*flag;
        self.search(SearchMode::Incremental, cx);
        cx.notify();
    }

    /// Что показать за переключателями: счётчик вхождений или ошибку в запросе.
    fn counter(&self, cx: &App) -> Counter {
        match &self.status {
            Status::Idle => Counter::None,
            Status::Error(message) => Counter::Error(message.clone()),
            Status::Found { truncated } => {
                let (count, active) = self.editor().map_or((0, None), |editor| {
                    let highlights = editor.read(cx).search_highlights();
                    (highlights.matches.len(), highlights.active)
                });
                let label = counter_label(count, active, *truncated).into();
                if count == 0 {
                    Counter::Error(label)
                } else {
                    Counter::Found(label)
                }
            }
        }
    }
}

/// Правая часть строки поиска после переключателей.
enum Counter {
    None,
    /// «3 of 12», «12 results».
    Found(SharedString),
    /// «No results» или ошибка в регулярном выражении.
    Error(SharedString),
}

impl Render for FindBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        // Подсказки — с сочетаниями так, будто фокус в поле: они верны и когда фокус в тексте.
        let query_focus = self.query.focus_handle(cx);
        let replace_focus = self.replacement.focus_handle(cx);
        let tip = |label: &'static str, action: &dyn Action, focus: &FocusHandle| {
            ui::tooltip(label, ui::shortcut_in(action, focus, window))
        };
        let row = || div().flex().items_center().gap_1p5();
        let counter = match self.counter(cx) {
            Counter::None => None,
            Counter::Found(label) => Some(ui::badge(label, ui.accent_text).into_any_element()),
            Counter::Error(message) => Some(
                div()
                    .flex_none()
                    .max_w(px(COUNTER_MAX_WIDTH))
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_color(ui.error)
                    .child(icon(IconName::Warning, ui.error).size(px(13.)))
                    .child(div().min_w_0().truncate().child(message))
                    .into_any_element(),
            ),
        };
        let chevron = if self.replace_open {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };
        let find_row = row()
            .child(
                ui::icon_button("toggle-replace", chevron, ui)
                    .tooltip(tip("Toggle Replace", &DeployReplace, &query_focus))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.toggle_replace(window, cx)
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(INPUT_WIDTH))
                    .child(self.query.clone()),
            )
            .child(
                ui::toggle_button("case", IconName::CaseSensitive, self.case_sensitive, ui)
                    .tooltip(tip("Match Case", &ToggleCaseSensitive, &query_focus))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.toggle_option(|this| &mut this.case_sensitive, cx)
                    })),
            )
            .child(
                ui::toggle_button("word", IconName::WholeWord, self.whole_word, ui)
                    .tooltip(tip("Whole Word", &ToggleWholeWord, &query_focus))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.toggle_option(|this| &mut this.whole_word, cx)
                    })),
            )
            .child(
                ui::toggle_button("regex", IconName::Regex, self.regex, ui)
                    .tooltip(tip("Regular Expression", &ToggleRegex, &query_focus))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.toggle_option(|this| &mut this.regex, cx)
                    })),
            )
            .child(div().flex_none().w(px(2.)))
            .children(counter)
            .child(
                ui::icon_button("previous", IconName::ArrowUp, ui)
                    .tooltip(tip("Previous Match", &SelectPreviousMatch, &query_focus))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.step(true, cx))),
            )
            .child(
                ui::icon_button("next", IconName::ArrowDown, ui)
                    .tooltip(tip("Next Match", &SelectNextMatch, &query_focus))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.step(false, cx))),
            )
            .child(div().flex_1())
            .child(
                ui::icon_button("close", IconName::Close, ui)
                    .tooltip(tip("Close", &Dismiss, &query_focus))
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx)),
                    ),
            );
        let replace_row = self.replace_open.then(|| {
            row()
                // Под шевроном — пусто: поля стоят друг под другом.
                .child(div().flex_none().w(px(ICON_BUTTON_SIZE)))
                .child(
                    div()
                        .key_context("ReplaceField")
                        .flex_none()
                        .w(px(INPUT_WIDTH))
                        .child(self.replacement.clone()),
                )
                .child(
                    ui::icon_button("replace", IconName::Replace, ui)
                        .tooltip(tip("Replace", &ReplaceNext, &replace_focus))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.replace_next(cx))),
                )
                .child(
                    ui::icon_button("replace-all", IconName::ReplaceAll, ui)
                        .tooltip(tip("Replace All", &ReplaceAll, &replace_focus))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.replace_all(cx))),
                )
        });

        div()
            .key_context("FindBar")
            .relative()
            .flex_none()
            .flex()
            .flex_col()
            .gap_1p5()
            .px_2()
            .py_2()
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_SM))
            .text_color(ui.foreground)
            .on_action(cx.listener(|this, _: &SelectNextMatch, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &SelectPreviousMatch, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &SelectAllMatches, window, cx| {
                this.select_all_matches(window, cx)
            }))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.close(window, cx)))
            .on_action(
                cx.listener(|this, _: &FocusNextField, window, cx| {
                    this.focus_next_field(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ToggleCaseSensitive, _, cx| {
                this.toggle_option(|this| &mut this.case_sensitive, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleWholeWord, _, cx| {
                this.toggle_option(|this| &mut this.whole_word, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleRegex, _, cx| {
                this.toggle_option(|this| &mut this.regex, cx)
            }))
            .on_action(cx.listener(|this, _: &ReplaceNext, _, cx| this.replace_next(cx)))
            .on_action(cx.listener(|this, _: &ReplaceAll, _, cx| this.replace_all(cx)))
            .child(find_row)
            .children(replace_row)
            // Разделитель под строкой — от края до края острова, с отступом от скруглений.
            .child(
                div()
                    .absolute()
                    .left(px(GAP))
                    .right(px(GAP))
                    .bottom_0()
                    .h(px(1.))
                    .bg(ui.divider),
            )
    }
}

// --- Выбор вхождения: вхождения — по возрастанию, без пересечений ---

/// Первое вхождение, которое кончается после `pos` (содержит `pos` или правее); дальше
/// конца — первое (по кругу).
fn match_around(matches: &[Range<usize>], pos: usize) -> Option<usize> {
    wrap_forward(matches, matches.partition_point(|m| m.end <= pos))
}

/// Первое вхождение, которое начинается в `pos` или правее; по кругу.
fn next_match(matches: &[Range<usize>], pos: usize) -> Option<usize> {
    wrap_forward(matches, matches.partition_point(|m| m.start < pos))
}

/// Последнее вхождение, которое кончается в `pos` или левее; по кругу — последнее.
fn previous_match(matches: &[Range<usize>], pos: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let after = matches.partition_point(|m| m.end <= pos);
    Some(after.checked_sub(1).unwrap_or(matches.len() - 1))
}

fn wrap_forward(matches: &[Range<usize>], index: usize) -> Option<usize> {
    if matches.is_empty() {
        None
    } else if index == matches.len() {
        Some(0)
    } else {
        Some(index)
    }
}

/// Следующее (`backward` — предыдущее) вхождение от выделения `from..to`: выделенное
/// вхождение пропускается.
fn step_index(matches: &[Range<usize>], from: usize, to: usize, backward: bool) -> Option<usize> {
    if backward {
        previous_match(matches, from)
    } else {
        next_match(matches, to)
    }
}

/// Вхождение, совпадающее с выделением `from..to`.
fn exact_match(matches: &[Range<usize>], from: usize, to: usize) -> Option<usize> {
    let index = matches.partition_point(|m| m.start < from);
    matches
        .get(index)
        .filter(|m| m.start == from && m.end == to)
        .map(|_| index)
}

/// «3 of 17», «17 results», «No results»; найдены не все — «100000+».
fn counter_label(count: usize, active: Option<usize>, truncated: bool) -> String {
    let total = if truncated {
        format!("{count}+")
    } else {
        count.to_string()
    };
    match (count, active) {
        (0, _) => "No results".into(),
        (_, Some(index)) => format!("{} of {total}", index + 1),
        (1, None) if !truncated => "1 result".into(),
        (_, None) => format!("{total} results"),
    }
}

fn replaced_label(count: usize) -> String {
    match count {
        1 => "Replaced 1 occurrence".into(),
        _ => format!("Replaced {count} occurrences"),
    }
}

/// Выделенный текст для запроса в режиме регулярного выражения ищется буквально.
fn escape_regex(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if r"\.+*?()|[]{}^$#&-~".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    /// «ab let cd let ef let»: вхождения let — 3..6, 10..13, 17..20.
    const MATCHES: [Range<usize>; 3] = [3..6, 10..13, 17..20];

    #[test]
    fn incremental_search_finds_the_match_around_or_after_the_selection() {
        assert_eq!(match_around(&MATCHES, 0), Some(0));
        // Курсор внутри слова и сразу перед ним — само слово.
        assert_eq!(match_around(&MATCHES, 4), Some(0));
        assert_eq!(match_around(&MATCHES, 3), Some(0));
        // Сразу за словом — следующее.
        assert_eq!(match_around(&MATCHES, 6), Some(1));
        // За последним — по кругу первое.
        assert_eq!(match_around(&MATCHES, 20), Some(0));
        assert_eq!(match_around(&[], 5), None);
    }

    #[test]
    fn next_and_previous_skip_the_selected_match_and_wrap() {
        // Выделено второе вхождение.
        assert_eq!(step_index(&MATCHES, 10, 13, false), Some(2));
        assert_eq!(step_index(&MATCHES, 10, 13, true), Some(0));
        // С последнего — на первое и наоборот.
        assert_eq!(step_index(&MATCHES, 17, 20, false), Some(0));
        assert_eq!(step_index(&MATCHES, 3, 6, true), Some(2));
        // Курсор между вхождениями.
        assert_eq!(step_index(&MATCHES, 8, 8, false), Some(1));
        assert_eq!(step_index(&MATCHES, 8, 8, true), Some(0));
        // Курсор в начале вхождения — оно и есть следующее.
        assert_eq!(step_index(&MATCHES, 10, 10, false), Some(1));
        // Курсор внутри вхождения: назад — предыдущее, вперёд — следующее.
        assert_eq!(step_index(&MATCHES, 11, 11, true), Some(0));
        assert_eq!(step_index(&MATCHES, 11, 11, false), Some(2));
        assert_eq!(step_index(&[], 0, 0, false), None);
        assert_eq!(step_index(&[], 0, 0, true), None);
    }

    #[test]
    fn exact_match_needs_the_same_range() {
        assert_eq!(exact_match(&MATCHES, 10, 13), Some(1));
        assert_eq!(exact_match(&MATCHES, 10, 12), None);
        assert_eq!(exact_match(&MATCHES, 11, 11), None);
        assert_eq!(exact_match(&MATCHES, 30, 30), None);
    }

    #[test]
    fn after_a_replacement_the_next_match_is_selected() {
        // Заменили второе: вставка кончается на 12 — следующее начинается на 17.
        assert_eq!(next_match(&MATCHES, 12), Some(2));
        // Заменили последнее — по кругу первое.
        assert_eq!(next_match(&MATCHES, 25), Some(0));
    }

    #[test]
    fn counter_texts() {
        assert_eq!(counter_label(17, Some(2), false), "3 of 17");
        assert_eq!(counter_label(17, None, false), "17 results");
        assert_eq!(counter_label(1, None, false), "1 result");
        assert_eq!(counter_label(0, None, false), "No results");
        assert_eq!(counter_label(100_000, Some(0), true), "1 of 100000+");
        assert_eq!(counter_label(100_000, None, true), "100000+ results");
        assert_eq!(replaced_label(1), "Replaced 1 occurrence");
        assert_eq!(replaced_label(3), "Replaced 3 occurrences");
    }

    #[test]
    fn regex_metacharacters_are_escaped() {
        assert_eq!(escape_regex("a.b(c)"), r"a\.b\(c\)");
        assert_eq!(escape_regex("x*y+z?"), r"x\*y\+z\?");
        assert_eq!(escape_regex("[a-z]{2}"), r"\[a\-z\]\{2\}");
        assert_eq!(escape_regex("^$|\\"), r"\^\$\|\\");
        assert_eq!(escape_regex("привет"), "привет");
    }
}
