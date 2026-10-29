//! Part 10.2: `!` commands and the message field.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // Shell mode of the message field
    ("Shell command", "Команда оболочки"),
    ("Claude answers the output", "Claude ответит на вывод"),
    (
        "Claude only reads the output",
        "Claude только прочтёт вывод",
    ),
    ("{0} without an answer", "{0} — без ответа"),
    ("{0} with an answer", "{0} — с ответом"),
    (
        "A shell command — runs in the project's root",
        "Команда оболочки — выполнится в корне проекта",
    ),
    ("Run", "Выполнить"),
    ("Stop the command", "Остановить команду"),
    // The attach button
    ("Attach pictures or files", "Приложить картинки или файлы"),
    ("Attach", "Приложить"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
