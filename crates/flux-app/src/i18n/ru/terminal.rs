//! The terminal: its text area, keyboard, clipboard, and the commands of a terminal pane.

pub(super) const STRINGS: &[(&str, &str)] = &[
    ("Terminal", "Терминал"),
    ("Clear", "Очистить"),
    ("Split Right", "Разделить вправо"),
    ("Split Down", "Разделить вниз"),
    (
        "The shell exited with code {0}",
        "Оболочка завершилась с кодом {0}",
    ),
    ("The shell was terminated", "Оболочка прервана"),
];
