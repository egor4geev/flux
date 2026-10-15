//! The menu bar: the application menu and the commands behind it.

pub(super) const STRINGS: &[(&str, &str)] = &[
    ("About Flux", "О программе Flux"),
    ("Settings…", "Настройки…"),
    ("Services", "Службы"),
    ("Hide Flux", "Скрыть Flux"),
    ("Hide Others", "Скрыть остальные"),
    ("Show All", "Показать все"),
    ("Quit Flux", "Завершить Flux"),
    // The Help menu (part 10.6); "Help" is also the chip of its commands in the palette.
    ("Help", "Справка"),
    ("Report a Bug…", "Сообщить об ошибке…"),
    ("Suggest a Feature…", "Предложить идею…"),
    ("Ask a Question…", "Задать вопрос…"),
    ("Show Log in Finder", "Показать журнал в Finder"),
    // Command palette titles of the menu actions (`flux::Hide`…).
    ("Hide", "Скрыть"),
];
