//! Part 10.5: soft wraps (`wrap.rs`) and Settings → Editor (`editor_settings.rs`).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The command (the palette, the editor's context menu), as JetBrains IDEs' View → Active
    // Editor → Soft-Wrap.
    ("Soft-Wrap", "Мягкий перенос"),
    // Settings → Editor.
    (
        "Soft wraps: a long line goes on in the next rows instead of past the edge of the editor. A commit message always wraps.",
        "Мягкий перенос: длинная строка продолжается на следующих строках экрана, а не уходит за край редактора. Сообщение коммита переносится всегда.",
    ),
    ("Soft Wraps", "Мягкие переносы"),
    ("Soft-wrap these files:", "Мягкий перенос в файлах:"),
    (
        "Masks of file names, separated with semicolons: *.md, README.*. Other files keep long lines whole, with a horizontal scroll.",
        "Маски имён файлов через точку с запятой: *.md, README.*. В остальных файлах длинные строки не переносятся — их прокручивают по горизонтали.",
    ),
    (
        "Use original line’s indent for wrapped fragments",
        "Сохранять отступ строки в перенесённых частях",
    ),
    (
        "The wrapped part of a line starts under the line’s text, not at the edge.",
        "Перенесённая часть строки начинается под её текстом, а не от края.",
    ),
    (
        "Add additional indent, in spaces:",
        "Дополнительный отступ, в пробелах:",
    ),
    (
        "Only show soft-wrap indicators for the current line",
        "Знаки переноса — только в текущей строке",
    ),
    (
        "⤶ marks where a line wraps and ⤷ where it goes on. Off: on every wrapped line.",
        "⤶ — где строка переносится, ⤷ — где продолжается. Если выключено — во всех перенесённых строках.",
    ),
    (
        "Soft-Wrap: on or off in the editor you are in, whatever the file",
        "Мягкий перенос: включить или выключить в текущем редакторе для любого файла",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
