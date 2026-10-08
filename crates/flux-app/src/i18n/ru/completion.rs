//! Code completion and hover: messages, command palette sections and titles.

pub(super) const STRINGS: &[(&str, &str)] = &[
    ("No documentation", "Нет документации"),
    // Command palette.
    ("Completion", "Подсказки"),
    ("Hover", "Документация"),
    ("Show Completions", "Показать подсказки"),
    ("Show Hover", "Показать документацию"),
    ("Hide Hover", "Скрыть документацию"),
    ("Confirm Insert", "Вставить подсказку"),
    ("Confirm Replace", "Заменить подсказкой"),
];
