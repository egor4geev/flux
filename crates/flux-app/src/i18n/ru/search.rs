//! The find bar and Find in Files.

pub(super) const STRINGS: &[(&str, &str)] = &[
    ("Find", "Найти"),
    ("Toggle Replace", "Показать замену"),
    ("Whole Word", "Слово целиком"),
    ("Regular Expression", "Регулярное выражение"),
    ("Previous Match", "Предыдущее совпадение"),
    ("Next Match", "Следующее совпадение"),
    ("Search in project", "Поиск по проекту"),
    ("Words", "Слова"),
    ("Regex", "Regex"),
    ("No project folder", "Нет папки проекта"),
    (
        "Open a folder with {0} to search across its files.",
        "Откройте папку через {0}, чтобы искать по её файлам.",
    ),
    ("Search across the project", "Поиск по всему проекту"),
    (
        "Type text to find it in every file.",
        "Введите текст, чтобы найти его во всех файлах.",
    ),
    (
        "Match Case {0}, Words {1} and Regex {2} narrow the search; ↑↓ pick a result, {3} opens it.",
        "Регистр {0}, слова {1} и regex {2} сужают поиск; ↑↓ — выбор результата, {3} — открыть.",
    ),
    (
        "Looking for “{0}” in the project files.",
        "Ищем «{0}» в файлах проекта.",
    ),
    ("No results for “{0}”", "Ничего не найдено по запросу «{0}»"),
    (
        "Check the spelling or turn off Match Case, Words and Regex.",
        "Проверьте написание или отключите регистр, слова и regex.",
    ),
    (
        "Invalid regular expression",
        "Некорректное регулярное выражение",
    ),
    (
        "{0} MB — too large to preview",
        "{0} МБ — слишком большой для предпросмотра",
    ),
    ("Searching… {0}", "Поиск… {0}"),
];
