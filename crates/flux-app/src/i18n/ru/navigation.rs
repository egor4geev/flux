//! Language server navigation and refactoring: go to definition, usages, reformat, rename.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // Palette.
    ("Navigation", "Навигация"),
    ("Go To Definition", "К определению"),
    ("Find Usages", "Найти использования"),
    ("Reformat Code", "Переформатировать код"),
    ("Rename Symbol", "Переименовать символ"),
    ("Navigate Back", "Назад"),
    ("Navigate Forward", "Вперёд"),
    // Requests.
    (
        "The language server is starting…",
        "Языковой сервер запускается…",
    ),
    (
        "No language server for this file",
        "Для этого файла нет языкового сервера",
    ),
    ("{0} doesn't support this", "{0} этого не умеет"),
    ("{0} has stopped", "{0} остановился"),
    ("unexpected response: {0}", "непонятный ответ: {0}"),
    (
        "the language server is starting",
        "языковой сервер запускается",
    ),
    (
        "Go to definition failed: {0}",
        "Не удалось перейти к определению: {0}",
    ),
    ("No definition found", "Определение не найдено"),
    (
        "Find usages failed: {0}",
        "Не удалось найти использования: {0}",
    ),
    ("No usages found", "Использований не найдено"),
    // The list of places.
    ("Filter definitions…", "Фильтр определений…"),
    ("Filter usages of {0}…", "Фильтр использований {0}…"),
    ("Filter usages…", "Фильтр использований…"),
    ("No matching places", "Подходящих мест нет"),
    // Reformat.
    (
        "The file changed while reformatting — try again",
        "Файл изменился во время форматирования — попробуйте ещё раз",
    ),
    ("Reformatted", "Код переформатирован"),
    ("Already formatted", "Код уже отформатирован"),
    ("Reformat failed: {0}", "Не удалось переформатировать: {0}"),
    // Rename.
    (
        "This symbol can't be renamed",
        "Этот символ нельзя переименовать",
    ),
    ("Can't rename: {0}", "Нельзя переименовать: {0}"),
    (
        "Place the cursor on a symbol to rename it",
        "Поставьте курсор на символ, чтобы переименовать его",
    ),
    ("New name", "Новое имя"),
    ("Type a new name", "Введите новое имя"),
    ("Nothing to rename here", "Здесь нечего переименовывать"),
    ("Renamed {0}", "Переименовано {0}"),
    ("{0}; not written: {1}", "{0}; не записаны: {1}"),
    ("file not found", "файл не найден"),
    ("Renaming…", "Переименование…"),
    ("rename", "переименовать"),
    ("cancel", "отмена"),
];
