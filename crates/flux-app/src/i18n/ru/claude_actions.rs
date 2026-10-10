//! Part 9.2: Claude around the editor — the editor's context menu, Explain / Fix with Claude, Send to Claude (`editor_menu.rs`, `claude_actions.rs`, `context_menu.rs`).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The editor's context menu.
    ("Show Context Actions", "Показать контекстные действия"),
    ("Copy Path/Reference", "Копировать путь/ссылку"),
    ("Absolute Path", "Абсолютный путь"),
    ("Path From Project Root", "Путь от корня проекта"),
    ("File Name", "Имя файла"),
    ("Reference", "Ссылка"),
    ("Refactor", "Рефакторинг"),
    ("Go To", "Перейти"),
    ("Declaration or Usages", "К объявлению или использованиям"),
    ("Line…", "К строке…"),
    ("Forward", "Вперёд"),
    ("Open In", "Открыть в"),
    ("Finder", "Finder"),
    ("Send Selection to Claude", "Отправить выделение в Claude"),
    ("Send File to Claude", "Отправить файл в Claude"),
    ("Send to Claude", "Отправить в Claude"),
    // The ready requests.
    ("Explain with Claude", "Объяснить с помощью Claude"),
    ("Fix with Claude", "Исправить с помощью Claude"),
    ("Find Problems with Claude", "Найти проблемы с помощью Claude"),
    ("Write Tests with Claude", "Написать тесты с помощью Claude"),
    ("Add Documentation with Claude", "Написать документацию с помощью Claude"),
    ("More actions…", "Другие действия…"),
    ("No problems here", "Здесь нет проблем"),
    (
        "Save the file first: Claude reads it from the disk",
        "Сначала сохраните файл: Claude читает его с диска",
    ),
    // The sessions' titles.
    ("Explain {0}", "Объяснить {0}"),
    ("Fix: {0}", "Исправить: {0}"),
    ("Find problems in {0}", "Проблемы в {0}"),
    ("Tests for {0}", "Тесты для {0}"),
    ("Document {0}", "Документация для {0}"),
    // The requests, in the language of the interface.
    (
        "Explain what the code in {0} does and how it works.",
        "Объясни, что делает код в {0} и как он работает.",
    ),
    (
        "Fix the problems in {0} the language server reports:\n{1}",
        "Исправь проблемы в {0} — их нашёл языковой сервер:\n{1}",
    ),
    (
        "Review the code in {0} for bugs and problems. List what you find with the lines; don't change the code yet.",
        "Проверь код в {0} на ошибки и проблемы. Перечисли найденное с номерами строк; код пока не меняй.",
    ),
    (
        "Write tests for the code in {0} in the style of the project's existing tests and run them.",
        "Напиши тесты для кода в {0} в стиле существующих тестов проекта и запусти их.",
    ),
    (
        "Write documentation comments for the code in {0} in the style of the project.",
        "Напиши документирующие комментарии для кода в {0} в стиле проекта.",
    ),
    // The palette (action names).
    ("Explain With Claude", "Объяснить с Claude"),
    ("Fix With Claude", "Исправить с Claude"),
    ("Find Problems With Claude", "Найти проблемы с Claude"),
    ("Write Tests With Claude", "Написать тесты с Claude"),
    ("Document With Claude", "Написать документацию с Claude"),
    ("Copy Absolute Path", "Копировать абсолютный путь"),
    ("Copy Path From Root", "Копировать путь от корня проекта"),
    ("Copy File Name", "Копировать имя файла"),
    ("Copy Reference", "Копировать ссылку"),
    ("Open In Terminal", "Открыть в терминале"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
