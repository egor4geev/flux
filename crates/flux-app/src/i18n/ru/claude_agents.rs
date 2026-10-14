//! Part 10.2: a subagent's live conversation, the search in the chat.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The search (⌘F)
    ("Find in Chat", "Найти в чате"),
    // A subagent's view
    ("Back to the Session", "Назад к сессии"),
    ("Agent · {0}", "Агент · {0}"),
    ("Task from Claude", "Задание от Claude"),
    ("The agent works in the background", "Агент работает в фоне"),
    (
        "The agent works — Claude waits for its answer",
        "Агент работает — Claude ждёт его итог",
    ),
    ("The agent failed", "Агент завершился с ошибкой"),
    ("The agent was stopped", "Агент остановлен"),
    (
        "The agent is done — its answer went to Claude",
        "Агент закончил — итог передан Claude",
    ),
    ("back to the session", "назад к сессии"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
