//! Корневой вид окна: дерево файлов, вкладки с документами, открытие файлов, закрытие
//! вкладок и окна, выход — с вопросами о несохранённых изменениях.

use std::fs;
use std::path::{Path, PathBuf};

use flux_core::Document;
use flux_fs::remap;
use gpui::{
    Action, AnyView, App, AsyncApp, AsyncWindowContext, ClickEvent, Context, DismissEvent, Entity,
    FocusHandle, Focusable, Global, Hsla, KeyBinding, ManagedView, MouseButton, MouseDownEvent,
    MouseUpEvent, PathPromptOptions, PromptLevel, Render, ScrollHandle, SharedString, Subscription,
    Task, WeakEntity, Window, WindowHandle, actions, div, prelude::*, px,
};

use crate::editor::{self, Editor};
use crate::file_tree::{self, FileTreeEvent, FileTreePanel};
use crate::find_bar::{self, FindBar};
use crate::project_search::{self, ProjectSearch, ProjectSearchEvent};
use crate::theme::{self, Theme, UiColors};
use crate::{command_palette, file_finder, go_to_line};

/// Высота полосы вкладок — примерно как у статус-бара.
const TAB_BAR_HEIGHT: f32 = 28.;
const TAB_TEXT_SIZE: f32 = 12.;
/// Длинные имена файлов и каталогов на вкладке сокращаются посередине.
const TAB_LABEL_MAX_CHARS: usize = 32;
/// Отступ всплывающего окна (палитра, поиск файла) от верха окна.
const MODAL_TOP: f32 = TAB_BAR_HEIGHT + 24.;

actions!(
    workspace,
    [
        Open,
        NewFile,
        CloseTab,
        CloseWindow,
        Quit,
        NextTab,
        PrevTab,
        LastTab,
    ]
);

/// Перейти на вкладку с номером (с нуля): cmd-1…cmd-8.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = workspace, no_json)]
pub struct ActivateTab(pub usize);

pub fn init(cx: &mut App) {
    bind_keys(cx);
    cx.on_action(quit);
}

fn bind_keys(cx: &mut App) {
    let context = Some("Workspace");
    cx.bind_keys([
        KeyBinding::new("cmd-o", Open, context),
        KeyBinding::new("cmd-n", NewFile, context),
        KeyBinding::new("cmd-w", CloseTab, context),
        KeyBinding::new("cmd-shift-w", CloseWindow, context),
        KeyBinding::new("cmd-q", Quit, context),
        // macOS присылает cmd-shift-] как cmd-}: shift уже учтён в символе.
        KeyBinding::new("cmd-}", NextTab, context),
        KeyBinding::new("cmd-{", PrevTab, context),
        KeyBinding::new("cmd-shift-]", NextTab, context),
        KeyBinding::new("cmd-shift-[", PrevTab, context),
        KeyBinding::new("ctrl-tab", NextTab, context),
        KeyBinding::new("ctrl-shift-tab", PrevTab, context),
        KeyBinding::new("cmd-9", LastTab, context),
        // Фокус вне редактора (дерево файлов, поля поиска) — сохраняется активный документ.
        KeyBinding::new("cmd-s", editor::Save, context),
    ]);
    cx.bind_keys(
        (1..=8).map(|n| KeyBinding::new(&format!("cmd-{n}"), ActivateTab(n - 1), context)),
    );
}

/// Место в файле для перехода: строка с нуля и колонки в символах, выделяется `start..end`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// Корневой вид окна: вкладки (по редактору на документ) и активная из них, панели поиска,
/// всплывающие окна.
pub struct Workspace {
    /// Корень проекта: по нему ищут файлы (cmd-p) и текст (cmd-shift-f).
    root: Option<PathBuf>,
    tabs: Vec<Tab>,
    active: usize,
    /// Фокус пустого окна: без него не сработали бы cmd-o, cmd-n, cmd-w и cmd-q.
    focus_handle: FocusHandle,
    tab_scroll: ScrollHandle,
    /// Идёт разбор несохранённых документов: новые запросы закрытия игнорируются.
    closing: bool,
    /// Сколько пачек файлов ещё читается в фоне.
    loading: usize,
    /// Сообщение в пустом окне; при открытых вкладках сообщения идут в статус-бар редактора.
    notice: Option<SharedString>,
    /// Последние выставленные заголовок окна и признак «есть несохранённое».
    title: String,
    edited: bool,
    /// Всплывающее окно поверх вкладок: палитра команд, поиск файла.
    modal: Option<Modal>,
    /// Строка поиска в документе (между вкладками и текстом).
    find_bar: Entity<FindBar>,
    /// Панель поиска по проекту (под текстом).
    project_search: Entity<ProjectSearch>,
    /// Дерево файлов слева; есть, только когда есть корень проекта.
    file_tree: Option<TreePanel>,
    /// Дерево показано (cmd-b).
    tree_open: bool,
    /// Файл, последним показанный в дереве: дерево следует за активной вкладкой.
    revealed: Option<PathBuf>,
    _subscriptions: Vec<Subscription>,
}

/// Дерево файлов и подписка на его события; заводится заново со сменой корня проекта.
struct TreePanel {
    panel: Entity<FileTreePanel>,
    _subscription: Subscription,
}

struct Tab {
    editor: Entity<Editor>,
    /// Полоса вкладок и заголовок окна перерисовываются при изменениях в редакторе.
    _observer: Subscription,
}

/// Всплывающее окно. Закрывается само (`DismissEvent`: Esc, выбор), щелчком мимо него,
/// повторным вызовом того же окна или когда фокус ушёл из него (например, cmd-n открыл
/// вкладку); фокус возвращается туда, где был до открытия.
struct Modal {
    view: AnyView,
    focus_handle: FocusHandle,
    previous_focus: Option<FocusHandle>,
    _subscriptions: [Subscription; 2],
}

/// Откуда пришли пути — от этого зависит, куда сообщать об ошибках.
#[derive(Clone, Copy)]
enum Source {
    CommandLine,
    Dialog,
}

/// Файл, который не удалось открыть.
struct OpenError {
    path: PathBuf,
    reason: String,
}

impl Workspace {
    /// Окно проекта `root` с файлами из командной строки (читаются в фоне). `untitled` —
    /// без файлов начать с безымянного документа (`flux` без аргументов); `flux .` открывает
    /// пустое окно проекта.
    pub fn new(
        root: Option<PathBuf>,
        paths: Vec<PathBuf>,
        untitled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let this = cx.weak_entity();
        // Красная кнопка окна — то же, что cmd-shift-w.
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |this, cx| this.request_close_window(window, cx))
                .unwrap_or(true)
        });
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);
        let find_bar = cx.new(|cx| FindBar::new(window, cx));
        let project_search = cx.new(|cx| ProjectSearch::new(root.clone(), window, cx));
        let file_tree = root.clone().map(|root| Self::build_tree(root, window, cx));
        // Панели открываются и закрываются сами (Esc, ×) — тогда меняется и раскладка окна.
        let subscriptions = vec![
            cx.observe(&find_bar, |_, _, cx| cx.notify()),
            cx.observe(&project_search, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &project_search,
                window,
                |this, _, event, window, cx| match event {
                    ProjectSearchEvent::Open { location, focus } => {
                        this.open_location(location.clone(), *focus, window, cx)
                    }
                    ProjectSearchEvent::FocusEditor => this.focus_active(window, cx),
                },
            ),
        ];
        let mut workspace = Self {
            root,
            tabs: Vec::new(),
            active: 0,
            focus_handle,
            tab_scroll: ScrollHandle::new(),
            closing: false,
            loading: 0,
            notice: None,
            title: String::new(),
            edited: false,
            modal: None,
            find_bar,
            project_search,
            file_tree,
            tree_open: true,
            revealed: None,
            _subscriptions: subscriptions,
        };
        if untitled || !paths.is_empty() {
            workspace.open_paths(paths, Source::CommandLine, window, cx);
        }
        workspace
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// Новый корень проекта (cmd-o с каталогом).
    fn set_root(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let root = fs::canonicalize(&root).unwrap_or(root);
        self.project_search
            .update(cx, |search, cx| search.set_root(Some(root.clone()), cx));
        self.file_tree = Some(Self::build_tree(root.clone(), window, cx));
        self.revealed = None;
        self.show_message(format!("Project: {}", tilde(&root)).into(), cx);
        self.root = Some(root);
        self.reveal_active(cx);
        cx.notify();
    }

    fn editors(&self) -> Vec<Entity<Editor>> {
        self.tabs.iter().map(|tab| tab.editor.clone()).collect()
    }

    pub(crate) fn active_editor(&self) -> Option<Entity<Editor>> {
        self.tabs.get(self.active).map(|tab| tab.editor.clone())
    }

    fn index_of(&self, editor: &Entity<Editor>) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.editor == *editor)
    }

    // --- Вкладки ---

    /// Делает вкладку активной и переводит фокус в её редактор.
    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let editor = tab.editor.clone();
        window.focus(&editor.focus_handle(cx));
        self.active = index;
        self.tab_scroll.scroll_to_item(index);
        self.find_bar.update(cx, |bar, cx| {
            bar.set_active_editor(Some(editor), window, cx)
        });
        self.reveal_active(cx);
        cx.notify();
    }

    /// Фокус — в активный редактор, а без вкладок — на само окно.
    fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_editor() {
            Some(editor) => window.focus(&editor.focus_handle(cx)),
            None => window.focus(&self.focus_handle),
        }
    }

    /// gpui прокручивает полосу к вкладке, только если полоса уже была нарисована; в её
    /// первом кадре запрос теряется (запуск с множеством файлов). Тогда повторяем его
    /// перед следующим кадром.
    fn retry_tab_scroll(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_scroll.bounds().size.width > px(0.) {
            return;
        }
        cx.on_next_frame(window, |this, _, cx| {
            this.tab_scroll.scroll_to_item(this.active);
            cx.notify();
        });
    }

    fn activate_editor(
        &mut self,
        editor: &Entity<Editor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.index_of(editor) {
            self.activate(index, window, cx);
        }
    }

    /// Соседняя вкладка по кругу: `step` = 1 — следующая, -1 — предыдущая.
    fn cycle(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.tabs.len() as isize;
        if len > 0 {
            let index = (self.active as isize + step).rem_euclid(len);
            self.activate(index as usize, window, cx);
        }
    }

    /// Новая вкладка справа от активной; она и становится активной.
    fn add_document(&mut self, document: Document, window: &mut Window, cx: &mut Context<Self>) {
        let editor = cx.new(|cx| Editor::new(document, window, cx));
        // Путь документа меняется при «Сохранить как» — дерево показывает новый файл.
        let observer = cx.observe(&editor, |this, _, cx| {
            this.reveal_active(cx);
            cx.notify()
        });
        let index = if self.tabs.is_empty() {
            0
        } else {
            self.active + 1
        };
        self.tabs.insert(
            index,
            Tab {
                editor,
                _observer: observer,
            },
        );
        self.notice = None;
        self.activate(index, window, cx);
    }

    /// Убирает вкладку. Если она была активной, фокус уходит в соседнюю (правую, у последней —
    /// левую); без вкладок — на само окно.
    fn remove_tab(&mut self, editor: &Entity<Editor>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.index_of(editor) else {
            return;
        };
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.active = 0;
            window.focus(&self.focus_handle);
            self.find_bar
                .update(cx, |bar, cx| bar.set_active_editor(None, window, cx));
            return cx.notify();
        }
        let active = if index < self.active {
            self.active - 1
        } else {
            self.active.min(self.tabs.len() - 1)
        };
        self.activate(active, window, cx);
    }

    // --- Открытие ---

    /// cmd-o: файлы открываются во вкладках, выбранный каталог становится корнем проекта.
    fn open(&mut self, _: &Open, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                let (dirs, files): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| p.is_dir());
                if let Some(dir) = dirs.into_iter().last() {
                    this.set_root(dir, window, cx);
                }
                this.open_paths(files, Source::Dialog, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// Открывает файлы (как из cmd-o): уже открытые только активируются.
    pub fn open_files(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.open_paths(paths, Source::Dialog, window, cx);
    }

    /// Открывает файл (или активирует его вкладку) и выделяет место в нём. `focus == false` —
    /// фокус остаётся там, где был (например, в панели результатов поиска).
    pub fn open_location(
        &mut self,
        location: Location,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = location.path.clone();
        self.open_and(path, focus, window, cx, move |this, cx| {
            this.select_location(&location, cx)
        });
    }

    /// Открывает файл (или активирует его вкладку). `focus == false` — фокус остаётся там,
    /// где был (в дереве файлов).
    pub fn open_file(
        &mut self,
        path: PathBuf,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_and(path, focus, window, cx, |_, _| {});
    }

    /// Открывает или активирует файл, затем `then` с ним в активной вкладке. Без `focus`
    /// фокус возвращается туда, где был до открытия.
    fn open_and(
        &mut self,
        path: PathBuf,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) {
        let keep_focus = window.focused(cx).filter(|_| !focus);
        if self.activate_path(&path, window, cx) {
            then(self, cx);
            return restore_focus(keep_focus, window);
        }
        self.loading += 1;
        let read = cx.background_executor().spawn({
            let path = path.clone();
            async move { read_document(path) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = read.await;
            this.update_in(cx, |this, window, cx| {
                this.loading -= 1;
                this.finish_open(vec![result], Source::Dialog, window, cx);
                if this.activate_path(&path, window, cx) {
                    then(this, cx);
                    restore_focus(keep_focus, window);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Выделяет место в активном редакторе.
    fn select_location(&mut self, location: &Location, cx: &mut Context<Self>) {
        if let Some(editor) = self.active_editor() {
            editor.update(cx, |editor, cx| {
                let start = editor.position(location.line, location.start);
                let end = editor.position(location.line, location.end);
                editor.select_range(start..end, cx);
            });
        }
    }

    /// Открывает файлы во вкладках по порядку. Уже открытые только активируются,
    /// остальные читаются в фоне, чтобы большой файл не блокировал окно.
    fn open_paths(
        &mut self,
        paths: Vec<PathBuf>,
        source: Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths: Vec<PathBuf> = paths
            .into_iter()
            .filter(|path| !self.activate_path(path, window, cx))
            .collect();
        if paths.is_empty() {
            return self.finish_open(Vec::new(), source, window, cx);
        }
        self.loading += 1;
        let read = cx
            .background_executor()
            .spawn(async move { paths.into_iter().map(read_document).collect::<Vec<_>>() });
        cx.spawn_in(window, async move |this, cx| {
            let results = read.await;
            this.update_in(cx, |this, window, cx| {
                this.loading -= 1;
                this.finish_open(results, source, window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Если файл уже открыт (сравниваем канонические пути), активирует его вкладку.
    fn activate_path(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let target = canonical(path);
        let found = self.tabs.iter().position(|tab| {
            let document = &tab.editor.read(cx).document;
            document
                .path()
                .is_some_and(|path| canonical(path) == target)
        });
        if let Some(index) = found {
            self.activate(index, window, cx);
        }
        found.is_some()
    }

    fn finish_open(
        &mut self,
        results: Vec<Result<Document, OpenError>>,
        source: Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut errors = Vec::new();
        for result in results {
            match result {
                Ok(document) => self.add_or_activate(document, window, cx),
                Err(error) => errors.push(error),
            }
        }
        match source {
            Source::CommandLine => self.report_startup(errors, window, cx),
            Source::Dialog => self.report_dialog(errors, cx),
        }
    }

    /// Файл могли выбрать дважды, пока он читался, — тогда активируем уже открытую вкладку.
    fn add_or_activate(&mut self, document: Document, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = document.path()
            && self.activate_path(path, window, cx)
        {
            return;
        }
        self.add_document(document, window, cx);
    }

    /// Ошибки командной строки — в stderr. Если не открылось ничего — безымянная вкладка.
    fn report_startup(
        &mut self,
        errors: Vec<OpenError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for error in errors {
            eprintln!("flux: {}: {}", error.path.display(), error.reason);
        }
        if self.tabs.is_empty() {
            self.add_document(Document::from_text(""), window, cx);
        }
    }

    fn report_dialog(&mut self, errors: Vec<OpenError>, cx: &mut Context<Self>) {
        if errors.is_empty() {
            return;
        }
        let message = errors
            .iter()
            .map(|error| format!("Cannot open {}: {}", file_name(&error.path), error.reason))
            .collect::<Vec<_>>()
            .join("; ");
        self.show_message(message.into(), cx);
    }

    /// Сообщение пользователю: в статус-баре активного редактора, а в пустом окне — под подсказкой.
    pub(crate) fn show_message(&mut self, message: SharedString, cx: &mut Context<Self>) {
        match self.active_editor() {
            Some(editor) => editor.update(cx, |editor, cx| editor.show_status(message, cx)),
            None => {
                self.notice = Some(message);
                cx.notify();
            }
        }
    }

    // --- Закрытие ---

    fn close_active_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_editor() {
            Some(editor) => self.close_tab(editor, window, cx),
            // В пустом окне cmd-w закрывает само окно.
            None => window.remove_window(),
        }
    }

    /// Закрывает вкладку; изменённую — только после ответа на вопрос о сохранении.
    fn close_tab(&mut self, editor: Entity<Editor>, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        if !is_modified(&editor, cx) {
            return self.remove_tab(&editor, window, cx);
        }
        let confirm = self.confirm(vec![editor.clone()], window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if confirm.await {
                this.update_in(cx, |this, window, cx| this.remove_tab(&editor, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn close_window(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
        if self.request_close_window(window, cx) {
            window.remove_window();
        }
    }

    /// `true` — окно можно закрыть сразу. Иначе спрашивает про несохранённые документы
    /// и закрывает окно само, если пользователь ничего не отменил.
    fn request_close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.closing {
            return false;
        }
        if !self.tabs.iter().any(|tab| is_modified(&tab.editor, cx)) {
            return true;
        }
        let confirm = self.confirm(self.editors(), window, cx);
        cx.spawn_in(window, async move |_, cx| {
            if confirm.await {
                cx.update(|window, _| window.remove_window()).ok();
            }
        })
        .detach();
        false
    }

    /// Спрашивает по очереди про каждый изменённый документ из `editors`, показывая его вкладку.
    /// `true` — все разобраны (сохранены или брошены); `false` — отмена, ошибка сохранения
    /// или уже идёт другой разбор.
    fn confirm(
        &mut self,
        editors: Vec<Entity<Editor>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        if self.closing {
            return Task::ready(false);
        }
        let modified: Vec<_> = editors.into_iter().filter(|e| is_modified(e, cx)).collect();
        if modified.is_empty() {
            return Task::ready(true);
        }
        self.closing = true;
        window.activate_window();
        cx.spawn_in(window, async move |this, cx| {
            let confirmed = ask_each(&this, modified, cx).await;
            this.update(cx, |this, _| this.closing = false).ok();
            confirmed
        })
    }

    /// cmd-f / cmd-alt-f: строка поиска для активной вкладки.
    fn deploy_find(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        let editor = self.active_editor();
        self.find_bar
            .update(cx, |bar, cx| bar.deploy(replace, editor, window, cx));
    }

    // --- Дерево файлов ---

    fn build_tree(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> TreePanel {
        let panel = cx.new(|cx| FileTreePanel::new(root, window, cx));
        let subscription = cx.subscribe_in(&panel, window, Self::on_tree_event);
        TreePanel {
            panel,
            _subscription: subscription,
        }
    }

    fn on_tree_event(
        &mut self,
        _: &Entity<FileTreePanel>,
        event: &FileTreeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            FileTreeEvent::Open { path, focus } => self.open_file(path.clone(), *focus, window, cx),
            FileTreeEvent::FocusEditor => self.focus_active(window, cx),
            FileTreeEvent::Moved { from, to } => self.documents_moved(from, to, cx),
            FileTreeEvent::Removed { paths } => self.documents_removed(paths, window, cx),
            FileTreeEvent::Message(message) => self.show_message(message.clone(), cx),
        }
    }

    fn tree_panel(&self) -> Option<Entity<FileTreePanel>> {
        self.file_tree.as_ref().map(|tree| tree.panel.clone())
    }

    /// Дерево следует за активной вкладкой: её файл раскрыт и выделен. Зовётся при смене
    /// вкладки и любом изменении редактора — дерево трогаем, только когда сменился файл.
    fn reveal_active(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.tree_panel().filter(|_| self.tree_open) else {
            return;
        };
        let path = self
            .active_editor()
            .and_then(|editor| editor.read(cx).document.path().map(Path::to_path_buf));
        if path == self.revealed {
            return;
        }
        if let Some(path) = &path {
            panel.update(cx, |panel, cx| panel.reveal(path, cx));
        }
        self.revealed = path;
    }

    /// cmd-b: показать или скрыть дерево файлов.
    fn toggle_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.tree_panel() else {
            return self.show_message("No project folder — open one with ⌘O".into(), cx);
        };
        let focused = panel.focus_handle(cx).contains_focused(window, cx);
        self.tree_open = !self.tree_open;
        if !self.tree_open && focused {
            self.focus_active(window, cx);
        }
        self.revealed = None;
        self.reveal_active(cx);
        cx.notify();
    }

    /// cmd-shift-e: фокус в дерево файлов (показав его), из дерева — обратно в редактор.
    fn toggle_tree_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.tree_panel() else {
            return self.show_message("No project folder — open one with ⌘O".into(), cx);
        };
        let handle = panel.focus_handle(cx);
        if self.tree_open && handle.contains_focused(window, cx) {
            return self.focus_active(window, cx);
        }
        if !self.tree_open {
            self.tree_open = true;
            self.revealed = None;
            self.reveal_active(cx);
        }
        window.focus(&handle);
        cx.notify();
    }

    /// Файл или каталог переехал (переименован, перемещён в дереве): открытые документы
    /// внутри него — на новые пути.
    fn documents_moved(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        for editor in self.editors() {
            let moved = editor
                .read(cx)
                .document
                .path()
                .and_then(|path| remap(path, from, to));
            if let Some(path) = moved {
                editor.update(cx, |editor, cx| editor.set_path(path, cx));
            }
        }
    }

    /// Файлы удалены в Корзину: их вкладки закрываются. Изменённые остаются открытыми —
    /// правки не теряются, сохранение создаст файл заново.
    fn documents_removed(
        &mut self,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Закрытие вкладки активирует соседнюю и уводит в неё фокус, а удаляли из дерева —
        // фокус должен остаться там (если не был в закрытой вкладке).
        let mut keep_focus = window.focused(cx);
        let mut kept = false;
        for editor in self.editors() {
            let (removed, modified) = {
                let document = &editor.read(cx).document;
                let removed = document
                    .path()
                    .is_some_and(|path| paths.iter().any(|removed| path.starts_with(removed)));
                (removed, document.is_modified())
            };
            match (removed, modified) {
                (true, true) => kept = true,
                (true, false) => {
                    if keep_focus.as_ref() == Some(&editor.focus_handle(cx)) {
                        keep_focus = None;
                    }
                    self.remove_tab(&editor, window, cx)
                }
                _ => {}
            }
        }
        restore_focus(keep_focus, window);
        if kept {
            self.show_message(
                "Deleted files with unsaved changes stay open — save to restore them".into(),
                cx,
            );
        }
    }

    // --- Всплывающие окна ---

    /// Открывает всплывающее окно `V`; если оно уже открыто — закрывает. Другое открытое
    /// окно заменяется. `build` вызывается до переноса фокуса: в нём `window.focused` — тот,
    /// кто был в фокусе (палитре команд это нужно, чтобы собрать доступные ему действия).
    pub fn toggle_modal<V: ManagedView>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        build: impl FnOnce(&mut Window, &mut Context<V>) -> V,
    ) {
        if let Some(modal) = &self.modal
            && modal.view.clone().downcast::<V>().is_ok()
        {
            return self.dismiss_modal(window, cx);
        }
        let previous_focus = match self.modal.take() {
            Some(modal) => modal.previous_focus,
            None => window.focused(cx),
        };
        let view = cx.new(|cx| build(window, cx));
        let focus_handle = view.focus_handle(cx);
        let subscriptions = [
            cx.subscribe_in(&view, window, |this, _, _: &DismissEvent, window, cx| {
                this.dismiss_modal(window, cx)
            }),
            cx.on_focus_out(&focus_handle, window, |this, _, window, cx| {
                this.dismiss_modal(window, cx)
            }),
        ];
        window.focus(&focus_handle);
        self.modal = Some(Modal {
            view: view.into(),
            focus_handle,
            previous_focus,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    /// Закрывает всплывающее окно. Фокус возвращается на прежнее место, только если он был
    /// в окне: при щелчке мимо фокус уже ушёл туда, куда щёлкнули.
    pub fn dismiss_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        if modal.focus_handle.contains_focused(window, cx) || window.focused(cx).is_none() {
            match (modal.previous_focus, self.active_editor()) {
                (Some(previous), _) => window.focus(&previous),
                (None, Some(editor)) => window.focus(&editor.focus_handle(cx)),
                (None, None) => window.focus(&self.focus_handle),
            }
        }
        cx.notify();
    }

    fn render_modal(&self, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        let modal = self.modal.as_ref()?;
        Some(
            div()
                .absolute()
                .top(px(MODAL_TOP))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .on_mouse_down_out(
                            cx.listener(|this, _, window, cx| this.dismiss_modal(window, cx)),
                        )
                        .child(modal.view.clone()),
                ),
        )
    }

    // --- Отображение ---

    /// Заголовок окна «● имя — проект» по активной вкладке (без проекта — «— flux») и точка
    /// на красной кнопке, если есть несохранённое. Платформу дёргаем, только когда что-то
    /// поменялось.
    fn update_title(&mut self, window: &mut Window, cx: &App) {
        let project = self.root.as_deref().and_then(Path::file_name).map_or_else(
            || "flux".to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let title = self.active_editor().map_or_else(
            || project.clone(),
            |editor| {
                let document = &editor.read(cx).document;
                let modified = if document.is_modified() { "● " } else { "" };
                format!("{modified}{} — {project}", document.display_name())
            },
        );
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
        let edited = self.tabs.iter().any(|tab| is_modified(&tab.editor, cx));
        if edited != self.edited {
            window.set_window_edited(edited);
            self.edited = edited;
        }
    }

    fn render_tab_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let paths: Vec<Option<PathBuf>> = self
            .tabs
            .iter()
            .map(|tab| tab.editor.read(cx).document.path().map(Path::to_path_buf))
            .collect();
        let tabs: Vec<_> = self
            .tabs
            .iter()
            .zip(tab_details(&paths))
            .enumerate()
            .map(|(index, (tab, detail))| self.render_tab(index, tab, detail, cx))
            .collect();
        div()
            .relative()
            .flex_none()
            .h(px(TAB_BAR_HEIGHT))
            .bg(ui.tab_bar)
            .text_size(px(TAB_TEXT_SIZE))
            // Линия под вкладками; активная вкладка перекрывает её и сливается с текстом.
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(1.))
                    .bg(ui.border),
            )
            .child(
                div()
                    .id("tabs")
                    .size_full()
                    .flex()
                    .overflow_x_scroll()
                    .track_scroll(&self.tab_scroll)
                    .children(tabs),
            )
    }

    fn render_tab(
        &self,
        index: usize,
        tab: &Tab,
        detail: Option<String>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let document = &tab.editor.read(cx).document;
        let active = index == self.active;
        let (activate, close) = (tab.editor.clone(), tab.editor.clone());
        div()
            .id(("tab", tab.editor.entity_id()))
            .group("tab")
            .relative()
            .flex_none()
            .h_full()
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .border_r_1()
            .border_color(ui.border)
            .text_color(if active { ui.foreground } else { ui.dim })
            .when(active, |el| {
                el.bg(ui.background).child(accent_line(ui.tab_accent))
            })
            .when(!active, |el| {
                el.hover(|style| style.text_color(ui.foreground))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    this.activate_editor(&activate, window, cx)
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, _: &MouseUpEvent, window, cx| {
                    this.close_tab(close.clone(), window, cx)
                }),
            )
            .child(label(&document.display_name()))
            .children(detail.map(|detail| label(&detail).text_color(ui.dim)))
            .child(close_button(
                tab.editor.clone(),
                active,
                document.is_modified(),
                cx,
            ))
    }

    fn render_empty(&self, ui: UiColors) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .text_color(ui.dim)
            // Пока читаются файлы из командной строки, подсказку не показываем.
            .when(self.loading == 0, |empty| {
                empty
                    .child("⌘P find file · ⌘⇧F search · ⌘⇧E files · ⌘O open · ⌘N new")
                    .children(
                        self.root
                            .as_deref()
                            .map(|root| div().text_size(px(TAB_TEXT_SIZE)).child(tilde(root))),
                    )
            })
            .children(
                self.notice
                    .clone()
                    .map(|notice| div().text_color(ui.error).child(notice)),
            )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.update_title(window, cx);
        let ui = Theme::ui(cx);
        let root = div()
            .key_context("Workspace")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(ui.background)
            .text_color(ui.foreground)
            .font_family(theme::FONT_FAMILY)
            .text_size(px(theme::FONT_SIZE))
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(|this, _: &NewFile, window, cx| {
                this.add_document(Document::from_text(""), window, cx)
            }))
            // Esc в редакторе: открытая строка поиска закрывается раньше, чем редактор снимет
            // выделение; когда снимать нечего (редактор пропускает Esc дальше) — панель поиска
            // по проекту.
            .capture_action(cx.listener(|this, _: &editor::Cancel, window, cx| {
                if this.find_bar.read(cx).is_open() {
                    this.find_bar.update(cx, |bar, cx| bar.close(window, cx));
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &editor::Cancel, _, cx| {
                if this.project_search.read(cx).is_open() {
                    this.project_search
                        .update(cx, |search, cx| search.close(cx));
                }
            }))
            .on_action(cx.listener(|this, _: &editor::Save, _, cx| {
                if let Some(editor) = this.active_editor() {
                    editor.update(cx, |editor, cx| editor.save(cx).detach());
                }
            }))
            .on_action(cx.listener(Self::close_active_tab))
            .on_action(cx.listener(Self::close_window))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle(1, window, cx)))
            .on_action(cx.listener(|this, _: &PrevTab, window, cx| this.cycle(-1, window, cx)))
            .on_action(cx.listener(|this, action: &ActivateTab, window, cx| {
                this.activate(action.0, window, cx)
            }))
            .on_action(cx.listener(|this, _: &LastTab, window, cx| {
                this.activate(this.tabs.len().saturating_sub(1), window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &command_palette::Toggle, window, cx| {
                    command_palette::toggle(this, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &file_finder::Toggle, window, cx| {
                file_finder::toggle(this, window, cx)
            }))
            .on_action(cx.listener(|this, _: &go_to_line::Toggle, window, cx| {
                go_to_line::toggle(this, window, cx)
            }))
            .on_action(cx.listener(|this, _: &find_bar::Deploy, window, cx| {
                this.deploy_find(false, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &find_bar::DeployReplace, window, cx| {
                    this.deploy_find(true, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &find_bar::FindNext, window, cx| {
                this.find_bar
                    .update(cx, |bar, cx| bar.select_next(false, window, cx))
            }))
            .on_action(cx.listener(|this, _: &find_bar::FindPrevious, window, cx| {
                this.find_bar
                    .update(cx, |bar, cx| bar.select_next(true, window, cx))
            }))
            .on_action(cx.listener(|this, _: &file_tree::ToggleOpen, window, cx| {
                this.toggle_tree(window, cx)
            }))
            .on_action(cx.listener(|this, _: &file_tree::ToggleFocus, window, cx| {
                this.toggle_tree_focus(window, cx)
            }))
            .on_action(cx.listener(|this, _: &project_search::Toggle, window, cx| {
                let seed = this
                    .active_editor()
                    .and_then(|editor| editor.read(cx).search_seed());
                this.project_search
                    .update(cx, |search, cx| search.toggle(seed, window, cx))
            }));
        // Слева — дерево файлов; справа сверху вниз: вкладки, строка поиска, текст, панель
        // поиска по проекту. Под ними во всю ширину — статус-бар.
        let active = self.active_editor();
        let main = div().flex_1().min_w_0().h_full().flex().flex_col();
        let main = match &active {
            Some(editor) => {
                self.retry_tab_scroll(window, cx);
                main.child(self.render_tab_bar(cx))
                    .when(self.find_bar.read(cx).is_open(), |main| {
                        main.child(self.find_bar.clone())
                    })
                    .child(div().flex_1().min_h_0().child(editor.clone()))
            }
            None => main.child(self.render_empty(ui)),
        };
        let main = main.when(self.project_search.read(cx).is_open(), |main| {
            main.child(self.project_search.clone())
        });
        let tree = self.tree_panel().filter(|_| self.tree_open);
        root.child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_row()
                .children(tree)
                .child(main),
        )
        .children(active.map(|editor| editor.read(cx).render_status_bar(ui)))
        .children(self.render_modal(cx))
    }
}

// --- Вопросы о несохранённых документах ---

async fn ask_each(
    this: &WeakEntity<Workspace>,
    editors: Vec<Entity<Editor>>,
    cx: &mut AsyncWindowContext,
) -> bool {
    for editor in editors {
        if !ask_to_save(this, editor, cx).await {
            return false;
        }
    }
    true
}

/// Save / Don't Save / Cancel для одного документа; Save без пути — «Сохранить как».
/// `true` — документ сохранён или его разрешили бросить.
async fn ask_to_save(
    this: &WeakEntity<Workspace>,
    editor: Entity<Editor>,
    cx: &mut AsyncWindowContext,
) -> bool {
    // Пока очередь дошла до документа, его могли сохранить.
    let Ok(Some(name)) = editor.read_with(cx, |editor, _| {
        let document = &editor.document;
        document.is_modified().then(|| document.display_name())
    }) else {
        return true;
    };
    let shown = this.update_in(cx, |this, window, cx| {
        this.activate_editor(&editor, window, cx)
    });
    if shown.is_err() {
        return false;
    }
    let answer = cx.prompt(
        PromptLevel::Warning,
        &format!("Save changes to {name}?"),
        Some("Your changes will be lost if you don’t save them."),
        &["Save", "Don’t Save", "Cancel"],
    );
    match answer.await {
        Ok(0) => match editor.update(cx, |editor, cx| editor.save(cx)) {
            Ok(saving) => saving.await,
            Err(_) => false,
        },
        Ok(1) => true,
        _ => false,
    }
}

/// Выход уже идёт: повторный cmd-q, пока открыты диалоги, второй разбор не начинает.
#[derive(Default)]
struct Quitting(bool);

impl Global for Quitting {}

/// Выход: окна по очереди разбирают несохранённые документы; отказ в любом отменяет выход.
fn quit(_: &Quit, cx: &mut App) {
    let quitting = cx.default_global::<Quitting>();
    if quitting.0 {
        return;
    }
    quitting.0 = true;
    let windows: Vec<WindowHandle<Workspace>> = cx
        .windows()
        .into_iter()
        .filter_map(|window| window.downcast())
        .collect();
    cx.spawn(async move |cx| {
        let confirmed = confirm_windows(windows, cx).await;
        cx.update(|cx| {
            if confirmed {
                cx.quit();
            } else {
                cx.global_mut::<Quitting>().0 = false;
            }
        })
        .ok();
    })
    .detach();
}

async fn confirm_windows(windows: Vec<WindowHandle<Workspace>>, cx: &mut AsyncApp) -> bool {
    for handle in windows {
        let confirm = handle.update(cx, |workspace, window, cx| {
            workspace.confirm(workspace.editors(), window, cx)
        });
        // Окно успели закрыть — спрашивать не о чем.
        let Ok(confirm) = confirm else {
            continue;
        };
        if !confirm.await {
            return false;
        }
    }
    true
}

// --- Мелочи ---

/// Возвращает фокус туда, где он был (если был запомнен).
fn restore_focus(focus: Option<FocusHandle>, window: &mut Window) {
    if let Some(focus) = focus {
        window.focus(&focus);
    }
}

fn is_modified(editor: &Entity<Editor>, cx: &App) -> bool {
    editor.read(cx).document.is_modified()
}

/// Читает файл (в фоновом потоке). Каталог как файл не открывается: он может быть только
/// корнем проекта.
fn read_document(path: PathBuf) -> Result<Document, OpenError> {
    if path.is_dir() {
        let reason = "is a directory".into();
        return Err(OpenError { path, reason });
    }
    Document::open(&path).map_err(|err| OpenError {
        path,
        reason: err.to_string(),
    })
}

/// Путь для сравнения «тот же ли это файл»: канонический, а для ещё не созданного
/// файла — канонический каталог плюс имя.
fn canonical(path: &Path) -> PathBuf {
    if let Ok(path) = fs::canonicalize(path) {
        return path;
    }
    match (
        path.parent().and_then(|dir| fs::canonicalize(dir).ok()),
        path.file_name(),
    ) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => path.to_path_buf(),
    }
}

/// Путь для показа: домашний каталог — как `~`.
pub(crate) fn tilde(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Подписи каталогов для вкладок с одинаковыми именами файлов: столько последних
/// компонентов пути каталога, сколько нужно, чтобы отличить файл от тёзок.
fn tab_details(paths: &[Option<PathBuf>]) -> Vec<Option<String>> {
    let all: Vec<&Path> = paths.iter().flatten().map(PathBuf::as_path).collect();
    paths
        .iter()
        .map(|path| {
            let path = path.as_deref()?;
            let namesakes: Vec<&Path> = all
                .iter()
                .copied()
                .filter(|other| *other != path && other.file_name() == path.file_name())
                .collect();
            if namesakes.is_empty() {
                return None;
            }
            let depth = path.parent().map_or(0, |dir| dir.components().count());
            let distinct = |n: &usize| {
                namesakes
                    .iter()
                    .all(|o| dir_tail(o, *n) != dir_tail(path, *n))
            };
            let n = (1..=depth).find(distinct).unwrap_or(depth);
            Some(dir_tail(path, n).display().to_string())
        })
        .collect()
}

/// Последние `n` компонентов каталога, в котором лежит файл.
fn dir_tail(path: &Path, n: usize) -> PathBuf {
    let dir: Vec<_> = path
        .parent()
        .map(|dir| dir.components().collect())
        .unwrap_or_default();
    dir[dir.len().saturating_sub(n)..].iter().collect()
}

fn label(text: &str) -> gpui::Div {
    div()
        .whitespace_nowrap()
        .child(shorten(text, TAB_LABEL_MAX_CHARS))
}

/// Сокращает посередине: «начало…конец» — так видны и начало имени, и расширение.
/// Шрифт моноширинный, поэтому число символов — это и ширина. (Многоточие средствами
/// gpui здесь не работает: в прокручиваемой полосе ширина текста не ограничена.)
fn shorten(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_string();
    }
    let head = (max_chars - 1) / 2;
    let tail = max_chars - 1 - head;
    let mut short: String = chars[..head].iter().collect();
    short.push('…');
    short.extend(&chars[chars.len() - tail..]);
    short
}

/// Цветная полоска сверху активной вкладки.
fn accent_line(color: Hsla) -> impl IntoElement {
    div()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(2.))
        .bg(color)
}

/// Правый край вкладки: «●» у изменённой, «×» — при наведении (у активной без изменений — всегда).
fn close_button(
    editor: Entity<Editor>,
    active: bool,
    modified: bool,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let ui = Theme::ui(cx);
    let close = div()
        .id("close")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .hover(move |style| style.bg(ui.border).text_color(ui.foreground))
        .when(modified || !active, |close| {
            close
                .invisible()
                .group_hover("tab", |style| style.visible())
        })
        // Нажатие на «×» не должно активировать вкладку.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.close_tab(editor.clone(), window, cx)
        }))
        .child("×");
    let dot = modified.then(|| {
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .group_hover("tab", |style| style.invisible())
            .child("●")
    });
    div()
        .relative()
        .flex_none()
        .size(px(16.))
        .children(dot)
        .child(close)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn details(paths: &[&str]) -> Vec<Option<String>> {
        let paths: Vec<Option<PathBuf>> = paths.iter().map(|p| Some(PathBuf::from(p))).collect();
        tab_details(&paths)
    }

    #[test]
    fn unique_names_have_no_detail() {
        assert_eq!(details(&["/a/main.rs", "/a/lib.rs"]), vec![None, None]);
    }

    #[test]
    fn namesakes_get_parent_dir() {
        assert_eq!(
            details(&["/x/core/lib.rs", "/x/app/lib.rs", "/x/app/main.rs"]),
            vec![Some("core".into()), Some("app".into()), None]
        );
    }

    #[test]
    fn same_parent_name_goes_deeper() {
        assert_eq!(
            details(&["/x/core/src/lib.rs", "/x/app/src/lib.rs"]),
            vec![Some("core/src".into()), Some("app/src".into())]
        );
    }

    #[test]
    fn long_labels_are_shortened_in_the_middle() {
        assert_eq!(shorten("main.rs", 9), "main.rs");
        assert_eq!(shorten("abcdefghij.md", 9), "abcd…j.md");
        assert_eq!(shorten("абвгдеёжзий", 5), "аб…ий");
    }

    #[test]
    fn home_is_shown_as_tilde() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(tilde(&home), "~");
        assert_eq!(tilde(&home.join("dev/flux")), "~/dev/flux");
        assert_eq!(tilde(Path::new("/opt/x")), "/opt/x");
    }

    #[test]
    fn untitled_documents_are_ignored() {
        let paths = vec![None, Some(PathBuf::from("/a/lib.rs")), None];
        assert_eq!(tab_details(&paths), vec![None, None, None]);
    }
}
