//! Вью редактора: связывает документ из ядра с окном, клавиатурой, мышью и IME.

use std::ops::Range as Utf16Range;
use std::path::PathBuf;
use std::time::Duration;

use flux_core::movement::{self, Direction};
use flux_core::text::{CharClass, char_class, line_len, line_start};
use flux_core::{
    Assoc, ChangeSet, Document, EditKind, Range, Rope, Selection, TextChange, Transaction, edit,
};
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point,
    Render, ScrollWheelEvent, SharedString, Subscription, Task, UTF16Selection, Window, actions,
    div, point, prelude::*, px,
};

use crate::element::{EditorElement, LayoutCache};
use crate::highlighter::{self, Highlighter, ParseMode};
use crate::theme::{self, Theme};

actions!(
    editor,
    [
        MoveLeft,
        MoveRight,
        MoveUp,
        MoveDown,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        MoveWordLeft,
        MoveWordRight,
        SelectWordLeft,
        SelectWordRight,
        MoveLineStart,
        MoveLineEnd,
        SelectLineStart,
        SelectLineEnd,
        MoveDocumentStart,
        MoveDocumentEnd,
        SelectDocumentStart,
        SelectDocumentEnd,
        PageUp,
        PageDown,
        AddCursorAbove,
        AddCursorBelow,
        Cancel,
        SelectAll,
        Backspace,
        Delete,
        DeleteWordBackward,
        DeleteLine,
        Newline,
        Tab,
        Undo,
        Redo,
        Copy,
        Cut,
        Paste,
        Save,
    ]
);

pub fn bind_keys(cx: &mut App) {
    let context = Some("Editor");
    cx.bind_keys([
        KeyBinding::new("left", MoveLeft, context),
        KeyBinding::new("right", MoveRight, context),
        KeyBinding::new("up", MoveUp, context),
        KeyBinding::new("down", MoveDown, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("shift-up", SelectUp, context),
        KeyBinding::new("shift-down", SelectDown, context),
        KeyBinding::new("alt-left", MoveWordLeft, context),
        KeyBinding::new("alt-right", MoveWordRight, context),
        KeyBinding::new("alt-shift-left", SelectWordLeft, context),
        KeyBinding::new("alt-shift-right", SelectWordRight, context),
        KeyBinding::new("cmd-left", MoveLineStart, context),
        KeyBinding::new("cmd-right", MoveLineEnd, context),
        KeyBinding::new("home", MoveLineStart, context),
        KeyBinding::new("end", MoveLineEnd, context),
        KeyBinding::new("cmd-shift-left", SelectLineStart, context),
        KeyBinding::new("cmd-shift-right", SelectLineEnd, context),
        KeyBinding::new("shift-home", SelectLineStart, context),
        KeyBinding::new("shift-end", SelectLineEnd, context),
        KeyBinding::new("cmd-up", MoveDocumentStart, context),
        KeyBinding::new("cmd-down", MoveDocumentEnd, context),
        KeyBinding::new("cmd-shift-up", SelectDocumentStart, context),
        KeyBinding::new("cmd-shift-down", SelectDocumentEnd, context),
        KeyBinding::new("pageup", PageUp, context),
        KeyBinding::new("pagedown", PageDown, context),
        KeyBinding::new("cmd-alt-up", AddCursorAbove, context),
        KeyBinding::new("cmd-alt-down", AddCursorBelow, context),
        KeyBinding::new("escape", Cancel, context),
        KeyBinding::new("cmd-a", SelectAll, context),
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new("shift-backspace", Backspace, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new("alt-backspace", DeleteWordBackward, context),
        // Как Delete Line в JetBrains: строка целиком.
        KeyBinding::new("cmd-backspace", DeleteLine, context),
        KeyBinding::new("enter", Newline, context),
        KeyBinding::new("shift-enter", Newline, context),
        KeyBinding::new("tab", Tab, context),
        KeyBinding::new("cmd-z", Undo, context),
        KeyBinding::new("cmd-shift-z", Redo, context),
        KeyBinding::new("cmd-c", Copy, context),
        KeyBinding::new("cmd-x", Cut, context),
        KeyBinding::new("cmd-v", Paste, context),
        KeyBinding::new("cmd-s", Save, context),
    ]);
}

/// События редактора — для тех, кто следит за ним со стороны (строка поиска).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorEvent {
    /// Текст документа изменился: правка, вставка, IME, undo, redo.
    Edited,
}

/// Как прокрутить к главному курсору при следующей отрисовке.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Autoscroll {
    /// Держать курсор в окне с запасом в несколько строк — обычное движение и правка.
    Fit,
    /// Если строка курсора не видна — поставить её в середину окна: переход к найденному,
    /// к строке, к результату поиска по проекту.
    Center,
    /// Строку курсора — в середину окна, даже если она видна: превью поиска по проекту.
    Middle,
}

/// Вхождения, найденные строкой поиска: по возрастанию, без пересечений; `active` — текущее
/// (рисуется ярче). Владеет ими редактор: он рисует их и сдвигает своими правками, пока не
/// придут свежие результаты поиска.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchHighlights {
    pub matches: Vec<std::ops::Range<usize>>,
    pub active: Option<usize>,
}

impl SearchHighlights {
    /// Сдвигает вхождения правкой; схлопнувшиеся до пустых убираются.
    fn map(&mut self, changes: &ChangeSet) {
        let positions = self
            .matches
            .iter()
            .flat_map(|m| [(m.start, Assoc::After), (m.end, Assoc::Before)]);
        let mapped = changes.map_sorted(positions);
        let active = self.active.take();
        let mut matches = Vec::with_capacity(self.matches.len());
        for (i, &[start, end]) in mapped.as_chunks::<2>().0.iter().enumerate() {
            if start < end {
                if active == Some(i) {
                    self.active = Some(matches.len());
                }
                matches.push(start..end);
            }
        }
        self.matches = matches;
    }
}

/// Вид одного документа: свои скролл, выделение и состояние IME.
pub struct Editor {
    pub(crate) document: Document,
    pub(crate) focus_handle: FocusHandle,
    /// Смещение видимой области в пикселях.
    pub(crate) scroll: Point<f32>,
    /// Прокрутить к курсору при следующей отрисовке.
    pub(crate) autoscroll: Option<Autoscroll>,
    /// Текст, который сейчас набирается через IME (ещё не подтверждён).
    pub(crate) marked_range: Option<std::ops::Range<usize>>,
    pub(crate) layout: Option<LayoutCache>,
    /// Подсветка синтаксиса документа и её фоновый разбор.
    pub(crate) highlighter: Highlighter,
    /// Фаза мигания: курсор сейчас нарисован.
    pub(crate) cursor_visible: bool,
    /// Таймер мигания; есть, только пока редактор в фокусе и окно активно.
    blink_task: Option<Task<()>>,
    selecting: bool,
    status: Option<SharedString>,
    /// Подсветка найденного строкой поиска.
    pub(crate) search: SearchHighlights,
    /// Только для чтения (превью поиска по проекту): без фокуса, правок и мыши — кроме
    /// колеса прокрутки; щелчки уходят родителю.
    preview: bool,
    _subscriptions: Vec<Subscription>,
}

/// Статус документа для статус-бара окна: позиция, курсоры, язык, переводы строк и
/// сообщение (ошибка ввода-вывода и т.п.).
pub(crate) struct StatusInfo {
    pub message: Option<SharedString>,
    /// Строка и колонка с единицы.
    pub line: usize,
    pub column: usize,
    pub cursors: usize,
    pub language: String,
    pub line_ending: &'static str,
}

impl EventEmitter<EditorEvent> for Editor {}

impl Editor {
    pub fn new(document: Document, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        // Мигание включается и выключается вместе с фокусом и активностью окна.
        let subscriptions = vec![
            cx.on_focus(&focus_handle, window, Self::restart_blink),
            cx.on_blur(&focus_handle, window, Self::restart_blink),
            cx.observe_window_activation(window, Self::restart_blink),
            // Новая тема — новое отображение capture на её области.
            cx.observe_global::<Theme>(|this, cx| {
                this.highlighter.refresh_map(Theme::get(cx));
                cx.notify();
            }),
        ];
        Self::build(document, focus_handle, false, subscriptions, cx)
    }

    /// Превью только для чтения: документ с подсветкой, без фокуса, курсора и правок. Место
    /// показывает [`Editor::show_position`], найденное — [`Editor::set_search_highlights`].
    pub fn preview(document: Document, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let subscriptions = vec![cx.observe_global::<Theme>(|this, cx| {
            this.highlighter.refresh_map(Theme::get(cx));
            cx.notify();
        })];
        Self::build(document, focus_handle, true, subscriptions, cx)
    }

    fn build(
        document: Document,
        focus_handle: FocusHandle,
        preview: bool,
        subscriptions: Vec<Subscription>,
        cx: &mut Context<Self>,
    ) -> Self {
        let highlighter = Highlighter::new(document.path(), document.text());
        let mut editor = Self {
            document,
            focus_handle,
            scroll: point(0., 0.),
            autoscroll: Some(Autoscroll::Fit),
            marked_range: None,
            layout: None,
            highlighter,
            cursor_visible: true,
            blink_task: None,
            selecting: false,
            status: None,
            search: SearchHighlights::default(),
            preview,
            _subscriptions: subscriptions,
        };
        // Первый разбор — сразу в фон: заодно там скомпилируется запрос подсветки.
        highlighter::parse(&mut editor, ParseMode::Background, cx);
        editor
    }

    /// Ставит курсор в `position` и прокручивает так, чтобы его строка была посередине
    /// (превью: курсор не рисуется, строка подсвечена как текущая).
    pub fn show_position(&mut self, position: usize, cx: &mut Context<Self>) {
        let position = position.min(self.document.text().len_chars());
        self.document.set_selection(Selection::point(position));
        self.autoscroll = Some(Autoscroll::Middle);
        cx.notify();
    }

    fn set_selection(&mut self, selection: Selection, cx: &mut Context<Self>) {
        self.document.set_selection(selection);
        self.marked_range = None;
        self.status = None;
        self.autoscroll = Some(Autoscroll::Fit);
        self.pause_blink(cx);
        cx.notify();
    }

    /// Двигает каждое выделение функцией из `movement`.
    fn motion(&mut self, cx: &mut Context<Self>, f: impl Fn(&Rope, Range) -> Range) {
        let text = self.document.text().clone();
        let selection = self
            .document
            .selection()
            .transform(|range| f(&text, *range));
        self.set_selection(selection, cx);
    }

    fn apply(&mut self, tx: Transaction, kind: EditKind, cx: &mut Context<Self>) {
        if let Some(change) = self.document.apply(tx, kind) {
            self.text_changed(&[change], cx);
        }
        self.status = None;
        self.autoscroll = Some(Autoscroll::Fit);
        self.pause_blink(cx);
        cx.notify();
    }

    /// Undo или Redo; `step` возвращает изменения текста или `None`, если шагать некуда.
    fn history_step(
        &mut self,
        cx: &mut Context<Self>,
        step: impl FnOnce(&mut Document) -> Option<Vec<TextChange>>,
    ) {
        if let Some(changes) = step(&mut self.document) {
            self.text_changed(&changes, cx);
            self.marked_range = None;
            self.autoscroll = Some(Autoscroll::Fit);
            self.pause_blink(cx);
            cx.notify();
        }
    }

    /// Текст изменился: подсветка сдвигает дерево и запускает разбор, найденное сдвигается
    /// до прихода свежих результатов; подписчики узнают о правке.
    fn text_changed(&mut self, changes: &[TextChange], cx: &mut Context<Self>) {
        for change in changes {
            self.highlighter.edit(change);
            if !self.search.matches.is_empty() {
                self.search.map(&change.changes);
            }
        }
        highlighter::parse(self, ParseMode::AfterEdit, cx);
        cx.emit(EditorEvent::Edited);
    }

    fn edit(
        &mut self,
        kind: EditKind,
        cx: &mut Context<Self>,
        f: impl FnOnce(&Rope, &Selection) -> Transaction,
    ) {
        let tx = f(self.document.text(), self.document.selection());
        self.apply(tx, kind, cx);
    }

    fn page_lines(&self) -> usize {
        self.layout
            .as_ref()
            .map_or(20, |layout| layout.visible_lines().saturating_sub(1).max(1))
    }

    fn page(&mut self, dir: Direction, cx: &mut Context<Self>) {
        let lines = self.page_lines();
        if let Some(layout) = &self.layout {
            let delta = lines as f32 * f32::from(layout.line_height);
            self.scroll.y += match dir {
                Direction::Backward => -delta,
                Direction::Forward => delta,
            };
        }
        self.motion(cx, |text, range| {
            movement::move_vertically(text, range, dir, lines, false)
        });
    }

    fn add_cursor(&mut self, dir: Direction, cx: &mut Context<Self>) {
        let text = self.document.text();
        let selection = self.document.selection();
        let edge = match dir {
            Direction::Backward => selection.ranges()[0],
            Direction::Forward => *selection.ranges().last().unwrap(),
        };
        let new = movement::move_vertically(text, Range::point(edge.head), dir, 1, false);
        if new.head == edge.head {
            return;
        }
        let mut ranges = selection.ranges().to_vec();
        ranges.push(new);
        let primary = ranges.len() - 1;
        self.set_selection(Selection::new(ranges, primary), cx);
    }

    /// Esc: несколько курсоров — оставить главный, выделение — снять. Снимать нечего — Esc
    /// уходит выше: Workspace закроет панель поиска по проекту.
    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        let selection = self.document.selection();
        if selection.len() == 1 && selection.primary().is_empty() {
            return cx.propagate();
        }
        let primary = self.document.selection().primary();
        let selection = if self.document.selection().len() > 1 {
            Selection::from_range(primary)
        } else {
            Selection::point(primary.head)
        };
        self.set_selection(selection, cx);
    }

    // --- Поиск и переходы: их вызывают строка поиска, поиск по проекту, переход к строке ---

    /// Подсвечивает найденные вхождения (по возрастанию, без пересечений).
    pub fn set_search_highlights(
        &mut self,
        matches: Vec<std::ops::Range<usize>>,
        active: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.search = SearchHighlights { matches, active };
        cx.notify();
    }

    pub fn clear_search_highlights(&mut self, cx: &mut Context<Self>) {
        if self.search != SearchHighlights::default() {
            self.search = SearchHighlights::default();
            cx.notify();
        }
    }

    pub fn search_highlights(&self) -> &SearchHighlights {
        &self.search
    }

    pub fn set_active_match(&mut self, active: Option<usize>, cx: &mut Context<Self>) {
        if self.search.active != active {
            self.search.active = active;
            cx.notify();
        }
    }

    /// Позиция по строке (с нуля) и колонке в символах; за краем — конец строки или документа.
    pub fn position(&self, line: usize, column: usize) -> usize {
        let text = self.document.text();
        let line = line.min(text.len_lines() - 1);
        line_start(text, line) + column.min(line_len(text, line))
    }

    /// Выделяет диапазон (курсор — в конце) и прокручивает к нему: если он не виден — к середине окна.
    pub fn select_range(&mut self, range: std::ops::Range<usize>, cx: &mut Context<Self>) {
        self.select_ranges(vec![range], 0, cx);
    }

    /// Несколько выделений разом (например, все найденные вхождения); к `primary` — прокрутка.
    pub fn select_ranges(
        &mut self,
        ranges: Vec<std::ops::Range<usize>>,
        primary: usize,
        cx: &mut Context<Self>,
    ) {
        if ranges.is_empty() {
            return;
        }
        let ranges = ranges
            .into_iter()
            .map(|r| Range::new(r.start, r.end))
            .collect();
        self.set_selection(Selection::new(ranges, primary), cx);
        self.autoscroll = Some(Autoscroll::Center);
    }

    /// Что искать по Cmd+F: главное выделение, если оно в одну строку, а без выделения — слово
    /// под курсором.
    pub fn search_seed(&self) -> Option<String> {
        let text = self.document.text();
        let primary = self.document.selection().primary();
        let range = if primary.is_empty() {
            let word = movement::word_range_at(text, primary.head);
            let is_word = word.from() < text.len_chars()
                && char_class(text.char(word.from())) == CharClass::Word;
            if !is_word {
                return None;
            }
            word
        } else {
            primary
        };
        let seed = text.slice(range.from()..range.to()).to_string();
        let single_line = !seed.contains(['\n', '\r']);
        (single_line && !seed.trim().is_empty()).then_some(seed)
    }

    /// Заменяет диапазоны текстами одной правкой — один шаг undo. Диапазоны — по возрастанию,
    /// без пересечений.
    pub fn replace_ranges(
        &mut self,
        edits: Vec<(std::ops::Range<usize>, String)>,
        cx: &mut Context<Self>,
    ) {
        if edits.is_empty() {
            return;
        }
        let changes = edits
            .into_iter()
            .map(|(range, text)| (range.start, range.end, Some(text)));
        let tx = Transaction::change(self.document.text(), changes);
        self.apply(tx, EditKind::Other, cx);
    }

    // --- Буфер обмена ---

    fn selected_text(&self) -> Option<String> {
        let text = self.document.text();
        let parts: Vec<String> = self
            .document
            .selection()
            .iter()
            .filter(|range| !range.is_empty())
            .map(|range| text.slice(range.from()..range.to()).to_string())
            .collect();
        (!parts.is_empty()).then(|| parts.join(self.document.line_ending()))
    }

    /// Целые строки под курсорами — Copy/Cut без выделения работают со строкой.
    fn line_selection(&self) -> Selection {
        let text = self.document.text();
        self.document.selection().transform(|range| {
            let first = text.char_to_line(range.from());
            let last = text.char_to_line(range.to());
            let end = if last + 1 < text.len_lines() {
                line_start(text, last + 1)
            } else {
                text.len_chars()
            };
            Range::new(line_start(text, first), end)
        })
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let text = self.selected_text().unwrap_or_else(|| {
            let lines = self.line_selection();
            let rope = self.document.text();
            lines
                .iter()
                .map(|r| rope.slice(r.from()..r.to()).to_string())
                .collect()
        });
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        self.copy(&Copy, window, cx);
        let selection = match self.selected_text() {
            Some(_) => self.document.selection().clone(),
            None => self.line_selection(),
        };
        let tx = edit::insert_text(self.document.text(), &selection, "");
        self.apply(tx, EditKind::Other, cx);
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(clipboard) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let text = clipboard.replace("\r\n", "\n");
        let text = match self.document.line_ending() {
            "\n" => text,
            ending => text.replace('\n', ending),
        };
        self.edit(EditKind::Other, cx, |rope, sel| {
            edit::insert_text(rope, sel, &text)
        });
    }

    // --- Файл ---

    /// Сохраняет документ; без пути — через «Сохранить как». Результат можно дождаться:
    /// `true` — документ записан на диск.
    pub fn save(&mut self, cx: &mut Context<Self>) -> Task<bool> {
        if self.document.path().is_some() {
            Task::ready(self.save_now(cx))
        } else {
            self.save_as(cx)
        }
    }

    fn save_now(&mut self, cx: &mut Context<Self>) -> bool {
        let saved = match self.document.save() {
            Ok(()) => {
                self.status = Some("Saved".into());
                true
            }
            Err(err) => {
                self.status = Some(format!("Save failed: {err}").into());
                false
            }
        };
        cx.notify();
        saved
    }

    /// `false` — пользователь отменил выбор файла или запись не удалась.
    fn save_as(&mut self, cx: &mut Context<Self>) -> Task<bool> {
        let directory = std::env::current_dir().unwrap_or_default();
        let path = cx.prompt_for_new_path(&directory, Some("untitled.txt"));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = path.await else {
                return false;
            };
            this.update(cx, |this, cx| this.save_to(path, cx))
                .unwrap_or(false)
        })
    }

    /// Привязывает документ к `path` и сохраняет.
    pub(crate) fn save_to(&mut self, path: PathBuf, cx: &mut Context<Self>) -> bool {
        self.set_path(path, cx);
        self.save_now(cx)
    }

    /// Привязывает документ к `path` без записи на диск: «Сохранить как», файл
    /// переименован или перемещён в дереве файлов. Со сменой расширения может смениться
    /// язык — тогда подсветка заводится заново и разбирается в фоне.
    pub fn set_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.highlighter.set_path(&path, self.document.text()) {
            highlighter::parse(self, ParseMode::Background, cx);
        }
        self.document.set_path(path);
        cx.notify();
    }

    /// Сообщение в статус-баре — до следующей правки или движения курсора.
    pub fn show_status(&mut self, message: SharedString, cx: &mut Context<Self>) {
        self.status = Some(message);
        cx.notify();
    }

    // --- Мигание курсора ---

    /// Вызывается при смене фокуса и активности окна: курсор сразу виден, а таймер
    /// мигания есть, только пока редактор в фокусе и окно активно.
    fn restart_blink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let blinking = self.focus_handle.is_focused(window) && window.is_window_active();
        self.cursor_visible = true;
        self.blink_task = theme::CURSOR_BLINK
            .filter(|_| blinking)
            .map(|period| Self::blink(period, cx));
        cx.notify();
    }

    /// Правка или движение: курсор сразу виден, следующее мигание — через полный период.
    /// Поэтому во время набора курсор не мигает.
    fn pause_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_visible = true;
        if self.blink_task.is_some()
            && let Some(period) = theme::CURSOR_BLINK
        {
            self.blink_task = Some(Self::blink(period, cx));
        }
    }

    fn blink(period: Duration, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(period).await;
                let toggled = this.update(cx, |this, cx| {
                    this.cursor_visible = !this.cursor_visible;
                    cx.notify();
                });
                if toggled.is_err() {
                    break;
                }
            }
        })
    }

    // --- Мышь ---

    fn position_for_mouse(&self, position: Point<Pixels>) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        Some(layout.position_for_point(self.document.text(), position))
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        let Some(pos) = self.position_for_mouse(event.position) else {
            return;
        };
        let text = self.document.text();
        let selection = self.document.selection();
        let selection = match event.click_count {
            2 => Selection::from_range(movement::word_range_at(text, pos)),
            3.. => {
                let line = text.char_to_line(pos);
                let end = if line + 1 < text.len_lines() {
                    line_start(text, line + 1)
                } else {
                    text.len_chars()
                };
                Selection::single(line_start(text, line), end)
            }
            _ if event.modifiers.shift => {
                Selection::from_range(selection.primary().put_cursor(pos, true))
            }
            _ if event.modifiers.alt => {
                let mut ranges = selection.ranges().to_vec();
                ranges.push(Range::point(pos));
                let primary = ranges.len() - 1;
                Selection::new(ranges, primary)
            }
            _ => Selection::point(pos),
        };
        self.selecting = true;
        self.set_selection(selection, cx);
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            self.selecting = false;
            return;
        }
        let Some(pos) = self.position_for_mouse(event.position) else {
            return;
        };
        let selection = self.document.selection();
        let primary = selection.primary().put_cursor(pos, true);
        let mut ranges = selection.ranges().to_vec();
        ranges[selection.primary_index()] = primary;
        let index = selection.primary_index();
        self.set_selection(Selection::new(ranges, index), cx);
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(theme::LINE_HEIGHT));
        self.scroll.x -= f32::from(delta.x);
        self.scroll.y -= f32::from(delta.y);
        self.autoscroll = None;
        cx.notify();
    }

    // --- Отображение ---

    /// Статус-бар окна для этого редактора: рисует его Workspace внизу окна, под панелями.
    /// Что показать в статус-баре окна про этот документ (рисует Workspace).
    pub(crate) fn status_info(&self) -> StatusInfo {
        let text = self.document.text();
        let primary = self.document.selection().primary();
        let line = text.char_to_line(primary.head);
        let column = primary.head - line_start(text, line);
        StatusInfo {
            message: self.status.clone(),
            line: line + 1,
            column: column + 1,
            cursors: self.document.selection().len(),
            language: self.highlighter.status(),
            line_ending: match self.document.line_ending() {
                "\r\n" => "CRLF",
                _ => "LF",
            },
        }
    }

    // --- UTF-16 ↔ символы: IME и macOS считают позиции в UTF-16 ---

    fn utf16_range(&self, range: &std::ops::Range<usize>) -> Utf16Range<usize> {
        let text = self.document.text();
        text.char_to_utf16_cu(range.start)..text.char_to_utf16_cu(range.end)
    }

    fn char_range(&self, range: &Utf16Range<usize>) -> std::ops::Range<usize> {
        let text = self.document.text();
        let len = text.len_utf16_cu();
        text.utf16_cu_to_char(range.start.min(len))..text.utf16_cu_to_char(range.end.min(len))
    }

    /// Диапазон, который заменяет IME: явно заданный, текущая композиция или выделение.
    fn input_range(
        &self,
        range_utf16: Option<Utf16Range<usize>>,
    ) -> Option<std::ops::Range<usize>> {
        range_utf16
            .map(|r| self.char_range(&r))
            .or_else(|| self.marked_range.clone())
    }
}

impl EntityInputHandler for Editor {
    fn text_for_range(
        &mut self,
        range_utf16: Utf16Range<usize>,
        actual_range: &mut Option<Utf16Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.char_range(&range_utf16);
        actual_range.replace(self.utf16_range(&range));
        Some(self.document.text().slice(range).to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let primary = self.document.selection().primary();
        Some(UTF16Selection {
            range: self.utf16_range(&(primary.from()..primary.to())),
            reversed: primary.head < primary.anchor,
        })
    }

    fn marked_text_range(
        &self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Utf16Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.utf16_range(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    /// Обычный ввод символов приходит сюда, а не через действия.
    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Utf16Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let kind = if new_text.contains('\n') {
            EditKind::Other
        } else {
            EditKind::Insert
        };
        let tx = match self.input_range(range_utf16) {
            Some(range) => {
                let text = self.document.text();
                let end = range.start + new_text.chars().count();
                Transaction::change(text, [(range.start, range.end, Some(new_text.to_owned()))])
                    .with_selection(Selection::point(end))
            }
            // Без явного диапазона — печатаем во все курсоры.
            None => edit::insert_text(self.document.text(), self.document.selection(), new_text),
        };
        self.marked_range = None;
        self.apply(tx, kind, cx);
    }

    /// Промежуточный текст IME (например, набор иероглифов или «ё» через долгое нажатие).
    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Utf16Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Utf16Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let primary = self.document.selection().primary();
        let range = self
            .input_range(range_utf16)
            .unwrap_or(primary.from()..primary.to());
        let len = new_text.chars().count();
        // Выделение внутри композиции IME передаёт в UTF-16 относительно её начала.
        let selected = new_selected_range_utf16
            .map(|r| {
                let chars: Vec<usize> = new_text
                    .chars()
                    .scan(0, |utf16, c| {
                        let at = *utf16;
                        *utf16 += c.len_utf16();
                        Some(at)
                    })
                    .collect();
                let to_char = |u: usize| chars.partition_point(|&at| at < u);
                range.start + to_char(r.start)..range.start + to_char(r.end)
            })
            .unwrap_or(range.start + len..range.start + len);

        let tx = Transaction::change(
            self.document.text(),
            [(range.start, range.end, Some(new_text.to_owned()))],
        )
        .with_selection(Selection::single(selected.start, selected.end));
        self.apply(tx, EditKind::Insert, cx);
        self.marked_range = (len > 0).then(|| range.start..range.start + len);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Utf16Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.char_range(&range_utf16);
        self.layout
            .as_ref()?
            .bounds_for_position(self.document.text(), range.start)
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let pos = self.position_for_mouse(point)?;
        Some(self.document.text().char_to_utf16_cu(pos))
    }
}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use Direction::{Backward, Forward};
        let ui = Theme::ui(cx);
        // Превью не берёт фокус: иначе щелчок по нему увёл бы ввод из поля поиска.
        let root = div().key_context("Editor");
        let root = if self.preview {
            root
        } else {
            root.track_focus(&self.focus_handle)
        };
        root.size_full()
            .flex()
            .flex_col()
            .text_color(ui.foreground)
            .font_family(theme::code_font())
            .text_size(px(theme::FONT_SIZE))
            // Движение
            .on_action(cx.listener(|this, _: &MoveLeft, _, cx| {
                this.motion(cx, |t, r| {
                    movement::move_horizontally(t, r, Backward, false)
                })
            }))
            .on_action(cx.listener(|this, _: &MoveRight, _, cx| {
                this.motion(cx, |t, r| movement::move_horizontally(t, r, Forward, false))
            }))
            .on_action(cx.listener(|this, _: &MoveUp, _, cx| {
                this.motion(cx, |t, r| {
                    movement::move_vertically(t, r, Backward, 1, false)
                })
            }))
            .on_action(cx.listener(|this, _: &MoveDown, _, cx| {
                this.motion(cx, |t, r| {
                    movement::move_vertically(t, r, Forward, 1, false)
                })
            }))
            .on_action(cx.listener(|this, _: &SelectLeft, _, cx| {
                this.motion(cx, |t, r| movement::move_horizontally(t, r, Backward, true))
            }))
            .on_action(cx.listener(|this, _: &SelectRight, _, cx| {
                this.motion(cx, |t, r| movement::move_horizontally(t, r, Forward, true))
            }))
            .on_action(cx.listener(|this, _: &SelectUp, _, cx| {
                this.motion(cx, |t, r| {
                    movement::move_vertically(t, r, Backward, 1, true)
                })
            }))
            .on_action(cx.listener(|this, _: &SelectDown, _, cx| {
                this.motion(cx, |t, r| movement::move_vertically(t, r, Forward, 1, true))
            }))
            .on_action(cx.listener(|this, _: &MoveWordLeft, _, cx| {
                this.motion(cx, |t, r| movement::move_word(t, r, Backward, false))
            }))
            .on_action(cx.listener(|this, _: &MoveWordRight, _, cx| {
                this.motion(cx, |t, r| movement::move_word(t, r, Forward, false))
            }))
            .on_action(cx.listener(|this, _: &SelectWordLeft, _, cx| {
                this.motion(cx, |t, r| movement::move_word(t, r, Backward, true))
            }))
            .on_action(cx.listener(|this, _: &SelectWordRight, _, cx| {
                this.motion(cx, |t, r| movement::move_word(t, r, Forward, true))
            }))
            .on_action(cx.listener(|this, _: &MoveLineStart, _, cx| {
                this.motion(cx, |t, r| movement::move_line_start(t, r, false))
            }))
            .on_action(cx.listener(|this, _: &MoveLineEnd, _, cx| {
                this.motion(cx, |t, r| movement::move_line_end(t, r, false))
            }))
            .on_action(cx.listener(|this, _: &SelectLineStart, _, cx| {
                this.motion(cx, |t, r| movement::move_line_start(t, r, true))
            }))
            .on_action(cx.listener(|this, _: &SelectLineEnd, _, cx| {
                this.motion(cx, |t, r| movement::move_line_end(t, r, true))
            }))
            .on_action(cx.listener(|this, _: &MoveDocumentStart, _, cx| {
                this.set_selection(Selection::point(0), cx)
            }))
            .on_action(cx.listener(|this, _: &MoveDocumentEnd, _, cx| {
                let end = this.document.text().len_chars();
                this.set_selection(Selection::point(end), cx)
            }))
            .on_action(cx.listener(|this, _: &SelectDocumentStart, _, cx| {
                this.motion(cx, |_, r| movement::move_document_start(r, true))
            }))
            .on_action(cx.listener(|this, _: &SelectDocumentEnd, _, cx| {
                this.motion(cx, |t, r| movement::move_document_end(t, r, true))
            }))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| this.page(Backward, cx)))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| this.page(Forward, cx)))
            .on_action(cx.listener(|this, _: &AddCursorAbove, _, cx| this.add_cursor(Backward, cx)))
            .on_action(cx.listener(|this, _: &AddCursorBelow, _, cx| this.add_cursor(Forward, cx)))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                let end = this.document.text().len_chars();
                this.set_selection(Selection::single(0, end), cx)
            }))
            // Правка
            .on_action(cx.listener(|this, _: &Backspace, _, cx| {
                this.edit(EditKind::Delete, cx, edit::delete_backward)
            }))
            .on_action(cx.listener(|this, _: &Delete, _, cx| {
                this.edit(EditKind::Delete, cx, edit::delete_forward)
            }))
            .on_action(cx.listener(|this, _: &DeleteWordBackward, _, cx| {
                this.edit(EditKind::Other, cx, edit::delete_word_backward)
            }))
            .on_action(cx.listener(|this, _: &DeleteLine, _, cx| {
                this.edit(EditKind::Other, cx, edit::delete_lines)
            }))
            .on_action(cx.listener(|this, _: &Newline, _, cx| {
                let ending = this.document.line_ending();
                this.edit(EditKind::Other, cx, |t, s| {
                    edit::insert_newline(t, s, ending)
                })
            }))
            .on_action(cx.listener(|this, _: &Tab, _, cx| {
                this.edit(EditKind::Insert, cx, |t, s| {
                    edit::insert_tab(t, s, theme::TAB_WIDTH)
                })
            }))
            .on_action(cx.listener(|this, _: &Undo, _, cx| this.history_step(cx, Document::undo)))
            .on_action(cx.listener(|this, _: &Redo, _, cx| this.history_step(cx, Document::redo)))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            // Файл
            .on_action(cx.listener(|this, _: &Save, _, cx| this.save(cx).detach()))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .when(!self.preview, |area| {
                        area.cursor(CursorStyle::IBeam)
                            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                            .on_mouse_move(cx.listener(Self::on_mouse_move))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, _, _| this.selecting = false),
                            )
                    })
                    .on_scroll_wheel(cx.listener(Self::on_scroll))
                    .child(EditorElement::new(cx.entity())),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn highlights(matches: &[(usize, usize)], active: Option<usize>) -> SearchHighlights {
        SearchHighlights {
            matches: matches.iter().map(|&(start, end)| start..end).collect(),
            active,
        }
    }

    /// Правка документа длины `len`: `(from, to, вставка)`.
    fn mapped(
        search: &SearchHighlights,
        len: usize,
        changes: Vec<(usize, usize, Option<&str>)>,
    ) -> SearchHighlights {
        let changes = changes
            .into_iter()
            .map(|(from, to, text)| (from, to, text.map(str::to_string)));
        let mut search = search.clone();
        search.map(&ChangeSet::from_changes(len, changes));
        search
    }

    #[test]
    fn matches_shift_with_edits_before_them() {
        // «ab foo cd foo»: вхождения foo — 3..6 и 10..13.
        let search = highlights(&[(3, 6), (10, 13)], Some(1));
        let got = mapped(&search, 13, vec![(0, 0, Some("xx"))]);
        assert_eq!(got, highlights(&[(5, 8), (12, 15)], Some(1)));
    }

    #[test]
    fn text_typed_at_match_edges_is_not_highlighted() {
        let search = highlights(&[(3, 6)], Some(0));
        let got = mapped(&search, 13, vec![(3, 3, Some("<")), (6, 6, Some(">"))]);
        assert_eq!(got, highlights(&[(4, 7)], Some(0)));
    }

    #[test]
    fn deleted_matches_disappear_and_active_follows_its_match() {
        let search = highlights(&[(3, 6), (10, 13)], Some(1));
        // Удалили первое вхождение целиком: текущее — теперь первое по счёту.
        let got = mapped(&search, 13, vec![(2, 7, None)]);
        assert_eq!(got, highlights(&[(5, 8)], Some(0)));
        // Удалили текущее: текущего нет.
        let got = mapped(&search, 13, vec![(9, 13, None)]);
        assert_eq!(got, highlights(&[(3, 6)], None));
    }

    #[test]
    fn edits_inside_a_match_resize_it() {
        let search = highlights(&[(3, 6)], None);
        let got = mapped(&search, 13, vec![(4, 5, Some("ooo"))]);
        assert_eq!(got, highlights(&[(3, 8)], None));
    }
}
