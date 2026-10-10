//! Part 8.2: the plugins' items in the context menus, their permissions in Settings → Plugins and
//! in the install question (agent C).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // Permissions, in plain words.
    (
        "Connect to any server on the internet",
        "Подключаться к любым серверам в интернете",
    ),
    ("Connect to {0}", "Подключаться к {0}"),
    (
        "Accept connections from programs on this Mac",
        "Принимать подключения от программ на этом Mac",
    ),
    (
        "Run any program with your rights",
        "Запускать любые программы с вашими правами",
    ),
    ("Run programs: {0}", "Запускать программы: {0}"),
    (
        "Open terminals and type commands into them",
        "Открывать терминалы и вводить в них команды",
    ),
    ("Read the folder {0}", "Читать папку {0}"),
    (
        "Read and change the folder {0}",
        "Читать и изменять папку {0}",
    ),
    // The Contributions tab.
    ("Context Menus", "Контекстные меню"),
    ("Editor menu: {0}", "Меню редактора: {0}"),
    ("Project tree menu: {0}", "Меню дерева проекта: {0}"),
    ("Tab menu: {0}", "Меню вкладки: {0}"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
