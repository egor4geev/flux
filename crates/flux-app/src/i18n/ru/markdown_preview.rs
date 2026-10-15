//! Part 10.4: the Markdown preview (`markdown_preview.rs`) — Editor / Editor and Preview / Preview.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The layout buttons at the top right of the editor.
    ("Editor Only", "Только редактор"),
    ("Editor and Preview", "Редактор и предпросмотр"),
    ("Preview Only", "Только предпросмотр"),
    // GitHub alerts (`> [!NOTE]`); "Warning" is shared with the language servers' messages.
    ("Note", "Примечание"),
    ("Tip", "Совет"),
    ("Important", "Важно"),
    ("Caution", "Осторожно"),
    // The command palette (action names); the preview's Scroll Page Up / Down, To Top / Bottom are
    // shared with the terminal.
    ("Markdown", "Markdown"),
    ("Editor And Preview", "Редактор и предпросмотр"),
    ("Scroll Up", "Прокрутить вверх"),
    ("Scroll Down", "Прокрутить вниз"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
