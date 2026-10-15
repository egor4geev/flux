//! Part 10.5: the editor's scrollbar, its marks and the problems widget (`scrollbar.rs`).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The tooltip of the marks: a Git change against HEAD (lines from 1).
    ("Added line {0}", "Добавлена строка {0}"),
    ("Added lines {0}–{1}", "Добавлены строки {0}–{1}"),
    ("Modified line {0}", "Изменена строка {0}"),
    ("Modified lines {0}–{1}", "Изменены строки {0}–{1}"),
    (
        "Lines deleted after line {0}",
        "Удалены строки после строки {0}",
    ),
    ("Lines deleted at the start", "Удалены строки в начале"),
    // The problems widget at the editor's top right, with infos.
    (
        "Errors: {0} · Warnings: {1} · Info: {2}",
        "Ошибок: {0} · Предупреждений: {1} · Сведений: {2}",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
