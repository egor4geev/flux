//! The Stash tab of the commit window and the Stash Changes dialog (part C of stage 6.2).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The tab.
    ("Stash", "Stash"),
    ("No stashes", "Нет stash"),
    ("Reading stashes…", "Чтение stash…"),
    (
        "Put local changes aside to come back to them later",
        "Отложите локальные изменения, чтобы вернуться к ним позже",
    ),
    ("No files", "Нет файлов"),
    ("Select a stash", "Выберите stash"),
    ("on {0}", "в {0}"),
    ("Before the stash", "До stash"),
    ("Stash · {0}", "Stash · {0}"),
    ("drop", "удалить"),
    // Its actions.
    ("Stash Changes…", "Stash изменений…"),
    ("Apply", "Применить"),
    ("Pop", "Извлечь"),
    (
        "Apply the stash and keep it in the list",
        "Применить stash и оставить его в списке",
    ),
    (
        "Apply the stash and drop it from the list",
        "Применить stash и убрать его из списка",
    ),
    ("Unstash as Branch…", "Вернуть в новую ветку…"),
    ("Unstash as Branch", "Вернуть stash в новую ветку"),
    ("Drop…", "Удалить…"),
    ("Drop", "Удалить"),
    ("Drop Stash", "Удалить stash"),
    ("Clear…", "Очистить…"),
    ("Drop {0}?", "Удалить {0}?"),
    (
        "The stashed changes will be deleted. This can't be undone.",
        "Изменения из stash будут удалены. Это нельзя отменить.",
    ),
    ("Clear all stashes?", "Удалить все stash?"),
    ("Clear all stashes of {0}?", "Удалить все stash в {0}?"),
    ("The stash is already gone", "Этого stash уже нет"),
    // Results.
    ("Applied {0}", "Применён {0}"),
    ("Popped {0}", "Извлечён {0}"),
    ("Dropped {0}", "Удалён {0}"),
    (
        "Unstash: conflicts; the stash is kept",
        "Возврат из stash: конфликты; stash сохранён",
    ),
    (
        "Unstash: {0}; the stash is kept",
        "Возврат из stash: {0}; stash сохранён",
    ),
    ("Resolve…", "Разрешить…"),
    (
        "Couldn't unstash the changes",
        "Не удалось вернуть изменения из stash",
    ),
    ("Couldn't drop the stash", "Не удалось удалить stash"),
    ("Couldn't clear the stashes", "Не удалось очистить stash"),
    (
        "Couldn't stash the changes",
        "Не удалось спрятать изменения в stash",
    ),
    (
        "No local changes to stash",
        "Нет локальных изменений для stash",
    ),
    // Unstash as Branch.
    ("Branch name", "Имя ветки"),
    ("Create Branch", "Создать ветку"),
    ("from {0}", "из {0}"),
    ("Branch “{0}” already exists", "Ветка «{0}» уже существует"),
    (
        "Checked out a new branch {0}",
        "Переключено на новую ветку {0}",
    ),
    ("with the changes of {0}", "с изменениями {0}"),
    (
        "Couldn't unstash as a branch",
        "Не удалось вернуть stash в новую ветку",
    ),
    // The Stash Changes dialog.
    ("Stash Changes", "Stash изменений"),
    ("Message (optional)", "Сообщение (необязательно)"),
    ("Include untracked files", "Включая неотслеживаемые файлы"),
    ("Keep index", "Сохранить индекс"),
    (
        "Staged changes stay in the working tree too",
        "Проиндексированные изменения останутся и в рабочем дереве",
    ),
    ("Create Stash", "Создать stash"),
    // The palette: the tab's and the dialog's commands.
    ("Stash Panel", "Вкладка Stash"),
    ("Stash Dialog", "Диалог stash"),
    ("Unstash As Branch", "Вернуть в новую ветку"),
    ("Next Repo", "Следующий репозиторий"),
    ("Previous Repo", "Предыдущий репозиторий"),
    ("Clear", "Очистить"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[
    (
        "Stashed {n} file",
        [
            "В stash отправлен {n} файл",
            "В stash отправлено {n} файла",
            "В stash отправлено {n} файлов",
        ],
    ),
    (
        "{n} change",
        ["{n} изменение", "{n} изменения", "{n} изменений"],
    ),
    (
        "conflicts in {n} file",
        [
            "конфликты в {n} файле",
            "конфликты в {n} файлах",
            "конфликты в {n} файлах",
        ],
    ),
    (
        "{n} stash will be deleted. This can't be undone.",
        [
            "Будет удалён {n} stash. Это нельзя отменить.",
            "Будут удалены {n} stash. Это нельзя отменить.",
            "Будут удалены {n} stash. Это нельзя отменить.",
        ],
    ),
    (
        "Cleared {n} stash",
        ["Удалён {n} stash", "Удалено {n} stash", "Удалено {n} stash"],
    ),
];
