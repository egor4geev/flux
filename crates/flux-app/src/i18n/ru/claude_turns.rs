//! Part 10.2: actions on messages and turns: copy, edit and resend, rewind, the "Done" card.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // Editing a sent message
    ("Editing a message", "Изменение сообщения"),
    (
        "This message can't be edited",
        "Это сообщение нельзя изменить",
    ),
    (
        "The message isn't in the conversation",
        "Сообщения нет в разговоре",
    ),
    (
        "The session's record doesn't have this message",
        "В записи сессии нет этого сообщения",
    ),
    // The `!` field
    ("A command, e.g. git status", "Команда, например git status"),
    // Rewinding and rolling back
    (
        "Rewind the code to before this message?",
        "Откатить код до этого сообщения?",
    ),
    (
        "Roll back the changes of this turn?",
        "Откатить изменения этого хода?",
    ),
    (
        "Roll back Claude's changes to {0}?",
        "Откатить изменения Claude в {0}?",
    ),
    ("Roll back this edit of {0}?", "Откатить эту правку {0}?"),
    (
        "Can't roll back this edit of {0}",
        "Не удаётся откатить правку {0}",
    ),
    (
        "The file changed after the edit, and the edited text isn't in it as it was. Compare the edit with the file and undo it by hand.",
        "Файл изменился после правки, и исправленного текста в нём в прежнем виде нет. Сравните правку с файлом и отмените её вручную.",
    ),
    (
        "The files go back as they were before Claude's edits. What Claude's commands changed stays.",
        "Файлы вернутся к виду до правок Claude. То, что изменили его команды, останется.",
    ),
    (
        "{0} changed after Claude's edit: those changes go too",
        "{0} менялся после правки Claude: эти изменения тоже пропадут",
    ),
    ("to the Trash", "в Корзину"),
    (
        "The files are as they were before Claude's edits",
        "Файлы уже в том виде, что до правок Claude",
    ),
    // The diffs
    ("Before this turn", "До этого хода"),
    ("Before the edit", "До правки"),
    ("After the edit", "После правки"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
