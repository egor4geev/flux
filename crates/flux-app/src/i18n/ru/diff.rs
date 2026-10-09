//! The diff viewer.

pub(super) const STRINGS: &[(&str, &str)] = &[
    ("Previous Difference", "Предыдущее различие"),
    ("Next Difference", "Следующее различие"),
    ("No differences", "Различий нет"),
    ("Differences: {0}", "Различий: {0}"),
    ("Side by Side", "Рядом"),
    ("Unified", "Единый вид"),
    ("Jump to Source", "Перейти к исходнику"),
    ("HEAD · {0}", "HEAD · {0}"),
    ("Deleted", "Удалён"),
    ("The file is new", "Новый файл"),
    ("Revert", "Откатить"),
    // The command palette: the namespace chip and the titles of the viewer's actions.
    ("Diff", "Дифф"),
    ("Jump To Source", "Перейти к исходнику"),
    ("Revert Change", "Откатить изменение"),
    ("Toggle Unified", "Рядом / единый вид"),
    ("Include in Commit", "Включить в коммит"),
    ("Binary files differ", "Двоичные файлы различаются"),
    (
        "Contents can't be compared: a binary or too large file",
        "Содержимое не сравнить: двоичный или слишком большой файл",
    ),
    // Comparisons of revisions.
    ("The file doesn't exist in {0}", "В {0} этого файла нет"),
    (
        "The file doesn't exist in the working tree",
        "В рабочей копии этого файла нет",
    ),
    ("stash", "stash"),
];
