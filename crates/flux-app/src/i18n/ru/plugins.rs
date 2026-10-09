//! Stage 8: the plugins hub — the plugins' notifications, their questions and documents, the
//! palette's plugin commands.

pub(super) const STRINGS: &[(&str, &str)] = &[
    ("Plugins", "Плагины"),
    ("Disable", "Выключить"),
    ("Plugin «{0}» stopped", "Плагин «{0}» остановлен"),
    (
        "Couldn't build plugin «{0}»",
        "Не удалось собрать плагин «{0}»",
    ),
    ("Plugin «{0}» reloaded", "Плагин «{0}» перезагружен"),
    ("Building «{0}»…", "Сборка «{0}»…"),
    ("Building the plugin…", "Плагин собирается…"),
    ("The build failed", "Сборка не удалась"),
    ("The plugin is not installed", "Плагин не установлен"),
    (
        "A bundled plugin can't be removed; turn it off instead",
        "Встроенный плагин нельзя удалить — его можно выключить",
    ),
    ("Toggle {0} Window", "Окно {0}"),
    ("The document is closed", "Документ закрыт"),
    ("No selections given", "Выделения не заданы"),
    ("The document is read-only", "Документ только для чтения"),
    ("The edits overlap", "Правки пересекаются"),
    ("Couldn't open {0}", "Не удалось открыть {0}"),
    // The palette's titles of the plugins' actions (`plugins::OpenManager`…).
    ("Open Manager", "Открыть менеджер"),
    ("Install From Disk", "Установить с диска"),
    ("Reload Dev Plugins", "Перезагрузить плагины в разработке"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
