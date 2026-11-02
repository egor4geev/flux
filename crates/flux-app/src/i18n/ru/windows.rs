//! Part 10.1: Settings → System Settings (projects), the Window menu, the launchpad's Claude icon
//! and the shortcut labels (`settings_view.rs`, `app_menu.rs`, `launchpad.rs`, `ui.rs`).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The Window menu, and its actions' titles in the command palette (`window::Minimize`…).
    ("Window", "Окно"),
    ("Minimize", "Свернуть"),
    ("Zoom", "Изменить масштаб"),
    // Zoom on Windows (part 10.9).
    ("Maximize", "Развернуть"),
    ("Next Window", "Следующее окно"),
    ("Previous Window", "Предыдущее окно"),
    (
        "Bring All to Front",
        "Переместить все окна на передний план",
    ),
    (
        "Bring All To Front",
        "Переместить все окна на передний план",
    ),
    // Settings → System Settings.
    // «Системные настройки» doesn't fit the sidebar.
    ("System Settings", "Система"),
    (
        "How Flux opens projects: when it starts, and when the window already has one.",
        "Как Flux открывает проекты: при запуске и когда в окне уже открыт проект.",
    ),
    (
        "Reopen projects on startup",
        "Открывать проекты при запуске",
    ),
    (
        "Flux started from the Dock or the Finder opens the projects that were open when it quit.",
        "Flux, запущенный из Dock или Finder, открывает проекты, которые были открыты при выходе.",
    ),
    (
        "Flux started from the Start menu or the taskbar opens the projects that were open when it quit.",
        "Flux, запущенный из меню «Пуск» или с панели задач, открывает проекты, которые были открыты при выходе.",
    ),
    ("Open project in", "Где открывать проект"),
    ("New window", "В новом окне"),
    (
        "The project gets a window of its own; the open one stays as it is",
        "У проекта своё окно, открытое остаётся как есть",
    ),
    ("This window", "В этом окне"),
    (
        "The open project closes, keeping its tabs for next time, and the new one takes its place",
        "Открытый проект закрывается, сохранив вкладки до следующего раза, а новый занимает его место",
    ),
    ("Ask", "Спрашивать"),
    (
        "Flux asks every time; “Don’t ask again” remembers the answer here",
        "Flux спрашивает каждый раз; «Больше не спрашивать» запоминает ответ здесь",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
