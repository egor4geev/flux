//! FluxGram part 3 (plugin API 0.5): agent F2 — selecting and quoting a message's text, the
//! field's formatting, the viewer waiting for a picture's full size.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // A message's menu over a selection in it.
    ("Copy Selected Text", "Копировать выделенное"),
    // The field's menu: Formatting (as Telegram names its marks).
    ("Formatting", "Форматирование"),
    ("Bold", "Жирный"),
    ("Italic", "Курсив"),
    ("Strikethrough", "Зачёркнутый"),
    ("Spoiler", "Скрытый"),
    ("Monospace", "Моноширинный"),
    ("Link…", "Ссылка…"),
    // The dialog of ⌘K: the link's address.
    ("Link", "Ссылка"),
    ("Insert", "Вставить"),
    ("Enter an address", "Введите адрес"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
