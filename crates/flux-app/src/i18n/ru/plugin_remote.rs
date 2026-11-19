//! Bot part 1: API 0.6 for plugins — `claude-remote`, tasks, the install question's warnings (agent B).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The install question (plugin_manager.rs).
    (
        "Read and write in every Claude session and answer Claude's requests for you",
        "Читать и писать во все сессии Claude и отвечать на его запросы за вас",
    ),
    (
        "Read your tasks from other plugins",
        "Читать ваши задачи из других плагинов",
    ),
    (
        "Give your tasks to other plugins that ask",
        "Отдавать ваши задачи другим плагинам по запросу",
    ),
    (
        "Add to the sessions in the Claude console: {0}",
        "Добавлять к сессиям в пульте Claude: {0}",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
