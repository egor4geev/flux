//! Updates of Flux (part 10.8): the check, the card of a new version, its release notes, the
//! download in the status bar, Settings → Updates.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The Flux menu and the command palette.
    ("Check for Updates…", "Проверить обновления…"),
    // The check and its answers.
    ("Flux {0} is up to date", "Flux {0} — последняя версия"),
    (
        "Couldn't check for updates",
        "Не удалось проверить обновления",
    ),
    (
        "No connection to GitHub: check the network and try again",
        "Нет связи с GitHub: проверьте сеть и попробуйте ещё раз",
    ),
    (
        "Flux {0} can't be installed by this version: download it from its release page",
        "Flux {0} не установить из этой версии: скачайте его со страницы релиза",
    ),
    (
        "The new version can't be installed by this one: download it from its release page",
        "Новую версию не установить из этой: скачайте её со страницы релиза",
    ),
    // The card of a new version.
    ("Flux {0} is available", "Доступен Flux {0}"),
    (
        "A beta of the next version · released {0}",
        "Бета следующей версии · вышла {0}",
    ),
    ("Released {0}", "Вышел {0}"),
    ("Update and Restart", "Обновить и перезапустить"),
    ("Skip This Version", "Пропустить эту версию"),
    // The download and the restart.
    (
        "Couldn't update to Flux {0}",
        "Не удалось обновиться до Flux {0}",
    ),
    ("Couldn't update Flux", "Не удалось обновить Flux"),
    ("Open Release Page", "Открыть страницу релиза"),
    (
        "Restart Flux to finish the update",
        "Перезапустите Flux, чтобы завершить обновление",
    ),
    (
        "Flux {0} is downloaded and checked: it installs when Flux restarts.",
        "Flux {0} скачан и проверен: он установится при перезапуске.",
    ),
    ("Restart Now", "Перезапустить"),
    (
        "Flux can't update itself here",
        "Flux не может обновить себя здесь",
    ),
    (
        "This Flux doesn't run from an app bundle (a build from the sources). Download the new version from its release page.",
        "Этот Flux запущен не из пакета приложения (сборка из исходников). Скачайте новую версию со страницы релиза.",
    ),
    (
        "Flux can't write to {0}. Download the new version and replace Flux yourself, or move Flux to a folder you can change, such as Applications.",
        "Flux не может писать в {0}. Скачайте новую версию и замените Flux сами или перенесите Flux в папку, которую можно менять, например в «Программы».",
    ),
    (
        "Flux runs from the disk image or another read-only place. Move Flux to the Applications folder, open it from there and update again.",
        "Flux запущен с образа диска или из другого места только для чтения. Перенесите Flux в папку «Программы», откройте его оттуда и обновите снова.",
    ),
    ("Download…", "Скачать…"),
    // The status bar.
    ("Downloading Flux {0}", "Скачивается Flux {0}"),
    ("Checking the update…", "Проверка обновления…"),
    (
        "Checking the download of Flux {0}",
        "Проверка скачанного Flux {0}",
    ),
    ("Restart to update", "Перезапустить для обновления"),
    (
        "Flux {0} installs when Flux restarts",
        "Flux {0} установится при перезапуске",
    ),
    ("Cancel Update", "Отменить обновление"),
    // The release notes.
    ("This release has no notes.", "У этого релиза нет заметок."),
    ("Release page", "Страница релиза"),
    ("Downloading… {0}%", "Скачивание… {0}%"),
    ("Later", "Позже"),
    ("Released {0} · you have {1}", "Вышел {0} · у вас {1}"),
    // Settings → Updates.
    ("Updates", "Обновления"),
    (
        "Flux looks for new versions on GitHub. An update downloads in the background and installs when Flux restarts.",
        "Flux ищет новые версии на GitHub. Обновление скачивается в фоне и устанавливается при перезапуске.",
    ),
    ("Stable", "Стабильный"),
    ("Beta", "Бета"),
    ("Last checked just now", "Проверено только что"),
    ("Last checked {0} ago", "Проверено {0} назад"),
    ("Not checked yet", "Ещё не проверялось"),
    ("Check Now", "Проверить"),
    ("Downloading Flux {0}… {1}%", "Скачивается Flux {0}… {1}%"),
    (
        "Checking the download of Flux {0}…",
        "Проверка скачанного Flux {0}…",
    ),
    (
        "Flux {0} is ready: it installs when Flux restarts",
        "Flux {0} готов: он установится при перезапуске",
    ),
    ("Checking for updates…", "Проверка обновлений…"),
    (
        "Flux {0} is available (skipped)",
        "Доступен Flux {0} (пропущен)",
    ),
    ("You have the latest version", "У вас последняя версия"),
    (
        "Couldn't check for updates: {0}",
        "Не удалось проверить обновления: {0}",
    ),
    (
        "Flux looks for a new version on its own",
        "Flux сам ищет новую версию",
    ),
    (
        "Released versions, tested in betas first",
        "Выпущенные версии, сначала проверенные в бете",
    ),
    (
        "Earlier builds of the next version: new features sooner, a few rough edges",
        "Ранние сборки следующей версии: новое раньше, но возможны шероховатости",
    ),
    (
        "Check for updates automatically",
        "Проверять обновления автоматически",
    ),
    (
        "A few seconds after Flux starts, then once a day",
        "Через несколько секунд после запуска, затем раз в день",
    ),
    ("Update Channel", "Канал обновлений"),
    (
        "A build of a beta starts on the Beta channel. Switching to Stable waits for the next stable version.",
        "Бета-сборка сразу получает канал «Бета». После перехода на «Стабильный» Flux дождётся следующей стабильной версии.",
    ),
    // A release's date: "October 20, 2026" — «20 октября 2026».
    ("{0} {1}, {2}", "{1} {0} {2}"),
    ("January", "января"),
    ("February", "февраля"),
    ("March", "марта"),
    ("April", "апреля"),
    ("May", "мая"),
    ("June", "июня"),
    ("July", "июля"),
    ("August", "августа"),
    ("September", "сентября"),
    ("October", "октября"),
    ("November", "ноября"),
    ("December", "декабря"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
