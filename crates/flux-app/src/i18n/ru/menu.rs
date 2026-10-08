//! The menu bar: the application menu and the commands behind it.

pub(super) const STRINGS: &[(&str, &str)] = &[
    ("About Flux", "О программе Flux"),
    ("Settings…", "Настройки…"),
    ("Services", "Службы"),
    ("Hide Flux", "Скрыть Flux"),
    ("Hide Others", "Скрыть остальные"),
    ("Show All", "Показать все"),
    ("Quit Flux", "Завершить Flux"),
    // Command palette titles of the menu actions (`flux::Hide`…).
    ("Hide", "Скрыть"),
];
