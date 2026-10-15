//! The welcome guide's Claude Code slide and "All set" (part 10.7).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // Claude Code.
    ("Setup · Claude Code", "Настройка · Claude Code"),
    ("Claude in Its Own Chat", "Claude — в своём чате"),
    (
        "Flux works with your claude and your Claude subscription: the chat on the right or as an editor tab, edits as diffs with review and rollback, a fix for a problem with {0}.",
        "Flux работает с вашим claude и подпиской Claude: чат справа или вкладкой редактора, правки — диффом с обзором и откатом, исправление проблемы — по {0}.",
    ),
    (
        "Open the chat and go back to the editor",
        "Открыть чат и вернуться в редактор",
    ),
    ("The selection into the message", "Выделенное — в сообщение"),
    ("“{0}” on a problem", "«{0}» у проблемы"),
    ("Claude Code is ready", "Claude Code готов"),
    (
        "The chat opens with {0} — on the right or as an editor tab",
        "Чат открывается по {0} — справа или вкладкой редактора",
    ),
    (
        "With your Claude subscription: the sign-in opens the browser",
        "По подписке Claude: вход откроется в браузере",
    ),
    (
        "Install it in a terminal — Flux finds it by itself",
        "Установите его в терминале — Flux найдёт его сам",
    ),
    ("Waiting for the sign-in…", "Ждём вход в браузере…"),
    // All set.
    ("All Set", "Всё готово"),
    ("Start Screen", "Начальный экран"),
    (
        "Open a project folder — Flux finds its git, languages and Claude sessions by itself. The guide is always at hand: Help → Welcome Guide, or {0}.",
        "Откройте папку проекта — Flux сам найдёт в ней git, языки и сессии Claude. Гайд всегда под рукой: Справка → Гайд по Flux или {0}.",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
