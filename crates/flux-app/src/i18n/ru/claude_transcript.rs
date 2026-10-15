//! Stage 9: Claude Code — the conversation: messages, Claude's actions, notices
//! (`claude_transcript.rs`, `claude_chat.rs`).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // An empty session
    (
        "Ask Claude about this project",
        "Спросите Claude об этом проекте",
    ),
    ("Add the editor's selection", "Добавить выделение редактора"),
    ("Mention a file", "Упомянуть файл"),
    ("Commands and skills", "Команды и навыки"),
    ("Stop Claude", "Остановить Claude"),
    ("Starting Claude…", "Запускаем Claude…"),
    // The user's messages
    ("Queued", "В очереди"),
    ("Cancelled", "Отменено"),
    ("Don't Send", "Не отправлять"),
    ("Jump to Latest", "К последнему"),
    // Thinking and the line of what Claude does
    ("Thinking…", "Думает…"),
    ("Thinking… {0} tokens", "Думает… {0} токенов"),
    ("Thought for {0}", "Думал {0}"),
    ("Thought", "Думал"),
    ("Writing…", "Пишет…"),
    ("Running {0}…", "Выполняет {0}…"),
    ("Compacting the conversation…", "Сжимает разговор…"),
    ("Retrying ({0} of {1})…", "Повтор ({0} из {1})…"),
    ("Waiting for your answer", "Ждёт вашего ответа"),
    ("to stop", "остановить"),
    ("{0} tokens", "{0} токенов"),
    // Claude's actions
    ("Agent", "Агент"),
    ("Task", "Задача"),
    ("Tasks", "Задачи"),
    ("Question", "Вопрос"),
    ("Plan", "План"),
    ("Plan mode", "Режим плана"),
    ("in progress", "в работе"),
    ("done", "готова"),
    ("deleted", "удалена"),
    ("updated", "обновлена"),
    ("in the background", "в фоне"),
    ("lines {0}–{1}", "строки {0}–{1}"),
    ("waiting for your answer", "ждёт вашего ответа"),
    ("failed", "ошибка"),
    ("refused", "отклонено"),
    ("stopped", "остановлено"),
    ("The command was interrupted", "Команда прервана"),
    // Notices
    ("Interrupted", "Прервано"),
    ("Conversation compacted", "Разговор сжат"),
    (
        "Conversation compacted · {0} → {1} tokens",
        "Разговор сжат · {0} → {1} токенов",
    ),
    ("{0} was refused", "{0}: отклонено"),
    (
        "Claude needs you to sign in again",
        "Claude просит войти заново",
    ),
    ("Usage limit reached", "Лимит использования исчерпан"),
    (
        "A billing problem stopped the request",
        "Запрос остановлен из-за проблемы с оплатой",
    ),
    (
        "Claude is overloaded — try again in a moment",
        "Claude перегружен — попробуйте чуть позже",
    ),
    (
        "The answer hit the output limit",
        "Ответ упёрся в предел длины",
    ),
    ("The request failed", "Запрос не удался"),
    (
        "Claude stopped (exit code {0})",
        "Claude остановился (код выхода {0})",
    ),
    ("Claude couldn't start", "Claude не запустился"),
    (
        "Send a message to start it again.",
        "Отправьте сообщение, чтобы запустить его снова.",
    ),
    ("Context", "Контекст"),
    ("{0} of {1} tokens", "{0} из {1} токенов"),
    // The ends of turns
    ("ended with an error", "закончился ошибкой"),
    ("the turn limit was reached", "достигнут предел ходов"),
    (
        "the conversation is too long, /compact it",
        "разговор слишком длинный — сожмите его: /compact",
    ),
    ("the budget is spent", "бюджет исчерпан"),
    (
        "≈ ${0} at API prices (with a subscription, it counts towards its limits)",
        "≈ ${0} по ценам API (с подпиской идёт в её лимиты)",
    ),
    // Part 10.2: the chat's look — the user's messages, `!` commands, cards, the "Done" card
    ("Preview", "Предпросмотр"),
    ("Edit and Resend", "Изменить и отправить заново"),
    (
        "Rewind Code to Before This Message",
        "Откатить код до этого сообщения",
    ),
    ("exit {0}", "код {0}"),
    ("Claude got the output", "Вывод передан Claude"),
    ("Claude will get the output", "Вывод получит Claude"),
    ("Watch", "Смотреть"),
    ("Done in {0}", "Готово за {0}"),
    ("Review", "Обзор"),
    ("Changed Files", "Изменённые файлы"),
    ("New Files", "Новые файлы"),
    ("updated the tasks", "обновил задачи"),
    ("code", "код"),
    // Durations
    ("{0} ms", "{0} мс"),
    ("{0} s", "{0} с"),
    ("{0} min {1} s", "{0} мин {1} с"),
    ("{0} h {1} min", "{0} ч {1} мин"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[
    // Part 10.2: a group of Claude's steps, in short
    (
        "read {n} file",
        [
            "прочитал {n} файл",
            "прочитал {n} файла",
            "прочитал {n} файлов",
        ],
    ),
    (
        "searched {n} time",
        ["искал {n} раз", "искал {n} раза", "искал {n} раз"],
    ),
    (
        "searched the web {n} time",
        [
            "искал в сети {n} раз",
            "искал в сети {n} раза",
            "искал в сети {n} раз",
        ],
    ),
    (
        "fetched {n} page",
        [
            "открыл {n} страницу",
            "открыл {n} страницы",
            "открыл {n} страниц",
        ],
    ),
    (
        "used {n} tool",
        [
            "вызвал {n} инструмент",
            "вызвал {n} инструмента",
            "вызвал {n} инструментов",
        ],
    ),
    ("{n} error", ["{n} ошибка", "{n} ошибки", "{n} ошибок"]),
    (
        "Show {n} more line",
        ["Ещё {n} строка", "Ещё {n} строки", "Ещё {n} строк"],
    ),
    (
        "{n} tool use",
        [
            "{n} вызов инструмента",
            "{n} вызова инструментов",
            "{n} вызовов инструментов",
        ],
    ),
    ("{n} task", ["{n} задача", "{n} задачи", "{n} задач"]),
    (
        "new file · {n} line",
        [
            "новый файл · {n} строка",
            "новый файл · {n} строки",
            "новый файл · {n} строк",
        ],
    ),
];
