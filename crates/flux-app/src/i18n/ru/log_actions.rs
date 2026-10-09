//! The commit pane and the operations on commits (part C of stage 6.3).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The commit pane.
    ("Select a commit", "Выберите коммит"),
    ("Copy Revision Number", "Копировать хеш ревизии"),
    ("Empty", "Пусто"),
    ("Changed files", "Изменённые файлы"),
    ("In branches: reading…", "В ветках: чтение…"),
    ("In no branch", "Ни в одной ветке"),
    ("No changed files", "Нет изменённых файлов"),
    ("committed by {0} on {1}", "закоммитил {0} {1}"),
    // The commit menu.
    ("Cherry-Pick", "Cherry-pick"),
    ("Compare with Local", "Сравнить с локальной версией"),
    (
        "Reset Current Branch to Here…",
        "Сбросить текущую ветку сюда…",
    ),
    ("Revert Commit", "Отменить коммит (revert)"),
    ("Revert Commits", "Отменить коммиты (revert)"),
    ("Undo Commit…", "Откатить коммит…"),
    ("Edit Commit Message…", "Изменить сообщение коммита…"),
    ("Fixup Commits", "Слить коммиты (fixup)"),
    ("Squash Commits…", "Объединить коммиты…"),
    ("Drop Commit", "Удалить коммит"),
    ("Drop Commits", "Удалить коммиты"),
    (
        "Interactively Rebase from Here…",
        "Интерактивный rebase отсюда…",
    ),
    ("New Tag…", "Новый тег…"),
    ("Go to Parent Commit", "К родительскому коммиту"),
    // Cherry-pick and revert.
    ("Reverting…", "Revert…"),
    ("Cherry-picking…", "Cherry-pick…"),
    ("Skipping…", "Пропуск…"),
    ("Resetting…", "Сброс…"),
    ("Creating the tag…", "Создание тега…"),
    ("Revert failed", "Revert не удался"),
    ("Cherry-pick failed", "Cherry-pick не удался"),
    (
        "Nothing to revert: the changes are not on the branch",
        "Нечего отменять: изменений нет в ветке",
    ),
    (
        "Nothing to cherry-pick: the changes are already on the branch",
        "Нечего переносить: изменения уже есть в ветке",
    ),
    (
        "Skip this commit, or abort the operation.",
        "Пропустите этот коммит или прервите операцию.",
    ),
    (
        "Revert stopped on conflicts",
        "Revert остановился на конфликтах",
    ),
    (
        "Cherry-pick stopped on conflicts",
        "Cherry-pick остановился на конфликтах",
    ),
    (
        "Resolve the conflicts, then continue.",
        "Разрешите конфликты, затем продолжите.",
    ),
    (
        "Your local changes are in the stash: unstash them once the operation is done.",
        "Ваши локальные изменения в stash: верните их, когда операция завершится.",
    ),
    ("Commit skipped", "Коммит пропущен"),
    // Tags.
    ("New Tag", "Новый тег"),
    ("Tag name", "Имя тега"),
    ("Overwrite existing tag", "Перезаписать существующий тег"),
    ("Enter a tag name", "Введите имя тега"),
    ("Tag not created", "Тег не создан"),
    ("at {0}", "на {0}"),
    ("Tag '{0}' already exists", "Тег «{0}» уже существует"),
    ("Created tag '{0}' at {1}", "Создан тег «{0}» на {1}"),
    // Undo Commit.
    ("Undo Commit failed", "Не удалось откатить коммит"),
    ("The commit is already pushed", "Коммит уже отправлен"),
    (
        "Undoing it rewrites history others may have: the branch will need a force push.",
        "Откат переписывает историю, которая может быть у других: ветку придётся отправить с force push.",
    ),
    ("Undo Commit", "Откатить коммит"),
    ("Commit undone", "Коммит откачен"),
    (
        "Its changes are back in the commit window: {0}",
        "Его изменения снова в окне коммита: {0}",
    ),
    // Reset.
    ("Soft", "Soft"),
    ("Mixed", "Mixed"),
    ("Hard", "Hard"),
    ("Keep", "Keep"),
    (
        "Files aren't touched; the changes of the dropped commits stay staged.",
        "Файлы не меняются; изменения сброшенных коммитов остаются в индексе.",
    ),
    (
        "Files aren't touched; the changes of the dropped commits stay, unstaged.",
        "Файлы не меняются; изменения сброшенных коммитов остаются вне индекса.",
    ),
    (
        "Files become as in the commit: local changes and the dropped commits' changes are lost.",
        "Файлы станут как в коммите: локальные изменения и изменения сброшенных коммитов пропадут.",
    ),
    (
        "Files change as in the commit; local changes are kept, or the reset refuses if they are in the way.",
        "Файлы станут как в коммите; локальные изменения сохраняются, а если мешают — сброс не выполняется.",
    ),
    ("Reset failed", "Сброс не удался"),
    ("Git Reset", "Git Reset"),
    ("Reset", "Сбросить"),
    ("Reset '{0}' to {1}", "Ветка «{0}» сброшена на {1}"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[
    (
        "Reverted {n} commit",
        [
            "Отменён {n} коммит",
            "Отменено {n} коммита",
            "Отменено {n} коммитов",
        ],
    ),
    (
        "Cherry-picked {n} commit",
        [
            "Перенесён {n} коммит",
            "Перенесено {n} коммита",
            "Перенесено {n} коммитов",
        ],
    ),
    (
        "In {n} branch",
        ["В {n} ветке", "В {n} ветках", "В {n} ветках"],
    ),
    ("Parent", ["Родитель", "Родители", "Родители"]),
    ("{n} commit", ["{n} коммит", "{n} коммита", "{n} коммитов"]),
];
