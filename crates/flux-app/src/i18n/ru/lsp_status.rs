//! Part 10.5: the language servers' state and their popup (`lsp_status.rs`), watched files (`lsp_watch.rs`).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // A project a server couldn't load: the status bar, the notification, the popup.
    ("The project isn't loaded", "Проект не загружен"),
    ("Configure…", "Настроить…"),
    ("Show Log", "Показать журнал"),
    (
        "Couldn't save the log of {0}",
        "Не удалось сохранить журнал {0}",
    ),
    // The servers' popup.
    ("Start Server", "Запустить сервер"),
    ("Couldn't install: {0}", "Не удалось установить: {0}"),
    ("Language Server Settings…", "Настройки языковых серверов…"),
    (
        "Language servers start with the files they serve",
        "Языковой сервер запускается, когда открыт файл его языка",
    ),
    ("restart", "перезапустить"),
    // The command palette: "Language Server: Show Servers".
    ("Show Servers", "Показать серверы"),
    ("stop", "остановить"),
    ("log", "журнал"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
