//! Feedback (part 10.6): Help → Report a Bug…, Suggest a Feature…, Ask a Question…, About → Copy,
//! a plugin's Report an Issue.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // A plugin's Overview (Settings → Plugins, the Marketplace).
    ("Feedback", "Обратная связь"),
    ("Report an Issue", "Сообщить о проблеме"),
    // About.
    ("Computer", "Компьютер"),
    ("{0} GB", "{0} ГБ"),
    ("Copied", "Скопировано"),
    (
        "The version, macOS, the computer and the plugins — for a report",
        "Версия, macOS, компьютер и плагины — для сообщения о проблеме",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
