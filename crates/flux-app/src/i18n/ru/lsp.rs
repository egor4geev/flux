//! Language servers: server status, diagnostics, and their commands.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // Server status in the status bar.
    ("Starting…", "Запуск…"),
    ("{0} stopped", "{0} остановлен"),
    ("Click to restart", "Щёлкните, чтобы перезапустить"),
    ("Restarting {0}…", "Перезапуск {0}…"),
    ("{0} is not installed", "{0} не установлен"),
    ("Installing {0}…", "Установка {0}…"),
    ("Couldn't install {0}", "Не удалось установить {0}"),
    ("Click to retry", "Щёлкните, чтобы повторить"),
    // Why a server can't be installed (the installer's messages).
    ("Needs Go", "Нужен Go"),
    ("Can't be installed on this system", "Не устанавливается в этой системе"),
    ("Flux can't install it", "Flux не умеет его устанавливать"),
    ("Installation canceled", "Установка отменена"),
    (
        "No language server for this file",
        "Для этого файла нет языкового сервера",
    ),
    // Diagnostics.
    ("Error", "Ошибка"),
    ("Warning", "Предупреждение"),
    ("Info", "Сведения"),
    ("Hint", "Подсказка"),
    ("No problems", "Проблем нет"),
    (
        "Errors: {0} · Warnings: {1}",
        "Ошибок: {0} · Предупреждений: {1}",
    ),
    // Command palette: sections and commands.
    ("Language Server", "Языковой сервер"),
    ("Restart", "Перезапустить"),
    ("Diagnostics", "Диагностика"),
    ("Next Problem", "Следующая проблема"),
    ("Previous Problem", "Предыдущая проблема"),
];
