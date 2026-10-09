//! The Git window: the log, the graph, filters, file history (part B of stage 6.3).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The window and its tabs.
    ("Git", "Git"),
    ("Log", "Лог"),
    ("History: {0}", "История: {0}"),
    ("Hide", "Скрыть"),
    ("Move to Editor", "Перенести в редактор"),
    ("Close", "Закрыть"),
    (
        "No Git repository in the project",
        "В проекте нет Git-репозитория",
    ),
    (
        "The file isn’t in a Git repository",
        "Файл не в репозитории Git",
    ),
    // The filter bar.
    ("Text or hash", "Текст или хеш"),
    ("Branches", "Ветки"),
    ("Branch", "Ветка"),
    ("Branch: {0}", "Ветка: {0}"),
    ("User", "Автор"),
    ("User: {0}", "Автор: {0}"),
    ("me ({0})", "я ({0})"),
    ("Date", "Дата"),
    ("Since {0}", "С {0}"),
    ("Paths", "Пути"),
    ("Paths: {0}", "Пути: {0}"),
    ("All", "Все"),
    ("Last 24 hours", "За 24 часа"),
    ("Last 7 days", "За 7 дней"),
    ("Last 30 days", "За 30 дней"),
    ("Select…", "Выбрать…"),
    ("Name or e-mail", "Имя или e-mail"),
    ("Since a date: YYYY-MM-DD", "С даты: ГГГГ-ММ-ДД"),
    (
        "Path in the repository: src/, README.md",
        "Путь в репозитории: src/, README.md",
    ),
    ("Reset Filters", "Сбросить фильтры"),
    ("Refresh", "Обновить"),
    // The branches pane.
    ("Local", "Локальные"),
    ("Remote", "Удалённые"),
    ("Tags", "Теги"),
    // The list.
    ("Reading…", "Чтение…"),
    ("Not a Git repository", "Не Git-репозиторий"),
    (
        "No commits match the filters",
        "Нет коммитов под эти фильтры",
    ),
    ("No commits yet", "Коммитов пока нет"),
    ("Today {0}", "Сегодня {0}"),
    ("Yesterday {0}", "Вчера {0}"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
