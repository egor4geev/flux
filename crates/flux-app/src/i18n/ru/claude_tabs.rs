//! Part 10.2: session tabs' statuses, renaming, macOS notifications.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The status tooltip of a session's pill and editor tab
    ("Waiting for your answer: {0}", "Ждёт вашего ответа: {0}"),
    (
        "Click — to the place in the conversation",
        "Щелчок — к месту в ленте",
    ),
    ("{0} ago", "{0} назад"),
    // macOS notifications
    ("Claude · {0}", "Claude · {0}"),
    ("Stopped with an error", "Остановлен с ошибкой"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[
    (
        "{n} subagent works",
        [
            "{n} субагент работает",
            "{n} субагента работают",
            "{n} субагентов работают",
        ],
    ),
    (
        "{n} background task",
        ["{n} фоновая задача", "{n} фоновые задачи", "{n} фоновых задач"],
    ),
];
