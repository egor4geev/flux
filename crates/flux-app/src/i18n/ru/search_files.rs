//! Part 10.4: files by name in Find in Files (`project_search.rs`).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The empty window: what to type.
    (
        "Type text to find it in every file, or a file’s name to open the file.",
        "Введите текст, чтобы найти его во всех файлах, или имя файла, чтобы открыть файл.",
    ),
    // Under «Text» when the files by name were found and the text wasn't.
    ("No text matches", "В тексте совпадений нет"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[(
    "{n} more file",
    ["ещё {n} файл", "ещё {n} файла", "ещё {n} файлов"],
)];
