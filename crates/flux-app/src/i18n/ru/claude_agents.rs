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
    // subagent: a message for the agent, through Claude
    (
        "A message for the subagent «{0}» (agent id {1}), written in its view — pass it on to the agent with SendMessage:",
        "Сообщение для субагента «{0}» (agent id {1}), написано в его окне — передай его агенту через SendMessage:",
    ),
    (
        "A message for the subagent «{0}», written in its view — pass it on to the agent with SendMessage:",
        "Сообщение для субагента «{0}», написано в его окне — передай его агенту через SendMessage:",
    ),
    (
        "Delivered when the agent is done",
        "Дойдёт, когда агент закончит",
    ),
    ("A message for the agent", "Сообщение агенту"),
    (
        "Through Claude to the agent «{0}»",
        "Через Claude агенту «{0}»",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
