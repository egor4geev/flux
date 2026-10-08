//! Settings: the window, the Language Servers section.

pub(super) const STRINGS: &[(&str, &str)] = &[
    ("Settings", "Настройки"),
    ("Language Servers", "Языковые серверы"),
    (
        "Flux starts the server for the language of an open file: errors, completion, go to definition. Servers installed on this Mac are used as they are; those Flux installed can be updated or deleted here.",
        "Flux запускает сервер языка открытого файла: ошибки, подсказки, переход к определению. Серверы, установленные на этом Mac, используются как есть; поставленные Flux здесь можно обновить или удалить.",
    ),
    (
        "Install missing servers automatically",
        "Ставить недостающие серверы автоматически",
    ),
    (
        "When a file needs a server that isn't on this Mac, Flux downloads it to:",
        "Когда файлу нужен сервер, которого нет на этом Mac, Flux скачивает его в каталог:",
    ),
    ("Installing…", "Установка…"),
    ("Updating…", "Обновление…"),
    ("Deleting…", "Удаление…"),
    ("Failed: {0}", "Не удалось: {0}"),
    ("Checking…", "Проверка…"),
    ("On this Mac · {0}", "На этом Mac · {0}"),
    ("Installed by Flux · {0}", "Поставлен Flux · {0}"),
    ("Installed by Flux", "Поставлен Flux"),
    ("Not installed", "Не установлен"),
    ("Not installed: {0}", "Не установлен: {0}"),
    ("Install", "Установить"),
    ("Update", "Обновить"),
    ("Delete", "Удалить"),
    (
        "Automatic installation is off: install it in Settings",
        "Автоустановка выключена: поставьте сервер в Настройках",
    ),
];
