//! Part 10.3: contexts — the tabs of a task, Save / Load / Clear Context (agent D).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The palette's section and commands (`contexts::SaveContext`…), and the dialogs.
    ("Contexts", "Контексты"),
    ("Save Context", "Сохранить контекст"),
    ("Load Context", "Загрузить контекст"),
    ("Clear Context", "Очистить контекст"),
    (
        "Contexts are kept by project",
        "Контексты хранятся по проектам",
    ),
    ("Context name", "Название контекста"),
    (
        "The open tabs come back with Load Context",
        "Открытые вкладки вернутся по «Загрузить контекст»",
    ),
    ("Context “{0}” saved", "Контекст «{0}» сохранён"),
    // Load Context: the popup.
    ("Search contexts…", "Поиск контекстов…"),
    ("Delete Context", "Удалить контекст"),
    (
        "No saved contexts in this project",
        "В этом проекте нет сохранённых контекстов",
    ),
    ("load", "загрузить"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] =
    &[("{n} tab", ["{n} вкладка", "{n} вкладки", "{n} вкладок"])];
