//! Messages turned into notifications and dialogs (stage 7, part C).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // Git: results and errors of the commit window.
    ("Committed {0}", "Закоммичено {0}"),
    ("Couldn't add to .gitignore", "Не удалось добавить в .gitignore"),
    // Files: the tree, open documents, the disk.
    ("Couldn't move to Trash", "Не удалось переместить в Корзину"),
    ("Couldn't rename", "Не удалось переименовать"),
    ("Couldn't create", "Не удалось создать"),
    ("Couldn't move", "Не удалось переместить"),
    ("Couldn't copy", "Не удалось скопировать"),
    ("Cannot read {0}", "Не удалось прочитать {0}"),
    (
        "Not watching the project for changes",
        "Изменения в проекте не отслеживаются",
    ),
    ("Couldn't save {0}", "Не удалось сохранить {0}"),
    ("Cannot open {0}", "Не удалось открыть {0}"),
    ("“{0}” changed on disk", "«{0}» изменён на диске"),
    (
        "Your unsaved changes are kept",
        "Несохранённые правки оставлены",
    ),
    // Terminal.
    ("Couldn't start the shell", "Не удалось запустить оболочку"),
    // Language servers.
    ("Installed {0}", "Установлен {0}"),
];
