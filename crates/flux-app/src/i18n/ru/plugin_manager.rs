//! Stage 8: Settings → Plugins (the plugin manager, installing from disk) and the plugins' settings
//! pages.

pub(super) const STRINGS: &[(&str, &str)] = &[
    ("Plugins", "Плагины"),
    (
        "Plugins add commands, tool windows and settings. Each runs in its own sandbox: a plugin can't crash Flux, and it reaches only what its permissions allow.",
        "Плагины добавляют команды, окна инструментов и настройки. Каждый работает в своей песочнице: плагин не может уронить Flux и получает доступ только к тому, что ему разрешено.",
    ),
    ("Install and Reload", "Установка и перезагрузка"),
    ("Install Plugin from Disk…", "Установить плагин с диска…"),
    (
        "Reload Development Plugins",
        "Перезагрузить плагины в разработке",
    ),
    (
        "Search installed plugins",
        "Поиск по установленным плагинам",
    ),
    ("Development", "В разработке"),
    ("Downloaded", "Скачанные"),
    ("Bundled", "Встроенные"),
    ("Built-in", "Встроенный"),
    ("Installed", "Установлен"),
    ("No plugins yet", "Плагинов пока нет"),
    (
        "Install one from a folder or an archive: the gear menu above.",
        "Установите плагин из папки или архива: меню шестерёнки вверху.",
    ),
    ("No plugins found", "Плагины не найдены"),
    (
        "Select a plugin to see its details",
        "Выберите плагин, чтобы увидеть подробности",
    ),
    (
        "Search looks in names, descriptions and authors.",
        "Поиск идёт по названиям, описаниям и авторам.",
    ),
    ("Running", "Работает"),
    (
        "Built for plugin API {0}; this Flux has {1}",
        "Сделан для API плагинов {0}, а у этого Flux — {1}",
    ),
    ("The component {0} is missing", "Нет компонента {0}"),
    ("Disabled", "Выключен"),
    ("Stopped: {0}", "Остановлен: {0}"),
    ("Enable", "Включить"),
    ("Disable", "Выключить"),
    ("Reload", "Перезагрузить"),
    (
        "Builds the plugin with cargo and loads it again",
        "Собирает плагин через cargo и загружает заново",
    ),
    ("Uninstall", "Удалить"),
    ("Uninstall “{0}”?", "Удалить «{0}»?"),
    (
        "Flux stops using the plugin's folder; the folder itself stays on disk.",
        "Flux перестанет использовать папку плагина; сама папка останется на диске.",
    ),
    (
        "The plugin is deleted; its data and settings stay, in case it comes back.",
        "Плагин будет удалён; его данные и настройки останутся — на случай, если он вернётся.",
    ),
    ("Couldn't uninstall “{0}”", "Не удалось удалить «{0}»"),
    ("“{0}” stopped", "«{0}» остановлен"),
    ("Overview", "Обзор"),
    ("Permissions", "Разрешения"),
    ("Contributions", "Что добавляет"),
    ("The plugin has no description.", "У плагина нет описания."),
    ("Identifier", "Идентификатор"),
    ("Plugin API", "API плагинов"),
    ("Folder", "Папка"),
    ("Show in Finder", "Показать в Finder"),
    (
        "Read and search the files of the project",
        "Читать файлы проекта и искать по ним",
    ),
    (
        "Read, search and change the files of the project",
        "Читать, искать и изменять файлы проекта",
    ),
    (
        "Asks for no special permissions",
        "Особых разрешений не просит",
    ),
    (
        "Every plugin may show notifications and questions, and add what its manifest declares. Beyond that, the sandbox lets it reach only its own folder and what is listed here: no other files, no network, no programs.",
        "Любой плагин может показывать уведомления и вопросы и добавлять то, что объявлено в его манифесте. Сверх этого песочница пускает его только в его собственную папку и к тому, что перечислено здесь: никаких других файлов, сети и программ.",
    ),
    ("Commands", "Команды"),
    ("Tool Windows", "Окна инструментов"),
    ("Status Bar", "Статус-бар"),
    (
        "The plugin adds nothing to the window.",
        "Плагин ничего не добавляет в окно.",
    ),
    ("Nothing in the log yet", "В логе пока пусто"),
    ("Can't install the plugin", "Не удаётся установить плагин"),
    ("Can't install “{0}”", "Не удаётся установить «{0}»"),
    ("Couldn't install “{0}”", "Не удалось установить «{0}»"),
    ("Couldn't Load", "Не загрузились"),
    ("Install “{0}” {1}?", "Установить «{0}» {1}?"),
    ("By {0}", "Автор: {0}"),
    (
        "The plugin asks for no special permissions.",
        "Плагин не просит особых разрешений.",
    ),
    ("The plugin will be able to:", "Плагин сможет:"),
    (
        "It will be linked as a plugin under development: Flux builds it with cargo and reloads it when it changes.",
        "Он будет подключён как плагин в разработке: Flux сам соберёт его через cargo и перезагрузит при изменении.",
    ),
    // A plugin's settings page.
    (
        "Settings of the plugin “{0}”: it reads them and follows their changes.",
        "Настройки плагина «{0}»: он читает их и следит за изменениями.",
    ),
    ("The plugin has no settings.", "У плагина нет настроек."),
    ("Add", "Добавить"),
    ("The list is empty", "Список пуст"),
    ("A whole number", "Целое число"),
    ("From {0} to {1}", "От {0} до {1}"),
    ("At least {0}", "Не меньше {0}"),
    ("At most {0}", "Не больше {0}"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[
    ("{n} item", ["{n} пункт", "{n} пункта", "{n} пунктов"]),
    ("{n} line", ["{n} строка", "{n} строки", "{n} строк"]),
    (
        "{n} setting",
        ["{n} настройка", "{n} настройки", "{n} настроек"],
    ),
];
