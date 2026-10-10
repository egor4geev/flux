//! Stage 8.3: Settings → Appearance pages, Quick Switch, the interface language (agent C2).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // Settings → Appearance → Theme.
    (
        "The colors of the window and of the code. Themes come with plugins: Flux has Flux Night and Flux Day, more are in the Marketplace.",
        "Цвета окна и кода. Темы приносят плагины: во Flux есть Flux Night и Flux Day, другие — в Marketplace.",
    ),
    ("Dark themes", "Тёмные темы"),
    ("Light themes", "Светлые темы"),
    ("While macOS is dark", "Пока в macOS тёмный режим"),
    ("While macOS is light", "Пока в macOS светлый режим"),
    (
        "Switch between a light and a dark theme together with macOS.",
        "Переключать светлую и тёмную тему вместе с macOS.",
    ),
    (
        "macOS is dark now: Flux shows the dark theme.",
        "Сейчас в macOS тёмный режим — Flux показывает тёмную тему.",
    ),
    (
        "macOS is light now: Flux shows the light theme.",
        "Сейчас в macOS светлый режим — Flux показывает светлую тему.",
    ),
    ("Built into Flux", "Встроена во Flux"),
    (
        "Get more themes in the Marketplace",
        "Другие темы — в Marketplace",
    ),
    // Settings → Appearance → File Icons.
    (
        "The icons of files and folders in the project tree, tabs and lists. Sets of icons come with plugins.",
        "Значки файлов и папок в дереве проекта, на вкладках и в списках. Наборы значков приносят плагины.",
    ),
    (
        "No sets of icons: the plugins that bring them are turned off. Files show plain icons.",
        "Наборов значков нет: плагины, которые их приносят, выключены. У файлов — простые значки.",
    ),
    (
        "Get more icon sets in the Marketplace",
        "Другие наборы значков — в Marketplace",
    ),
    // Settings → Appearance → Language.
    (
        "The language of menus, windows and messages. It changes at once; what is already shown, such as notifications, stays as it was.",
        "Язык меню, окон и сообщений. Меняется сразу; то, что уже показано, например уведомления, остаётся как было.",
    ),
    ("Now: {0}", "Сейчас: {0}"),
    (
        "FLUX_LANG={0} sets the language of this run; the choice applies without it.",
        "FLUX_LANG={0} задаёт язык этого запуска; выбор действует без этой переменной.",
    ),
    // Quick Switch, and its commands in the palette (`quick_switch::SelectTheme` → «Quick Switch:
    // Select Theme»).
    ("Quick Switch", "Быстрое переключение"),
    ("Select Theme", "Выбрать тему"),
    ("Select File Icons", "Выбрать значки файлов"),
    ("Select Language", "Выбрать язык"),
    ("choose", "выбрать"),
    (
        "No sets of icons: their plugins are turned off",
        "Наборов значков нет: их плагины выключены",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
