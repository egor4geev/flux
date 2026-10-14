//! Part 10.2: the image tab (`image_view.rs`) — pictures from files and from Claude's chat, and the
//! SVG's Text / Preview pair.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The toolbar.
    ("Fit to Window", "По размеру окна"),
    ("Actual Size", "Реальный размер"),
    ("Zoom In", "Увеличить"),
    ("Zoom Out", "Уменьшить"),
    ("{0} × {1} px", "{0} × {1} пикс."),
    ("{0} B", "{0} Б"),
    ("{0} KB", "{0} КБ"),
    ("{0} MB", "{0} МБ"),
    // An SVG file: its text or its picture.
    ("Text", "Текст"),
    ("Couldn't show the picture", "Не удалось показать картинку"),
    // The command palette (action names).
    ("Image View", "Картинка"),
    ("Fit To Window", "По размеру окна"),
    ("Show Text", "Показать текст"),
    ("Show Preview", "Показать предпросмотр"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
