//! Part 9.2: Claude Code — the session history and sessions after a restart (`claude_history.rs`,
//! the History button and the recent sessions of `claude_panel.rs`, `/resume` of
//! `claude_composer.rs`).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The history popup.
    ("Search sessions…", "Поиск сессий…"),
    ("Reading the sessions…", "Чтение сессий…"),
    ("No saved sessions in this project", "В этом проекте нет сохранённых сессий"),
    ("already open", "уже открыта"),
    ("resume", "продолжить"),
    ("{0} min ago", "{0} мин назад"),
    // The Claude window.
    ("Session History", "История сессий"),
    ("Resume Session…", "Продолжить сессию…"),
    ("Recent Sessions", "Недавние сессии"),
    ("Show All…", "Показать все…"),
    // The message field's `/` list.
    ("Resume a session of this project", "Продолжить сессию этого проекта"),
    // The command palette (action names).
    ("Resume Session", "Продолжить сессию"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[(
    "{n} session",
    ["{n} сессия", "{n} сессии", "{n} сессий"],
)];
