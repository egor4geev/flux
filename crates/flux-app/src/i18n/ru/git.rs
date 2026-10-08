//! Version control: the commit window, the diff viewer, the gutter, push, the operations menu.

pub(super) const STRINGS: &[(&str, &str)] = &[
    ("Commit", "Коммит"),
    ("Commit Message", "Сообщение коммита"),
    ("Commit and Push…", "Коммит и пуш…"),
    ("Amend", "Исправить последний"),
    ("Refresh", "Обновить"),
    ("Committing…", "Коммит…"),
    ("Rolling back…", "Откат…"),
    ("Pushing…", "Пуш…"),
    ("Pushing… {0}%", "Пуш… {0}%"),
    ("Not in a Git repository", "Не в репозитории Git"),
    ("Enter a commit message", "Введите сообщение коммита"),
    ("No changes are checked", "Не отмечено ни одного изменения"),
    ("Commit failed: {0}", "Коммит не удался: {0}"),
    ("Committed: {0}", "Закоммичено: {0}"),
    ("Nothing to push", "Нечего пушить"),
    ("HEAD", "HEAD"),
    ("Working copy", "Рабочая копия"),
    ("The file is deleted", "Файл удалён"),
    ("{0} (diff) — {1}", "{0} (дифф) — {1}"),
    ("diff", "дифф"),
    ("OK", "ОК"),
    (
        "“{0}” changed on disk; your unsaved changes are kept",
        "«{0}» изменён на диске; несохранённые правки оставлены",
    ),
    // Command palette: the section and the titles of the window's git actions.
    ("Git", "Git"),
    ("Push", "Пуш"),
    ("Toggle Commit Window", "Окно коммита"),
    ("Vcs Operations", "Операции VCS"),
];
