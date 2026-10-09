//! Stage 9: Claude Code — the message field: mentions, commands, images, the model, effort and
//! mode pickers (`claude_composer.rs`, `prompt_input.rs`).

pub(super) const STRINGS: &[(&str, &str)] = &[
    ("Ask Claude… @ for files, / for commands", "Спросите Claude… @ файлы, / команды"),
    ("Claude is working — your message will wait", "Claude работает — сообщение подождёт"),
    ("Answer Claude above, or write here", "Ответьте выше или напишите здесь"),
    ("Claude stopped — a message resumes the session", "Claude остановлен — сообщение продолжит сессию"),
    ("Ask before edits", "Спрашивать"),
    ("Accept edits", "Принимать правки"),
    ("Plan mode", "Режим плана"),
    ("Don't ask", "Не спрашивать"),
    ("Auto mode", "Авторежим"),
    ("Bypass permissions", "Без разрешений"),
    ("Low effort", "Низкое усилие"),
    ("Medium effort", "Среднее усилие"),
    ("High effort", "Высокое усилие"),
    ("Extra high effort", "Очень высокое усилие"),
    ("Max effort", "Максимальное усилие"),
    ("Only PNG, JPEG, GIF and WebP pictures can be attached", "Прикрепить можно только картинки PNG, JPEG, GIF и WebP"),
    ("The picture is larger than 10 MB", "Картинка больше 10 МБ"),
    ("Model", "Модель"),
    ("Effort", "Усилие"),
    ("Permission Mode", "Режим разрешений"),
    ("Permission mode (⇧⇥)", "Режим разрешений (⇧⇥)"),
    ("{0} goes with the message", "{0} уйдёт вместе с сообщением"),
    ("Remove", "Удалить"),
    ("{0} / {1} tokens in the context", "{0} / {1} токенов в контексте"),
    ("Stop", "Остановить"),
    ("Send after the turn", "Отправить после ответа"),
    ("Send", "Отправить"),
    ("Low", "Низкое"),
    ("Medium", "Среднее"),
    ("High", "Высокое"),
    ("Extra high", "Очень высокое"),
    ("Max", "Максимальное"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
