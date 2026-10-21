//! Part «Пульт Claude»: the console view (fork B).

pub(super) const STRINGS: &[(&str, &str)] = &[
    ("Show archived", "Показать архивные"),
    // When a limit resets
    ("until {0}", "до {0}"),
    // When an idle session last worked
    ("{0} min ago", "{0} мин назад"),
    ("{0} h ago", "{0} ч назад"),
    // The groups
    ("Now", "Сейчас"),
    ("This Week", "На этой неделе"),
    ("No sessions", "Нет сессий"),
    ("Filter sessions", "Фильтр сессий"),
    // A session's row: what it does now
    ("Asks: {0}", "Спрашивает: {0}"),
    ("Asks permission: {0}", "Просит разрешения: {0}"),
    ("Waits for the plan's approval", "Ждёт одобрения плана"),
    ("Running {0}", "Запускает {0}"),
    ("Reading {0}", "Читает {0}"),
    ("Editing {0}", "Правит {0}"),
    ("Searching", "Ищет"),
    ("Searching the web", "Ищет в сети"),
    ("Agent: {0}", "Агент: {0}"),
    ("Thinking", "Думает"),
    ("Writing", "Пишет"),
    // Quick actions and the menu
    ("Answer in the Chat…", "Ответить в чате…"),
    ("Open the Plan", "Открыть план"),
    ("Archive", "В архив"),
    ("Restore from the Archive", "Вернуть из архива"),
    // Times of the history
    ("Yesterday", "Вчера"),
    ("Mon", "пн"),
    ("Tue", "вт"),
    ("Wed", "ср"),
    ("Thu", "чт"),
    ("Fri", "пт"),
    ("Sat", "сб"),
    ("Sun", "вс"),
    // The limits
    ("5 h", "5 ч"),
    ("7 days", "7 дней"),
    ("{0}% · {1}", "{0}% · {1}"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
