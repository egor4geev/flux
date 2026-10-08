//! Pickers: command palette, file finder, go to line.

pub(super) const STRINGS: &[(&str, &str)] = &[
    ("Run a command…", "Выполнить команду…"),
    ("No matching commands", "Нет подходящих команд"),
    ("No matches", "Нет совпадений"),
    ("Search files by name…", "Поиск файлов по имени…"),
    ("Indexing…", "Индексация…"),
    ("indexing…", "индексация…"),
    ("No matching files", "Нет подходящих файлов"),
    ("{0} of {1}+ files", "{0} из {1}+ файлов"),
    (
        "Line number or line:column",
        "Номер строки или строка:столбец",
    ),
    (
        "Type a line number, or line:column",
        "Введите номер строки или строка:столбец",
    ),
    (
        "Line must be between 1 and {0}",
        "Номер строки — от 1 до {0}",
    ),
    ("Current line {0} of {1}", "Текущая строка {0} из {1}"),
];
