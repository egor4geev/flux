//! Part 10.3: plugins' form dialogs and the settings page's new kinds — secrets, buttons, status
//! lines, sections (agent B); text and records (agent H2).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // A plugin's secret setting (a token, a password): kept in the macOS keychain.
    ("Not set", "Не задан"),
    ("Saved in the keychain", "Сохранён в Связке ключей"),
    ("Reading the keychain…", "Читаю Связку ключей…"),
    ("The keychain refused: {0}", "Связка ключей отказала: {0}"),
    // A record of a `records` setting (a plugin's own action): removing one with something typed.
    ("Remove “{0}”?", "Удалить «{0}»?"),
    (
        "Its fields go with it; Reset brings back the plugin's own list.",
        "Вместе с его полями; «Сбросить» вернёт список плагина.",
    ),
    // The install question and the Permissions tab: the `claude` permission.
    (
        "Open Claude Code sessions with its own requests",
        "Открывать сессии Claude Code со своими запросами",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
