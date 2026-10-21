//! Part «Пульт Claude»: the console's core: sessions, archive, the window (fork A).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // Archiving a session that works
    ("Archive “{0}”?", "Отправить «{0}» в архив?"),
    (
        "Claude is working in this session. Archiving it stops Claude. Its changes stay.",
        "Claude работает в этой сессии. Архивация остановит Claude. Его изменения останутся.",
    ),
    ("Stop and Archive", "Остановить и в архив"),
    // Closing a session that works (the console's ⌘⌫)
    (
        "Claude is working in this session. Closing it stops Claude. Its changes stay.",
        "Claude работает в этой сессии. Закрытие остановит Claude. Его изменения останутся.",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
