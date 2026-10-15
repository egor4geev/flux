//! Part 10.5: the user's settings of language servers in `settings.json` (`lsp_settings.rs`),
//! `settings.json` as a file — read again when it changes, opened in a tab (`settings_file.rs`).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // Settings → Language Servers: a row of a server.
    ("Configure…", "Настроить…"),
    ("Turn off", "Выключить"),
    ("Turn on", "Включить"),
    ("Off", "Выключен"),
    // The badge of a server the user set up in `settings.json`.
    ("Custom", "Свои настройки"),
    (
        "Turned off: Flux doesn't start it",
        "Выключен: Flux его не запускает",
    ),
    // The user's own command (`lsp.<server>.command`) isn't there.
    ("Not found: {0}", "Не найден: {0}"),
    ("{0} not found", "{0} не найден"),
    // Settings → Language Servers: the card about `settings.json`.
    ("Server settings", "Настройки серверов"),
    (
        "Each server's settings, command and environment are in settings.json, under “lsp”: Configure… opens a server's key.",
        "Настройки каждого сервера, его команда и окружение — в settings.json, в разделе «lsp»: «Настроить…» открывает раздел сервера.",
    ),
    ("Open settings.json", "Открыть settings.json"),
    // The command palette (`settings::OpenSettingsFile`).
    ("Open Settings File", "Открыть файл настроек"),
    // What the user's settings did to a running server.
    (
        "{0}: new settings applied",
        "{0}: новые настройки применены",
    ),
    (
        "{0} restarted with new settings",
        "{0} перезапущен с новыми настройками",
    ),
    ("{0} is turned off", "{0} выключен"),
    // A broken `settings.json`.
    (
        "Couldn't read settings.json",
        "Не удалось прочитать settings.json",
    ),
    (
        "{0} — line {1}, column {2}.",
        "{0} — строка {1}, столбец {2}.",
    ),
    (
        "Flux keeps the settings it has and won't write the file until it's fixed.",
        "Flux оставит прежние настройки и не будет записывать файл, пока его не исправят.",
    ),
    (
        "The settings must be a JSON object",
        "Настройки должны быть объектом JSON",
    ),
    (
        "Settings aren't saved to a file in this session",
        "В этом сеансе настройки не сохраняются в файл",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
