//! Interactive rebase and rewriting history (part D of stage 6.3).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The interactive rebase dialog.
    ("{0} onto {1}", "{0} на {1}"),
    ("the root", "корень"),
    ("Pick", "Взять"),
    ("Edit", "Остановиться"),
    ("Reword", "Изменить сообщение"),
    ("Squash", "Объединить"),
    ("Fixup", "Fixup"),
    ("pick", "взять"),
    ("edit", "правка"),
    ("reword", "сообщение"),
    ("squash", "squash"),
    ("fixup", "fixup"),
    ("drop", "удалить"),
    ("New message", "Новое сообщение"),
    (
        "Message of the squashed commit",
        "Сообщение объединённого коммита",
    ),
    ("Message", "Сообщение"),
    (
        "Select one commit to see its message",
        "Выберите один коммит, чтобы увидеть сообщение",
    ),
    ("move", "переместить"),
    ("message", "сообщение"),
    ("Start Rebasing", "Начать rebase"),
    ("Every commit is dropped", "Удалены все коммиты"),
    (
        "The oldest commit has nothing to meld into",
        "Самому старому коммиту не с чем объединиться",
    ),
    (
        "A reworded commit needs a message",
        "У коммита с новым сообщением оно пустое",
    ),
    // The flows.
    (
        "Finish the operation in progress first",
        "Сначала завершите текущую операцию",
    ),
    (
        "These commits are already pushed",
        "Эти коммиты уже отправлены",
    ),
    (
        "Rewriting them changes the history others may have. The branch will need a force push.",
        "Переписав их, вы измените историю, которая может быть у других. Ветку придётся отправить с force push.",
    ),
    ("Rewrite Anyway", "Всё равно переписать"),
    (
        "Couldn't edit the commit message",
        "Не удалось изменить сообщение коммита",
    ),
    (
        "Rebase stopped on conflicts",
        "Rebase остановлен на конфликтах",
    ),
    (
        "Local changes came back with conflicts",
        "Локальные изменения вернулись с конфликтами",
    ),
    (
        "Resolve the conflicts of the changes stashed around the rebase.",
        "Разрешите конфликты изменений, отложенных на время rebase.",
    ),
    (
        "Stopped at {0} for editing",
        "Остановлено на {0} для правки",
    ),
    (
        "Amend the commit as you need, then continue the rebase.",
        "Исправьте коммит (amend) и продолжите rebase.",
    ),
    ("Commit message updated", "Сообщение коммита изменено"),
    (
        "Can't rebase from this commit",
        "Нельзя сделать rebase от этого коммита",
    ),
    ("Couldn't read the commit", "Не удалось прочитать коммит"),
    ("Edit Commit Message", "Изменить сообщение коммита"),
    ("Can't meld these commits", "Нельзя объединить эти коммиты"),
    (
        "These commits aren't on the current branch",
        "Этих коммитов нет в текущей ветке",
    ),
    ("Into {0}", "В {0}"),
    ("Can't drop these commits", "Нельзя удалить эти коммиты"),
    ("confirm", "подтвердить"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[
    (
        "Rebasing {n} commit",
        [
            "Rebase {n} коммита",
            "Rebase {n} коммитов",
            "Rebase {n} коммитов",
        ],
    ),
    (
        "Rebased {n} commit",
        [
            "Rebase выполнен: {n} коммит",
            "Rebase выполнен: {n} коммита",
            "Rebase выполнен: {n} коммитов",
        ],
    ),
    (
        "Squashed {n} commit",
        [
            "Объединён {n} коммит",
            "Объединено {n} коммита",
            "Объединено {n} коммитов",
        ],
    ),
    (
        "Fixed up {n} commit",
        [
            "Объединён без сообщения {n} коммит",
            "Объединено без сообщения {n} коммита",
            "Объединено без сообщения {n} коммитов",
        ],
    ),
    (
        "Dropped {n} commit",
        [
            "Удалён {n} коммит",
            "Удалено {n} коммита",
            "Удалено {n} коммитов",
        ],
    ),
    (
        "Drop {n} commit?",
        [
            "Удалить {n} коммит?",
            "Удалить {n} коммита?",
            "Удалить {n} коммитов?",
        ],
    ),
    (
        "Squash {n} Commit",
        [
            "Объединить {n} коммит",
            "Объединить {n} коммита",
            "Объединить {n} коммитов",
        ],
    ),
];
