//! The welcome guide's setup (part 10.7): the look (themes, the window's look, the scale) and the
//! languages (the catalog's plugins, the ones found on this Mac, their installs).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The look.
    ("Setup · Appearance", "Настройка · внешний вид"),
    ("Make Flux Yours", "Каким будет Flux"),
    (
        "Everything changes at once — this window already shows your choice. Later: Settings → Appearance, or {0} Quick Switch.",
        "Всё меняется сразу — это окно уже показывает выбор. Потом: Настройки → Внешний вид или {0} Быстрое переключение.",
    ),
    (
        "Everything changes at once — this window already shows your choice. Later: Settings → Appearance.",
        "Всё меняется сразу — это окно уже показывает выбор. Потом: Настройки → Внешний вид.",
    ),
    ("Catalog", "Каталог"),
    ("Couldn't install: click to try again", "Не установилась: щёлкните, чтобы повторить"),
    ("Window Style", "Стиль окна"),
    // The languages.
    ("Setup · Languages", "Настройка · языки"),
    ("What Do You Write In?", "На чём вы пишете?"),
    (
        "The languages found on this Mac are checked. Flux installs their plugins from the catalog; a language server comes by itself when you open the first file.",
        "Отмечены языки, которые нашлись на этом Mac. Flux поставит их плагины из каталога, а языковой сервер скачает сам, когда вы откроете первый файл.",
    ),
    ("Already installed", "Уже установлен"),
    ("Ready to use", "Готов к работе"),
    ("Couldn't install", "Не установился"),
    ("Found on this Mac", "Найден на этом Mac"),
    ("In every project", "Есть в каждом проекте"),
    ("Highlighting, errors, completion", "Подсветка, ошибки, подсказки"),
    ("Highlighting", "Подсветка"),
    ("JavaScript · TypeScript", "JavaScript · TypeScript"),
    ("Come with Flux", "Встроены во Flux"),
    ("Reading the catalog…", "Читаем каталог…"),
    (
        "The catalog can't be reached. Languages can be installed later: Settings → Plugins.",
        "Каталог недоступен. Языки можно поставить потом: Настройки → Плагины.",
    ),
    (
        "Themes, file icons and tools are in the Marketplace: Settings → Plugins.",
        "Темы, значки файлов и инструменты — в Маркетплейсе: Настройки → Плагины.",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[(
    "Install {n} and Continue",
    [
        "Установить {n} и продолжить",
        "Установить {n} и продолжить",
        "Установить {n} и продолжить",
    ],
)];
