//! Stage 8.3: strings several parts show (Settings → Appearance, Quick Switch, the Marketplace).
//! The skeleton's table: agents don't edit it — a string of their own goes into their table.

pub(super) const STRINGS: &[(&str, &str)] = &[
    ("Appearance", "Внешний вид"),
    ("Theme", "Тема"),
    ("File Icons", "Значки файлов"),
    ("Language", "Язык"),
    ("Light", "Светлая"),
    ("Dark", "Тёмная"),
    ("Sync with OS", "Синхронизировать с ОС"),
    ("System", "Как в системе"),
    ("Marketplace", "Marketplace"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
