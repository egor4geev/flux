//! The window: title bar, status bar, tabs, launchpad, window dialogs, editor messages.

pub(super) const STRINGS: &[(&str, &str)] = &[
    (
        "No project — open a folder with {0}",
        "Нет проекта — откройте папку через {0}",
    ),
    (
        "Deleted files with unsaved changes stay open — save to restore them",
        "Удалённые файлы с несохранёнными изменениями остаются открытыми — сохраните, чтобы вернуть их",
    ),
    ("Untitled", "Без названия"),
    ("untitled", "без названия"),
    ("Project: {0}", "Проект: {0}"),
    ("Folder not found: {0}", "Папка не найдена: {0}"),
    ("Ln {0}, Col {1}", "Стр {0}, кол {1}"),
    ("Save changes to {0}?", "Сохранить изменения в «{0}»?"),
    (
        "Your changes will be lost if you don’t save them.",
        "Если не сохранить изменения, они будут потеряны.",
    ),
    ("Don’t Save", "Не сохранять"),
    ("Saved", "Сохранено"),
    ("Save failed: {0}", "Не удалось сохранить: {0}"),
    ("Plain Text", "Обычный текст"),
    ("{0} (no highlighting)", "{0} (без подсветки)"),
    // Terminals in the window: closing and quitting with running commands.
    (
        "Terminate running processes?",
        "Завершить запущенные процессы?",
    ),
    // Closing a window or quitting with commands running or Claude at work: one short question.
    ("Quit Flux?", "Выйти из Flux?"),
    (
        "All running processes will be stopped.",
        "При выходе будут прерваны все запущенные процессы.",
    ),
    ("Close the window?", "Закрыть окно?"),
    ("Close the project?", "Закрыть проект?"),
    (
        "All running processes of this window will be stopped.",
        "При закрытии будут прерваны все процессы этого окна.",
    ),
    ("Terminate", "Завершить"),
    // lifecycle
    ("Leave processes running?", "Оставить процессы работать?"),
    (
        "{0} started from Flux and will keep running after it quits. Until they end, the Dock shows Flux as open.",
        "{0} запущены из Flux и продолжат работать после выхода. Пока они не завершатся, Dock показывает Flux открытым.",
    ),
    ("Leave Running", "Оставить"),
];
