//! The terminal panel, terminal tabs and split panes.

pub(super) const STRINGS: &[(&str, &str)] = &[
    ("New Terminal", "Новый терминал"),
    (
        "Couldn't start the shell: {0}",
        "Не удалось запустить оболочку: {0}",
    ),
    ("No terminals", "Нет терминалов"),
    ("Try Again", "Повторить"),
    ("Hide", "Скрыть"),
    // Splits and tabs; also command titles in the palette.
    ("Split Right", "Разделить вправо"),
    ("Split Down", "Разделить вниз"),
    ("Close Pane", "Закрыть область"),
    ("Focus Next Pane", "Следующая область"),
    ("Focus Previous Pane", "Предыдущая область"),
    ("Move to Editor", "Перенести в редактор"),
    ("Close Other Tabs", "Закрыть другие вкладки"),
    // Closing a terminal that runs a command.
    ("Terminate “{0}”?", "Завершить «{0}»?"),
    (
        "The process is still running in this terminal.",
        "Процесс ещё работает в этом терминале.",
    ),
    (
        "Terminate running processes?",
        "Завершить запущенные процессы?",
    ),
    (
        "Still running in this tab: {0}.",
        "В этой вкладке ещё работают: {0}.",
    ),
    (
        "Still running in other tabs: {0}.",
        "В других вкладках ещё работают: {0}.",
    ),
    ("Terminate", "Завершить"),
];
