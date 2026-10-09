//! The window: title bar, status bar, tabs, launchpad, window dialogs, editor messages.

pub(super) const STRINGS: &[(&str, &str)] = &[
    (
        "No project — open a folder with ⌘O",
        "Нет проекта — откройте папку через ⌘O",
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
    (
        "“{0}” is still running in a terminal.",
        "«{0}» ещё работает в терминале.",
    ),
    (
        "{0} are still running in terminals.",
        "{0} ещё работают в терминалах.",
    ),
    ("Terminate", "Завершить"),
];
