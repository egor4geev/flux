//! Stage 8.3: the Marketplace, plugin updates, plugin suggestions (agent E).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The page's tabs, the Installed tab.
    ("Installed", "Установленные"),
    ("Check for Updates", "Проверить обновления"),
    ("Check for updates at start", "Обновления при запуске"),
    ("Update All", "Обновить все"),
    ("Update available: {0}", "Доступно обновление: {0}"),
    ("Update to {0}", "Обновить до {0}"),
    ("Part of Flux", "Часть Flux"),
    // The Contributions tab.
    ("Languages", "Языки"),
    ("grammar built into Flux", "грамматика встроена во Flux"),
    ("WebAssembly grammar", "грамматика WebAssembly"),
    ("uses the one on this Mac", "берётся с этого Mac"),
    ("installs with npm: {0}", "ставится через npm: {0}"),
    ("installs from GitHub: {0}", "ставится с GitHub: {0}"),
    ("installs with go install: {0}", "ставится через go install: {0}"),
    ("installs with rustup: {0}", "ставится через rustup: {0}"),
    ("Color Themes", "Цветовые темы"),
    ("Icon Sets", "Наборы значков"),
    ("Problems", "Проблемы"),
    // The Marketplace.
    ("Search the Marketplace", "Поиск в Marketplace"),
    ("Themes", "Темы"),
    ("Icons", "Значки"),
    ("Tools", "Инструменты"),
    ("Loading the catalog…", "Загрузка каталога…"),
    ("The catalog isn't loaded", "Каталог не загружен"),
    (
        "Flux reads it from the catalog's repository on GitHub.",
        "Flux читает его из репозитория каталога на GitHub.",
    ),
    ("Nothing found", "Ничего не найдено"),
    (
        "Search looks in names, descriptions, authors and languages.",
        "Поиск — по названиям, описаниям, авторам и языкам.",
    ),
    ("The catalog is empty", "Каталог пуст"),
    ("Under development", "В разработке"),
    ("Needs a newer Flux", "Нужен более новый Flux"),
    ("Installed: {0}", "Установлен: {0}"),
    (
        "A version under development is linked: the catalog doesn't replace it",
        "Подключена версия в разработке: каталог её не заменяет",
    ),
    ("Download", "Загрузка"),
    ("Updated", "Обновлён"),
    (
        "The catalog has a manifest Flux can't read for this plugin",
        "Манифест этого плагина в каталоге Flux прочитать не может",
    ),
    ("Couldn't load the catalog", "Не удалось загрузить каталог"),
    ("Retry", "Повторить"),
    ("Downloading…", "Загрузка…"),
    ("Downloading {0}%", "Загрузка {0}%"),
    ("“{0}” {1} is installed", "«{0}» {1} установлен"),
    ("“{0}” is updated to {1}", "«{0}» обновлён до {1}"),
    ("The window is closed", "Окно закрыто"),
    // Updates.
    ("All plugins are up to date", "Все плагины обновлены"),
    ("Couldn't check for plugin updates", "Не удалось проверить обновления плагинов"),
    // Suggestions.
    ("Plugins supporting {0} files found.", "Найдены плагины для файлов {0}."),
    ("Install {0}", "Установить {0}"),
    ("Installing “{0}”: {1}", "Установка «{0}»: {1}"),
    ("Ignore Extension", "Игнорировать расширение"),
    ("Ignore", "Игнорировать"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[
    ("{n} update", ["{n} обновление", "{n} обновления", "{n} обновлений"]),
    (
        "Plugin update available",
        [
            "Доступно обновление плагина",
            "Доступны обновления плагинов",
            "Доступны обновления плагинов",
        ],
    ),
];
