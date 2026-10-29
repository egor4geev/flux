//! Part «Флаг страны»: the chat — the field, the notes, the commit message (fork «chat»).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The field.
    ("Checking the country…", "Проверка страны…"),
    (
        "The text stays here until Claude is unblocked",
        "Текст останется здесь до разблокировки Claude",
    ),
    (
        "Claude is blocked by the country check",
        "Claude заблокирован проверкой страны",
    ),
    // The notes of the conversation.
    (
        "Claude was blocked: the IP address was in {0}.",
        "Claude был заблокирован: IP-адрес — {0}.",
    ),
    (
        "Claude was blocked: Flux couldn't find out the country of the IP address.",
        "Claude был заблокирован: Flux не смог определить страну IP-адреса.",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
