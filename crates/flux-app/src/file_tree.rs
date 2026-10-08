//! Дерево файлов проекта — панель слева от текста (⌘B — показать/скрыть, ⇧⌘E — фокус).
//!
//! Состояние — `flux_fs::tree::FileTree`: какие каталоги прочитаны и раскрыты. Каталоги
//! читаются только в фоне (`flux_fs::list_dir`): корень — при создании панели, остальные —
//! когда их раскрывают или когда дерево идёт к файлу активной вкладки ([`FileTreePanel::reveal`]).
//! Ответ на устаревший запрос чтения (каталог успели запросить ещё раз) отбрасывается по
//! номеру запроса. Изменения на диске сообщает `flux_fs::Watcher`: события склеиваются, и
//! перечитываются только прочитанные каталоги, которых они касаются.
//!
//! Операции — создать, переименовать, переместить (⌘X ⌘V, перетаскивание), скопировать,
//! удалить в Корзину — тоже в фоне; затронутые каталоги после них перечитываются сразу, не
//! дожидаясь событий. Открытые документы правит Workspace по событиям [`FileTreeEvent`].

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use flux_fs::tree::{FileTree, Row};
use flux_fs::{
    DirEntry, EntryKind, FsChange, Watcher, copy_into, create_dir, create_file, list_dir,
    move_into, remap, rename, trash, validate_name,
};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, CursorStyle, DismissEvent, DragMoveEvent,
    Entity, EventEmitter, FocusHandle, Focusable, KeyBinding, MouseButton, MouseDownEvent, Pixels,
    Point, PromptLevel, Render, ScrollStrategy, SharedString, Subscription, Task,
    UniformListScrollHandle, Window, actions, deferred, div, prelude::*, px, uniform_list,
};

use crate::context_menu::ContextMenu;
use crate::input::{InputEvent, TextInput};
use crate::theme::{self, Theme, UiColors};
use crate::workspace::tilde;

/// Ширина панели по умолчанию и пределы, в которых её тянут мышью.
pub const DEFAULT_WIDTH: f32 = 240.;
const MIN_WIDTH: f32 = 160.;
const MAX_WIDTH: f32 = 600.;
const ROW_HEIGHT: f32 = 22.;
const HEADER_HEIGHT: f32 = 28.;
/// Отступ на уровень вложенности и слева от строк.
const INDENT: f32 = 12.;
const ROW_PADDING: f32 = 8.;
/// Колонка под ▸/▾: у файлов она пустая — имена выровнены.
const CHEVRON_WIDTH: f32 = 14.;
const TEXT_SIZE: f32 = 13.;
const HEADER_TEXT_SIZE: f32 = 11.;
/// Полоса у правого края, за которую тянут ширину.
const RESIZE_HANDLE_WIDTH: f32 = 6.;
/// Склейка событий наблюдателя: после первого ждём, пока придут остальные.
const WATCH_DELAY: Duration = Duration::from_millis(80);
/// PageUp/PageDown, пока высота списка неизвестна.
const DEFAULT_PAGE_ROWS: usize = 20;

// Действия уровня окна: их обрабатывает Workspace.
actions!(file_tree, [ToggleOpen, ToggleFocus]);

// Действия панели (контекст "FileTree").
actions!(
    file_tree,
    [
        SelectNext,
        SelectPrevious,
        SelectFirst,
        SelectLast,
        SelectNextPage,
        SelectPreviousPage,
        Expand,
        Collapse,
        Open,
        OpenKeepFocus,
        NewFile,
        NewFolder,
        Rename,
        Duplicate,
        MoveToTrash,
        Cut,
        Copy,
        Paste,
        CopyPath,
        CopyRelativePath,
        RevealInFinder,
        CollapseAll,
        ShowContextMenu,
        Cancel,
        ConfirmEdit,
        CancelEdit,
    ]
);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-b", ToggleOpen, Some("Workspace")),
        KeyBinding::new("cmd-shift-e", ToggleFocus, Some("Workspace")),
    ]);
    // Пока правится имя, клавиши дерева молчат: стрелки, пробел, ⌫ и ⌘C/⌘V — у поля.
    let tree = Some("FileTree && !editing");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, tree),
        KeyBinding::new("up", SelectPrevious, tree),
        KeyBinding::new("home", SelectFirst, tree),
        KeyBinding::new("end", SelectLast, tree),
        KeyBinding::new("pagedown", SelectNextPage, tree),
        KeyBinding::new("pageup", SelectPreviousPage, tree),
        KeyBinding::new("right", Expand, tree),
        KeyBinding::new("left", Collapse, tree),
        KeyBinding::new("enter", Open, tree),
        KeyBinding::new("cmd-down", Open, tree),
        KeyBinding::new("space", OpenKeepFocus, tree),
        KeyBinding::new("cmd-n", NewFile, tree),
        KeyBinding::new("alt-cmd-n", NewFolder, tree),
        KeyBinding::new("f2", Rename, tree),
        KeyBinding::new("cmd-d", Duplicate, tree),
        // Основное сочетание — первым: его показывают контекстное меню и палитра.
        KeyBinding::new("cmd-backspace", MoveToTrash, tree),
        KeyBinding::new("backspace", MoveToTrash, tree),
        KeyBinding::new("delete", MoveToTrash, tree),
        KeyBinding::new("cmd-x", Cut, tree),
        KeyBinding::new("cmd-c", Copy, tree),
        KeyBinding::new("cmd-v", Paste, tree),
        KeyBinding::new("alt-cmd-c", CopyPath, tree),
        KeyBinding::new("alt-shift-cmd-c", CopyRelativePath, tree),
        KeyBinding::new("alt-cmd-r", RevealInFinder, tree),
        KeyBinding::new("shift-f10", ShowContextMenu, tree),
        KeyBinding::new("escape", Cancel, tree),
    ]);
    // Поле правки имени отдаёт Enter и Esc строке.
    let edit = Some("FileTreeEdit");
    cx.bind_keys([
        KeyBinding::new("enter", ConfirmEdit, edit),
        KeyBinding::new("escape", CancelEdit, edit),
    ]);
}

/// Что панель просит у Workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileTreeEvent {
    /// Открыть файл (или активировать его вкладку); `focus` — перевести фокус в редактор,
    /// иначе он остаётся в дереве.
    Open { path: PathBuf, focus: bool },
    /// Вернуть фокус в редактор (Esc в дереве).
    FocusEditor,
    /// Файл или каталог `from` переименован или перемещён в `to` — открытые документы
    /// внутри него переезжают на новые пути.
    Moved { from: PathBuf, to: PathBuf },
    /// Файлы и каталоги удалены в Корзину: вкладки документов внутри них закрываются
    /// (изменённые — остаются).
    Removed { paths: Vec<PathBuf> },
    /// Сообщение пользователю (ошибка операции) — в статус-бар.
    Message(SharedString),
}

/// Правка имени на месте: новый файл или каталог, переименование.
struct Edit {
    target: EditTarget,
    input: Entity<TextInput>,
    /// Почему не вышло (неверное имя, такое уже есть): красным под полем, поле остаётся.
    error: Option<SharedString>,
    /// Операция уже идёт — повторный ↵ ничего не делает.
    pending: bool,
    _subscriptions: [Subscription; 2],
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EditTarget {
    /// Новый файл или каталог в `dir`: строка поля — первой среди его детей.
    New {
        dir: PathBuf,
        kind: EntryKind,
    },
    Rename {
        path: PathBuf,
    },
}

/// Вырезанное (⌘X) или скопированное (⌘C) — до вставки.
#[derive(Debug, Clone)]
struct Clipboard {
    path: PathBuf,
    cut: bool,
}

struct Menu {
    menu: Entity<ContextMenu>,
    /// Точка щелчка в координатах окна.
    position: Point<Pixels>,
    _subscriptions: [Subscription; 2],
}

/// Перетаскиваемая строка; она же — подпись у курсора.
#[derive(Debug, Clone)]
struct DraggedEntry {
    path: PathBuf,
    name: SharedString,
}

/// Перетаскивание правого края панели.
#[derive(Debug, Clone, Copy)]
struct DraggedEdge;

/// Панель дерева файлов: одна на окно, живёт, пока у окна тот же корень проекта.
pub struct FileTreePanel {
    root: PathBuf,
    tree: FileTree,
    /// Видимые строки — пересобираются при каждом изменении дерева.
    rows: Vec<Row>,
    /// Выбранная строка — по пути: переживает перечитывание каталогов.
    selected: Option<PathBuf>,
    /// Выделить этот путь, как только дочитаются каталоги до него (reveal, новый файл).
    reveal_target: Option<PathBuf>,
    focus_handle: FocusHandle,
    scroll: UniformListScrollHandle,
    width: f32,
    /// Идущие чтения: номер последнего запроса по каталогу.
    reads: HashMap<PathBuf, u64>,
    next_read: u64,
    edit: Option<Edit>,
    clipboard: Option<Clipboard>,
    menu: Option<Menu>,
    /// Куда бросят перетаскиваемую строку: каталог под курсором.
    drop_target: Option<PathBuf>,
    _watcher: Option<Watcher>,
    _watch_task: Task<()>,
}

impl EventEmitter<FileTreeEvent> for FileTreePanel {}

impl FileTreePanel {
    /// Панель проекта `root` (канонический путь): сразу читает корень и начинает следить
    /// за изменениями на диске.
    pub fn new(root: PathBuf, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let watch_task = Self::watch(root.clone(), cx);
        let mut panel = Self {
            tree: FileTree::new(root.clone()),
            root,
            rows: Vec::new(),
            selected: None,
            reveal_target: None,
            focus_handle: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            width: DEFAULT_WIDTH,
            reads: HashMap::new(),
            next_read: 0,
            edit: None,
            clipboard: None,
            menu: None,
            drop_target: None,
            _watcher: None,
            _watch_task: watch_task,
        };
        panel.changed(cx);
        panel
    }

    /// Файл активной вкладки: раскрыть каталоги до него, выделить и прокрутить к нему.
    /// Файл вне корня проекта — снять выделение.
    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        if path == self.root || !self.tree.reveal(path) {
            self.selected = None;
            self.reveal_target = None;
            return cx.notify();
        }
        self.reveal_target = Some(path.to_path_buf());
        self.changed(cx);
    }

    // --- Чтение каталогов ---

    /// Дерево изменилось: пересобрать строки, дочитать видимое, довести reveal, перерисовать.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.rows = self.tree.rows();
        for (dir, ignored) in self.tree.pending_loads() {
            if !self.reads.contains_key(&dir) {
                self.read(dir, ignored, cx);
            }
        }
        self.finish_reveal();
        cx.notify();
    }

    /// Перечитывает каталоги: после своих операций и изменений на диске.
    fn reread(&mut self, dirs: impl IntoIterator<Item = PathBuf>, cx: &mut Context<Self>) {
        for dir in dirs {
            let ignored = self.tree.is_ignored(&dir);
            self.read(dir, ignored, cx);
        }
    }

    /// Читает каталог в фоне. Новый запрос по тому же каталогу делает прежний устаревшим.
    fn read(&mut self, dir: PathBuf, ignored: bool, cx: &mut Context<Self>) {
        self.next_read += 1;
        let id = self.next_read;
        self.reads.insert(dir.clone(), id);
        let listing = cx.background_spawn({
            let (root, dir) = (self.root.clone(), dir.clone());
            async move { list_dir(&root, &dir, ignored) }
        });
        cx.spawn(async move |this, cx| {
            let result = listing.await;
            this.update(cx, |this, cx| this.finish_read(dir, id, result, cx))
                .ok();
        })
        .detach();
    }

    fn finish_read(
        &mut self,
        dir: PathBuf,
        id: u64,
        result: io::Result<Vec<DirEntry>>,
        cx: &mut Context<Self>,
    ) {
        if self.reads.get(&dir) != Some(&id) {
            return;
        }
        self.reads.remove(&dir);
        let entries = match result {
            Ok(entries) => entries,
            // Каталог удалили: его родителя перечитают по тому же событию.
            Err(err) if err.kind() == io::ErrorKind::NotFound && dir != self.root => {
                self.tree.remove(&dir);
                return self.changed(cx);
            }
            // Перечитать не вышло — остаётся прежний список.
            Err(_) if self.tree.is_loaded(&dir) => return self.changed(cx),
            Err(err) => {
                let message = format!("Cannot read {}: {err}", display_name(&dir));
                cx.emit(FileTreeEvent::Message(message.into()));
                Vec::new()
            }
        };
        let stale = self.tree.set_listing(&dir, entries);
        self.reread(stale, cx);
        self.changed(cx);
    }

    /// Выделяет `reveal_target`, когда его строка появилась; всё дочитано, а строки нет —
    /// цель снимается.
    fn finish_reveal(&mut self) {
        let Some(target) = &self.reveal_target else {
            return;
        };
        if let Some(index) = self.row_index(target) {
            self.selected = Some(target.clone());
            let index = self.list_index(index);
            self.scroll.scroll_to_item(index, ScrollStrategy::Center);
            self.reveal_target = None;
        } else if self.reads.is_empty() {
            self.reveal_target = None;
        }
    }

    /// Выделить `path`, как только дочитаются каталоги до него.
    fn select_when_shown(&mut self, path: PathBuf) {
        self.tree.reveal(&path);
        self.reveal_target = Some(path);
    }

    /// Наблюдение за корнем: события склеиваются в пачку и превращаются в план перечитывания.
    fn watch(root: PathBuf, cx: &mut Context<Self>) -> Task<()> {
        let (sender, mut changes) = mpsc::unbounded::<FsChange>();
        let started = cx.background_spawn(async move {
            Watcher::new(&root, move |change| {
                sender.unbounded_send(change).ok();
            })
        });
        cx.spawn(async move |this, cx| {
            let watcher = match started.await {
                Ok(watcher) => watcher,
                Err(err) => {
                    let message = format!("Not watching the project for changes: {err}");
                    this.update(cx, |_, cx| cx.emit(FileTreeEvent::Message(message.into())))
                        .ok();
                    return;
                }
            };
            if this
                .update(cx, |this, _| this._watcher = Some(watcher))
                .is_err()
            {
                return;
            }
            while let Some(first) = changes.next().await {
                cx.background_executor().timer(WATCH_DELAY).await;
                let mut batch = vec![first];
                while let Ok(change) = changes.try_recv() {
                    batch.push(change);
                }
                let refreshed = this.update(cx, |this, cx| {
                    let plan = this.tree.refresh_plan(&batch);
                    this.reread(plan, cx);
                });
                if refreshed.is_err() {
                    break;
                }
            }
        })
    }

    // --- Выбор и навигация ---

    fn row_index(&self, path: &Path) -> Option<usize> {
        self.rows.iter().position(|row| row.path == path)
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected
            .as_deref()
            .and_then(|path| self.row_index(path))
    }

    /// Строка поля нового файла: место в списке, вложенность и вид.
    fn new_entry_slot(&self) -> Option<(usize, usize, EntryKind)> {
        let Some(Edit {
            target: EditTarget::New { dir, kind },
            ..
        }) = &self.edit
        else {
            return None;
        };
        if *dir == self.root {
            return Some((0, 0, *kind));
        }
        let index = self.row_index(dir)?;
        Some((index + 1, self.rows[index].depth + 1, *kind))
    }

    /// Номер в списке для строки дерева: поле нового файла сдвигает строки после себя.
    fn list_index(&self, row: usize) -> usize {
        match self.new_entry_slot() {
            Some((at, ..)) if row >= at => row + 1,
            _ => row,
        }
    }

    fn select_row(&mut self, index: usize, strategy: ScrollStrategy, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        self.selected = Some(row.path.clone());
        self.reveal_target = None;
        self.scroll.scroll_to_item(self.list_index(index), strategy);
        cx.notify();
    }

    /// Строки на страницу — по высоте списка в прошлом кадре.
    fn page_rows(&self) -> usize {
        let height = f32::from(self.scroll.0.borrow().base_handle.bounds().size.height);
        if height <= 0. {
            return DEFAULT_PAGE_ROWS;
        }
        ((height / ROW_HEIGHT) as usize).saturating_sub(1).max(1)
    }

    /// Прокрутка на минимум: вниз — строка встаёт к нижнему краю, вверх — к верхнему.
    fn move_selection(
        &mut self,
        to: impl FnOnce(Option<usize>, usize) -> usize,
        cx: &mut Context<Self>,
    ) {
        if self.rows.is_empty() {
            return;
        }
        let current = self.selected_index();
        let index = to(current, self.rows.len()).min(self.rows.len() - 1);
        let strategy = match current {
            Some(current) if index < current => ScrollStrategy::Top,
            _ => ScrollStrategy::Bottom,
        };
        self.select_row(index, strategy, cx);
    }

    /// → : свёрнутый каталог раскрыть, раскрытый — к первому ребёнку.
    fn expand(&mut self, _: &Expand, _: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_index() else {
            return;
        };
        let row = &self.rows[index];
        if row.kind != EntryKind::Dir {
            return;
        }
        if !row.expanded {
            let path = row.path.clone();
            self.tree.expand(&path);
            return self.changed(cx);
        }
        if self
            .rows
            .get(index + 1)
            .is_some_and(|next| next.depth > row.depth)
        {
            self.select_row(index + 1, ScrollStrategy::Bottom, cx);
        }
    }

    /// ← : раскрытый каталог свернуть, иначе — к родителю.
    fn collapse(&mut self, _: &Collapse, _: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_index() else {
            return;
        };
        let row = &self.rows[index];
        if row.kind == EntryKind::Dir && row.expanded {
            let path = row.path.clone();
            self.tree.collapse(&path);
            return self.changed(cx);
        }
        let parent = row.path.parent().and_then(|parent| self.row_index(parent));
        if let Some(parent) = parent {
            self.select_row(parent, ScrollStrategy::Top, cx);
        }
    }

    /// ↵, пробел, щелчок: каталог раскрыть или свернуть, файл — открыть.
    fn activate(&mut self, path: PathBuf, focus: bool, cx: &mut Context<Self>) {
        if self.tree.is_dir(&path) {
            self.tree.toggle(&path);
            self.keep_selection_visible();
            self.changed(cx);
        } else {
            cx.emit(FileTreeEvent::Open { path, focus });
        }
    }

    /// Выбранная строка скрылась в свёрнутом каталоге — выбрать ближайший видимый каталог над ней.
    fn keep_selection_visible(&mut self) {
        let Some(selected) = &self.selected else {
            return;
        };
        let rows = self.tree.rows();
        if rows.iter().any(|row| &row.path == selected) {
            return;
        }
        self.selected = selected
            .ancestors()
            .skip(1)
            .find(|dir| rows.iter().any(|row| row.path == *dir))
            .map(Path::to_path_buf);
    }

    fn collapse_all(&mut self, _: &CollapseAll, _: &mut Window, cx: &mut Context<Self>) {
        self.tree.collapse_all();
        self.keep_selection_visible();
        self.changed(cx);
    }

    /// Esc: снять «вырезано», иначе — фокус в редактор.
    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        if self
            .clipboard
            .as_ref()
            .is_some_and(|clipboard| clipboard.cut)
        {
            self.clipboard = None;
            return cx.notify();
        }
        cx.emit(FileTreeEvent::FocusEditor);
    }

    // --- Правка имени: новый файл, новый каталог, переименование ---

    fn new_entry(&mut self, kind: EntryKind, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.tree.target_dir(self.selected.as_deref());
        if dir != self.root {
            self.tree.reveal(&dir);
            self.tree.expand(&dir);
        }
        self.start_edit(EditTarget::New { dir, kind }, "", None, window, cx);
    }

    fn start_rename(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = display_name(&path);
        let stem = stem_range(&name, self.tree.is_dir(&path));
        self.start_edit(EditTarget::Rename { path }, &name, Some(stem), window, cx);
    }

    fn start_edit(
        &mut self,
        target: EditTarget,
        text: &str,
        select: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let placeholder = match &target {
            EditTarget::New {
                kind: EntryKind::Dir,
                ..
            } => "Folder name",
            EditTarget::New { .. } => "File name",
            EditTarget::Rename { .. } => "",
        };
        let input = cx.new(|cx| {
            let mut input = TextInput::new(placeholder, cx).compact();
            input.set_text(text, cx);
            match select {
                Some(range) => input.select_range(range, cx),
                None => input.select_all(cx),
            }
            input
        });
        let focus = input.focus_handle(cx);
        // Без ↵ на диске ничего не меняется: уход фокуса — отмена.
        let subscriptions = [
            cx.on_focus_out(&focus, window, |this, _, window, cx| {
                this.cancel_edit(false, window, cx)
            }),
            cx.subscribe(&input, |this, _, _: &InputEvent, cx| {
                if let Some(edit) = &mut this.edit {
                    edit.error = None;
                }
                cx.notify();
            }),
        ];
        window.focus(&focus);
        self.edit = Some(Edit {
            target,
            input,
            error: None,
            pending: false,
            _subscriptions: subscriptions,
        });
        self.changed(cx);
        let index = match self.new_entry_slot() {
            Some((at, ..)) => Some(at),
            None => self.selected_index().map(|row| self.list_index(row)),
        };
        if let Some(index) = index {
            self.scroll.scroll_to_item(index, ScrollStrategy::Center);
        }
    }

    /// Закрывает поле без изменений на диске. `refocus` — вернуть фокус в дерево (Esc); при
    /// уходе фокуса он уже там, куда его увели.
    fn cancel_edit(&mut self, refocus: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.take().is_none() {
            return;
        }
        if refocus {
            window.focus(&self.focus_handle);
        }
        self.changed(cx);
    }

    fn confirm_edit(&mut self, _: &ConfirmEdit, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = &mut self.edit else {
            return;
        };
        if edit.pending {
            return;
        }
        let name = edit.input.read(cx).text().trim().to_string();
        let target = edit.target.clone();
        if let Err(error) = check_name(&target, &name) {
            edit.error = Some(error.into());
            return cx.notify();
        }
        if let EditTarget::Rename { path } = &target
            && display_name(path) == name
        {
            return self.cancel_edit(true, window, cx);
        }
        edit.pending = true;
        let op = target.clone();
        self.run(
            move || match op {
                EditTarget::New {
                    dir,
                    kind: EntryKind::File,
                } => create_file(&dir, &name),
                EditTarget::New {
                    dir,
                    kind: EntryKind::Dir,
                } => create_dir(&dir, &name),
                EditTarget::Rename { path } => rename(&path, &name),
            },
            window,
            cx,
            |this, result, window, cx| this.finish_edit(target, result, window, cx),
        );
    }

    fn finish_edit(
        &mut self,
        target: EditTarget,
        result: io::Result<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editing = self.edit.as_ref().is_some_and(|edit| edit.target == target);
        let path = match result {
            Ok(path) => path,
            Err(err) => {
                match &mut self.edit {
                    Some(edit) if editing => {
                        edit.pending = false;
                        edit.error = Some(err.to_string().into());
                    }
                    _ => cx.emit(FileTreeEvent::Message(err.to_string().into())),
                }
                return cx.notify();
            }
        };
        if editing {
            self.edit = None;
            window.focus(&self.focus_handle);
        }
        match target {
            EditTarget::New { dir, kind } => {
                self.reread([dir], cx);
                self.select_when_shown(path.clone());
                if kind == EntryKind::File {
                    cx.emit(FileTreeEvent::Open { path, focus: true });
                }
            }
            EditTarget::Rename { path: from } => self.moved(&from, &path, cx),
        }
        self.changed(cx);
    }

    /// `from` переехал в `to` (переименование, перемещение): дерево, выбор и открытые документы —
    /// следом; оба каталога перечитываются. Тот же путь (то же имя, тот же каталог) — операция
    /// ничего не сделала, только выбор.
    fn moved(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        if from == to {
            return self.select_when_shown(to.to_path_buf());
        }
        self.tree.rename(from, to);
        if let Some(clipboard) = &mut self.clipboard
            && let Some(path) = remap(&clipboard.path, from, to)
        {
            clipboard.path = path;
        }
        let dirs: BTreeSet<PathBuf> = [from.parent(), to.parent()]
            .into_iter()
            .flatten()
            .map(Path::to_path_buf)
            .collect();
        self.reread(dirs, cx);
        self.select_when_shown(to.to_path_buf());
        cx.emit(FileTreeEvent::Moved {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
        });
    }

    // --- Операции ---

    /// Операция над файлами в фоне; `done` — с результатом в UI-потоке.
    fn run<T: Send + 'static>(
        &mut self,
        op: impl FnOnce() -> io::Result<T> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, io::Result<T>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let task = cx.background_spawn(async move { op() });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| done(this, result, window, cx))
                .ok();
        })
        .detach();
    }

    fn move_to_trash(&mut self, _: &MoveToTrash, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.selected.clone() else {
            return;
        };
        let name = display_name(&path);
        let detail = if self.tree.is_dir(&path) {
            "The folder and everything in it will be moved to the Trash."
        } else {
            "You can restore it from the Trash."
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Move “{name}” to Trash?"),
            Some(detail),
            &["Move to Trash", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            this.update_in(cx, |this, window, cx| {
                let trashed = path.clone();
                this.run(
                    move || trash(std::slice::from_ref(&trashed)),
                    window,
                    cx,
                    move |this, result, _, cx| this.finish_trash(path, result, cx),
                );
            })
            .ok();
        })
        .detach();
    }

    fn finish_trash(&mut self, path: PathBuf, result: io::Result<()>, cx: &mut Context<Self>) {
        if let Err(err) = result {
            return cx.emit(FileTreeEvent::Message(err.to_string().into()));
        }
        // Выбор — на соседа: следующую строку, у последней — предыдущую.
        let index = self.row_index(&path);
        self.tree.remove(&path);
        if self
            .selected
            .as_ref()
            .is_some_and(|selected| selected.starts_with(&path))
        {
            let rows = self.tree.rows();
            self.selected = index
                .and_then(|index| rows.get(index.min(rows.len().saturating_sub(1))))
                .map(|row| row.path.clone());
        }
        if self
            .clipboard
            .as_ref()
            .is_some_and(|c| c.path.starts_with(&path))
        {
            self.clipboard = None;
        }
        self.reread(path.parent().map(Path::to_path_buf), cx);
        cx.emit(FileTreeEvent::Removed { paths: vec![path] });
        self.changed(cx);
    }

    fn set_clipboard(&mut self, cut: bool, cx: &mut Context<Self>) {
        if let Some(path) = self.selected.clone() {
            self.clipboard = Some(Clipboard { path, cut });
            cx.notify();
        }
    }

    /// ⌘V: вырезанное переезжает в выбранный каталог (у файла — в его каталог), скопированное —
    /// копируется туда под свободным именем.
    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(clipboard) = self.clipboard.clone() else {
            return;
        };
        let dir = self.tree.target_dir(self.selected.as_deref());
        if clipboard.cut {
            self.clipboard = None;
            self.move_entry(clipboard.path, dir, window, cx);
        } else {
            self.copy_entry(clipboard.path, dir, false, window, cx);
        }
    }

    fn move_entry(
        &mut self,
        path: PathBuf,
        dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if path.parent() == Some(dir.as_path()) {
            return cx.notify();
        }
        let from = path.clone();
        self.run(
            move || move_into(&path, &dir),
            window,
            cx,
            move |this, result, _, cx| match result {
                Ok(to) => {
                    this.moved(&from, &to, cx);
                    this.changed(cx);
                }
                Err(err) => cx.emit(FileTreeEvent::Message(err.to_string().into())),
            },
        );
    }

    /// Копия в `dir`; `rename_after` — сразу переименовать её (⌘D).
    fn copy_entry(
        &mut self,
        path: PathBuf,
        dir: PathBuf,
        rename_after: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = dir.clone();
        self.run(
            move || copy_into(&path, &dir),
            window,
            cx,
            move |this, result, window, cx| match result {
                Ok(copy) => {
                    this.reread([target], cx);
                    this.select_when_shown(copy.clone());
                    this.changed(cx);
                    if rename_after {
                        this.rename_when_shown(copy, window, cx);
                    }
                }
                Err(err) => cx.emit(FileTreeEvent::Message(err.to_string().into())),
            },
        );
    }

    /// Переименование копии: её строка появится после перечитывания каталога.
    fn rename_when_shown(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.row_index(&path).is_some() {
            return self.start_rename(path, window, cx);
        }
        cx.spawn_in(window, async move |this, cx| {
            // Перечитывание — доли миллисекунды; ждём несколько кадров, не дольше.
            for _ in 0..30 {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let shown = this
                    .update_in(cx, |this, window, cx| {
                        let shown = this.row_index(&path).is_some();
                        if shown && this.edit.is_none() {
                            this.start_rename(path.clone(), window, cx);
                        }
                        shown
                    })
                    .unwrap_or(true);
                if shown {
                    break;
                }
            }
        })
        .detach();
    }

    fn duplicate(&mut self, _: &Duplicate, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.selected.clone() else {
            return;
        };
        let dir = self.tree.target_dir(path.parent());
        self.copy_entry(path, dir, true, window, cx);
    }

    /// ⌥⌘C и ⌥⇧⌘C: путь выбранного (без выбора — корня) в буфер обмена.
    fn copy_path(&mut self, relative: bool, cx: &mut Context<Self>) {
        let path = self.selected.clone().unwrap_or_else(|| self.root.clone());
        let text = if relative {
            relative_path(&self.root, &path)
        } else {
            path.display().to_string()
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn reveal_in_finder(&mut self, _: &RevealInFinder, _: &mut Window, cx: &mut Context<Self>) {
        let path = self.selected.clone().unwrap_or_else(|| self.root.clone());
        cx.reveal_path(&path);
    }

    // --- Мышь ---

    /// Щелчок по строке, как в VS Code: файл открывается, а фокус остаётся в дереве (можно
    /// листать дальше); двойной щелчок по файлу — фокус в редактор. Каталог раскрывается или
    /// сворачивается первым щелчком — второй щелчок двойного его не трогает.
    fn click_row(
        &mut self,
        path: PathBuf,
        click_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.selected = Some(path.clone());
        self.reveal_target = None;
        if !self.tree.is_dir(&path) {
            cx.emit(FileTreeEvent::Open {
                path,
                focus: click_count >= 2,
            });
        } else if click_count == 1 {
            self.activate(path, false, cx);
        }
        cx.notify();
    }

    /// Правая кнопка: выбрать строку (`None` — пустое место: корень) и открыть меню у курсора.
    fn secondary_click(
        &mut self,
        path: Option<PathBuf>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.selected = path.clone();
        self.reveal_target = None;
        let can_paste = self.clipboard.is_some();
        let menu = cx.new(|cx| {
            let menu = ContextMenu::new(window, cx)
                .entry("New File", NewFile)
                .entry("New Folder", NewFolder)
                .separator();
            let menu = match path {
                Some(_) => menu
                    .entry("Rename", Rename)
                    .entry("Duplicate", Duplicate)
                    .entry("Move to Trash", MoveToTrash)
                    .separator()
                    .entry("Cut", Cut)
                    .entry("Copy", Copy)
                    .entry_if(can_paste, "Paste", Paste)
                    .separator()
                    .entry("Copy Path", CopyPath)
                    .entry("Copy Relative Path", CopyRelativePath),
                None => menu
                    .entry_if(can_paste, "Paste", Paste)
                    .entry("Collapse All", CollapseAll)
                    .separator()
                    .entry("Copy Path", CopyPath),
            };
            menu.entry("Reveal in Finder", RevealInFinder)
        });
        let focus = menu.focus_handle(cx);
        let subscriptions = [
            cx.subscribe_in(&menu, window, |this, menu, _: &DismissEvent, window, cx| {
                this.close_menu(menu, window, cx)
            }),
            cx.on_focus_out(&focus, window, {
                let menu = menu.clone();
                move |this, _, window, cx| this.close_menu(&menu, window, cx)
            }),
        ];
        window.focus(&focus);
        self.menu = Some(Menu {
            menu,
            position,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    /// Закрывает меню (если это всё ещё оно); Esc в меню возвращает фокус в дерево.
    fn close_menu(
        &mut self,
        menu: &Entity<ContextMenu>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu.as_ref().is_none_or(|open| open.menu != *menu) {
            return;
        }
        let had_focus = menu.focus_handle(cx).contains_focused(window, cx);
        self.menu = None;
        if had_focus {
            window.focus(&self.focus_handle);
        }
        cx.notify();
    }

    /// Фокус в дереве — в нём самом, в поле правки имени или в его меню. Поле и меню
    /// проверяются напрямую: в первом кадре после создания их ещё нет в дереве фокуса.
    fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.contains_focused(window, cx)
            || self
                .edit
                .as_ref()
                .is_some_and(|edit| edit.input.focus_handle(cx).is_focused(window))
            || self
                .menu
                .as_ref()
                .is_some_and(|menu| menu.menu.focus_handle(cx).is_focused(window))
    }

    /// ⇧F10: меню выбранной строки (без выбора — корня) — под ней, как после щелчка.
    fn show_context_menu(
        &mut self,
        _: &ShowContextMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (bounds, offset) = {
            let state = self.scroll.0.borrow();
            (state.base_handle.bounds(), state.base_handle.offset())
        };
        let row = self
            .selected_index()
            .map(|index| (self.list_index(index), &self.rows[index]));
        let (top, depth) = match row {
            Some((index, row)) => (index as f32 + 1., row.depth),
            None => (0., 0),
        };
        let position = gpui::point(
            bounds.left() + px(ROW_PADDING + CHEVRON_WIDTH + depth as f32 * INDENT),
            bounds.top() + offset.y + px(top * ROW_HEIGHT),
        );
        self.secondary_click(self.selected.clone(), position, window, cx);
    }

    fn set_drop_target(&mut self, target: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.drop_target != target {
            self.drop_target = target;
            cx.notify();
        }
    }

    /// Строка списка под точкой окна — по раскладке прошлого кадра.
    fn list_index_at(&self, position: Point<Pixels>) -> Option<usize> {
        let state = self.scroll.0.borrow();
        let bounds = state.base_handle.bounds();
        if !bounds.contains(&position) {
            return None;
        }
        let y = f32::from(position.y - bounds.top() - state.base_handle.offset().y);
        let index = (y / ROW_HEIGHT).floor();
        (index >= 0.).then_some(index as usize)
    }

    /// Перетаскивание над панелью: цель — каталог строки под курсором (у файла — его
    /// каталог), под строками и над шапкой — корень; вне панели — никуда.
    fn drag_over(&mut self, event: &DragMoveEvent<DraggedEntry>, cx: &mut Context<Self>) {
        let dragged = event.drag(cx).path.clone();
        let position = event.event.position;
        let target = event.bounds.contains(&position).then(|| {
            let over = self
                .list_index_at(position)
                .and_then(|index| self.list_row(index))
                .map(|row| row.path.clone());
            drop_dir(&self.tree, &dragged, over.as_deref())
        });
        self.set_drop_target(target.flatten(), cx);
    }

    fn drop_entry(&mut self, entry: &DraggedEntry, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.drop_target.take() else {
            return;
        };
        cx.notify();
        self.move_entry(entry.path.clone(), dir, window, cx);
    }

    /// Строка дерева по номеру в списке; строка поля нового файла — `None`.
    fn list_row(&self, index: usize) -> Option<&Row> {
        match self.new_entry_slot() {
            Some((at, ..)) if index == at => None,
            Some((at, ..)) if index > at => self.rows.get(index - 1),
            _ => self.rows.get(index),
        }
    }

    // --- Отображение ---

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let name = self.root.file_name().map_or_else(
            || tilde(&self.root),
            |name| name.to_string_lossy().into_owned(),
        );
        let dropping_root = self.drop_target.as_ref() == Some(&self.root);
        div()
            .id("file-tree-header")
            .group("file-tree-header")
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .pl(px(ROW_PADDING + 4.))
            .pr_2()
            .flex()
            .items_center()
            .gap_1()
            .when(dropping_root, |header| header.bg(ui.drop_target))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.secondary_click(None, event.position, window, cx)
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(HEADER_TEXT_SIZE))
                    .text_color(ui.dim)
                    .child(name.to_uppercase()),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .gap_0p5()
                    .invisible()
                    .group_hover("file-tree-header", |style| style.visible())
                    .child(header_button(
                        "new-file",
                        "+ File",
                        ui,
                        cx,
                        |this, window, cx| this.new_entry(EntryKind::File, window, cx),
                    ))
                    .child(header_button(
                        "new-folder",
                        "+ Folder",
                        ui,
                        cx,
                        |this, window, cx| this.new_entry(EntryKind::Dir, window, cx),
                    ))
                    .child(header_button(
                        "collapse-all",
                        "⊟",
                        ui,
                        cx,
                        |this, window, cx| this.collapse_all(&CollapseAll, window, cx),
                    )),
            )
    }

    fn render_item(
        &mut self,
        index: usize,
        slot: Option<(usize, usize, EntryKind)>,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match slot {
            Some((at, depth, kind)) if index == at => self.render_new_entry(index, depth, kind, cx),
            Some((at, ..)) if index > at => self.render_row(index - 1, index, focused, cx),
            _ => self.render_row(index, index, focused, cx),
        }
    }

    fn render_row(
        &mut self,
        row_index: usize,
        list_index: usize,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let Some(row) = self.rows.get(row_index) else {
            return div().into_any_element();
        };
        let selected = self.selected.as_ref() == Some(&row.path);
        let dropping = self
            .drop_target
            .as_ref()
            .is_some_and(|target| *target != self.root && row.path.starts_with(target));
        let cut = self
            .clipboard
            .as_ref()
            .is_some_and(|clipboard| clipboard.cut && row.path.starts_with(&clipboard.path));
        let renaming = matches!(
            &self.edit,
            Some(Edit { target: EditTarget::Rename { path }, .. }) if *path == row.path
        );
        let base = row_base(list_index, row.depth)
            .when(dropping, |base| base.bg(ui.drop_target))
            .when(selected && !dropping, |base| {
                base.bg(if focused {
                    ui.list_selected
                } else {
                    ui.list_selected_inactive
                })
            })
            .when(!selected && !dropping, |base| {
                base.hover(|style| style.bg(ui.list_hover))
            })
            .child(chevron(row.kind, row.expanded, ui));
        if renaming {
            return base.child(self.render_edit_field(ui)).into_any_element();
        }
        let (click, secondary) = (row.path.clone(), row.path.clone());
        let dragged = DraggedEntry {
            path: row.path.clone(),
            name: row.name.clone().into(),
        };
        base.on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            this.click_row(click.clone(), event.click_count(), window, cx)
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                this.secondary_click(Some(secondary.clone()), event.position, window, cx)
            }),
        )
        .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(if row.ignored || cut {
                    ui.dim
                } else {
                    ui.foreground
                })
                .child(row.name.clone()),
        )
        .into_any_element()
    }

    fn render_new_entry(
        &self,
        list_index: usize,
        depth: usize,
        kind: EntryKind,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        row_base(list_index, depth)
            .child(chevron(kind, false, ui))
            .child(self.render_edit_field(ui))
            .into_any_element()
    }

    /// Поле правки имени; ошибка — под полем поверх соседних строк.
    fn render_edit_field(&self, ui: UiColors) -> AnyElement {
        let Some(edit) = &self.edit else {
            return div().into_any_element();
        };
        div()
            .key_context("FileTreeEdit")
            .relative()
            .flex_1()
            .min_w_0()
            .child(edit.input.clone())
            .children(edit.error.clone().map(|error| {
                deferred(
                    div()
                        .absolute()
                        .top(px(ROW_HEIGHT))
                        .left_0()
                        .right_0()
                        .px_2()
                        .py_0p5()
                        .bg(ui.panel)
                        .border_1()
                        .border_color(ui.error)
                        .rounded_sm()
                        .text_color(ui.error)
                        .child(error),
                )
                .with_priority(1)
            }))
            .into_any_element()
    }
}

impl Focusable for FileTreePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FileTreePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let slot = self.new_entry_slot();
        let count = self.rows.len() + usize::from(slot.is_some());
        let list = uniform_list(
            "file-tree-rows",
            count,
            cx.processor(move |this, range: Range<usize>, window, cx| {
                let focused = this.is_focused(window, cx);
                range
                    .map(|index| this.render_item(index, slot, focused, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.scroll.clone())
        .size_full();
        let dropping_root = self.drop_target.as_ref() == Some(&self.root);
        // Во время правки имени — свой контекст: клавиши дерева (стрелки, ⌫, пробел) молчат.
        let context = if self.edit.is_some() {
            "FileTree editing"
        } else {
            "FileTree"
        };
        div()
            .key_context(context)
            .track_focus(&self.focus_handle)
            .relative()
            .flex_none()
            .w(px(self.width))
            .h_full()
            .flex()
            .flex_col()
            .bg(ui.panel)
            .border_r_1()
            .border_color(ui.border)
            .font_family(theme::FONT_FAMILY)
            .text_size(px(TEXT_SIZE))
            .text_color(ui.foreground)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                this.move_selection(|current, _| current.map_or(0, |i| i + 1), cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                this.move_selection(|current, _| current.map_or(0, |i| i.saturating_sub(1)), cx)
            }))
            .on_action(
                cx.listener(|this, _: &SelectFirst, _, cx| this.move_selection(|_, _| 0, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectLast, _, cx| {
                    this.move_selection(|_, len| len - 1, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &SelectNextPage, _, cx| {
                let page = this.page_rows();
                this.move_selection(|current, _| current.map_or(0, |i| i + page), cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPreviousPage, _, cx| {
                let page = this.page_rows();
                this.move_selection(
                    |current, _| current.map_or(0, |i| i.saturating_sub(page)),
                    cx,
                )
            }))
            .on_action(cx.listener(Self::expand))
            .on_action(cx.listener(Self::collapse))
            .on_action(cx.listener(|this, _: &Open, _, cx| {
                if let Some(path) = this.selected.clone() {
                    this.activate(path, true, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &OpenKeepFocus, _, cx| {
                if let Some(path) = this.selected.clone() {
                    this.activate(path, false, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &NewFile, window, cx| {
                this.new_entry(EntryKind::File, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewFolder, window, cx| {
                this.new_entry(EntryKind::Dir, window, cx)
            }))
            .on_action(cx.listener(|this, _: &Rename, window, cx| {
                if let Some(path) = this.selected.clone() {
                    this.start_rename(path, window, cx)
                }
            }))
            .on_action(cx.listener(Self::duplicate))
            .on_action(cx.listener(Self::move_to_trash))
            .on_action(cx.listener(|this, _: &Cut, _, cx| this.set_clipboard(true, cx)))
            .on_action(cx.listener(|this, _: &Copy, _, cx| this.set_clipboard(false, cx)))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(|this, _: &CopyPath, _, cx| this.copy_path(false, cx)))
            .on_action(cx.listener(|this, _: &CopyRelativePath, _, cx| this.copy_path(true, cx)))
            .on_action(cx.listener(Self::reveal_in_finder))
            .on_action(cx.listener(Self::collapse_all))
            .on_action(cx.listener(Self::show_context_menu))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm_edit))
            .on_action(
                cx.listener(|this, _: &CancelEdit, window, cx| this.cancel_edit(true, window, cx)),
            )
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedEntry>, _, cx| {
                    this.drag_over(event, cx)
                }),
            )
            .on_drop(cx.listener(|this, entry: &DraggedEntry, window, cx| {
                this.drop_entry(entry, window, cx)
            }))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedEdge>, _, cx| {
                    let width = f32::from(event.event.position.x - event.bounds.left());
                    this.width = width.clamp(MIN_WIDTH, MAX_WIDTH);
                    cx.notify();
                }),
            )
            .child(self.render_header(cx))
            .child(
                div()
                    .id("file-tree-list")
                    .flex_1()
                    .min_h_0()
                    .when(dropping_root, |list| list.bg(ui.drop_target))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, _| window.focus(&this.focus_handle)),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            this.secondary_click(None, event.position, window, cx)
                        }),
                    )
                    .child(list),
            )
            .child(
                div()
                    .id("file-tree-resize")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(-RESIZE_HANDLE_WIDTH / 2.))
                    .w(px(RESIZE_HANDLE_WIDTH))
                    .cursor(CursorStyle::ResizeLeftRight)
                    .on_drag(DraggedEdge, |_, _, _, cx| cx.new(|_| DraggedEdge)),
            )
            .children(
                self.menu
                    .as_ref()
                    .map(|menu| ContextMenu::overlay(&menu.menu, menu.position)),
            )
    }
}

impl Render for DraggedEntry {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        div()
            .px_2()
            .py_0p5()
            .bg(ui.panel)
            .border_1()
            .border_color(ui.border)
            .rounded_sm()
            .shadow_md()
            .font_family(theme::FONT_FAMILY)
            .text_size(px(TEXT_SIZE))
            .text_color(ui.foreground)
            .child(self.name.clone())
    }
}

impl Render for DraggedEdge {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// Строка списка: высота, отступ по вложенности.
fn row_base(list_index: usize, depth: usize) -> gpui::Stateful<gpui::Div> {
    div()
        .id(list_index)
        .h(px(ROW_HEIGHT))
        .w_full()
        .flex()
        .items_center()
        .pl(px(ROW_PADDING + depth as f32 * INDENT))
        .pr_2()
        .whitespace_nowrap()
}

/// ▸/▾ у каталога; у файла — пустая колонка той же ширины.
fn chevron(kind: EntryKind, expanded: bool, ui: UiColors) -> impl IntoElement {
    let glyph = match (kind, expanded) {
        (EntryKind::Dir, true) => "▾",
        (EntryKind::Dir, false) => "▸",
        (EntryKind::File, _) => "",
    };
    div()
        .flex_none()
        .w(px(CHEVRON_WIDTH))
        .text_color(ui.dim)
        .child(glyph)
}

/// Кнопка шапки: щелчок — фокус в дерево и действие.
fn header_button(
    id: &'static str,
    label: &'static str,
    ui: UiColors,
    cx: &mut Context<FileTreePanel>,
    action: fn(&mut FileTreePanel, &mut Window, &mut Context<FileTreePanel>),
) -> impl IntoElement + use<> {
    div()
        .id(id)
        .flex_none()
        .px_1()
        .rounded_sm()
        .text_size(px(HEADER_TEXT_SIZE))
        .text_color(ui.dim)
        .hover(move |style| style.text_color(ui.foreground).bg(ui.list_hover))
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            window.focus(&this.focus_handle);
            action(this, window, cx)
        }))
        .child(label)
}

// --- Чистая логика ---

fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Что выделить при переименовании: имя без расширения (как в Finder); у каталога и у имени
/// на точку (`.gitignore`) — всё. В символах.
fn stem_range(name: &str, is_dir: bool) -> Range<usize> {
    let len = name.chars().count();
    match name.rfind('.') {
        Some(dot) if dot > 0 && !is_dir => 0..name[..dot].chars().count(),
        _ => 0..len,
    }
}

/// Проверка имени до операции. Новое имя может быть путём: `src/new/mod.rs` создаст
/// и каталоги, поэтому проверяется каждая часть.
fn check_name(target: &EditTarget, name: &str) -> Result<(), String> {
    let check = |part: &str| validate_name(part).map_err(|error| error.to_string());
    match target {
        EditTarget::Rename { .. } => check(name),
        EditTarget::New { .. } => {
            if name.trim_matches('/').is_empty() {
                return check("");
            }
            name.split('/')
                .filter(|part| !part.is_empty())
                .try_for_each(check)
        }
    }
}

/// Каталог, куда бросить `dragged`, если курсор над строкой `over` (`None` — пустое место или
/// шапка: корень): каталог — он сам, файл — его каталог. `None` — некуда: в себя, в своё
/// поддерево или туда, где он и так лежит.
fn drop_dir(tree: &FileTree, dragged: &Path, over: Option<&Path>) -> Option<PathBuf> {
    let dir = tree.target_dir(over);
    (!dir.starts_with(dragged) && dragged.parent() != Some(dir.as_path())).then_some(dir)
}

/// Путь относительно корня через `/`; сам корень — `.`.
fn relative_path(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rest) if rest.as_os_str().is_empty() => ".".into(),
        Ok(rest) => rest
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_selects_the_name_without_extension() {
        assert_eq!(stem_range("main.rs", false), 0..4);
        assert_eq!(stem_range("archive.tar.gz", false), 0..11);
        assert_eq!(stem_range(".gitignore", false), 0..10);
        assert_eq!(stem_range("Makefile", false), 0..8);
        assert_eq!(stem_range("заметки.md", false), 0..7);
        assert_eq!(stem_range("v1.2", true), 0..4);
    }

    #[test]
    fn new_names_may_be_paths_but_every_part_is_checked() {
        let new = EditTarget::New {
            dir: PathBuf::from("/p"),
            kind: EntryKind::File,
        };
        let rename = EditTarget::Rename {
            path: PathBuf::from("/p/a.rs"),
        };
        assert!(check_name(&new, "src/new/mod.rs").is_ok());
        assert!(check_name(&new, "/").is_err());
        assert!(check_name(&new, "").is_err());
        assert!(check_name(&new, "a/../b").is_err());
        assert!(check_name(&rename, "b.rs").is_ok());
        assert!(check_name(&rename, "dir/b.rs").is_err());
    }

    fn tree() -> FileTree {
        let entry = |name: &str, kind| DirEntry {
            name: name.into(),
            kind,
            ignored: false,
        };
        let mut tree = FileTree::new(PathBuf::from("/p"));
        tree.set_listing(
            Path::new("/p"),
            vec![entry("src", EntryKind::Dir), entry("a.rs", EntryKind::File)],
        );
        tree.expand(Path::new("/p/src"));
        tree.set_listing(
            Path::new("/p/src"),
            vec![
                entry("app", EntryKind::Dir),
                entry("main.rs", EntryKind::File),
            ],
        );
        tree
    }

    #[test]
    fn drop_goes_into_dirs_and_next_to_files() {
        let tree = tree();
        let p = |path: &str| PathBuf::from(path);
        // Файл из корня — в каталог и к файлу внутри него.
        assert_eq!(
            drop_dir(&tree, &p("/p/a.rs"), Some(&p("/p/src"))),
            Some(p("/p/src"))
        );
        assert_eq!(
            drop_dir(&tree, &p("/p/a.rs"), Some(&p("/p/src/main.rs"))),
            Some(p("/p/src"))
        );
        // Туда, где лежит, — некуда; на пустое место — в корень.
        assert_eq!(drop_dir(&tree, &p("/p/a.rs"), None), None);
        assert_eq!(drop_dir(&tree, &p("/p/src/main.rs"), None), Some(p("/p")));
        // Каталог — не в себя и не в своё поддерево.
        assert_eq!(drop_dir(&tree, &p("/p/src"), Some(&p("/p/src"))), None);
        assert_eq!(drop_dir(&tree, &p("/p/src"), Some(&p("/p/src/app"))), None);
        assert_eq!(
            drop_dir(&tree, &p("/p/src/app"), Some(&p("/p/src/main.rs"))),
            None
        );
    }

    #[test]
    fn relative_paths_use_slashes() {
        let root = Path::new("/p");
        assert_eq!(
            relative_path(root, Path::new("/p/src/main.rs")),
            "src/main.rs"
        );
        assert_eq!(relative_path(root, root), ".");
        assert_eq!(relative_path(root, Path::new("/q/x")), "/q/x");
    }
}
