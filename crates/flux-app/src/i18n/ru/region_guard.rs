//! Part «Флаг страны»: the country check itself (the coordinator).

pub(super) const STRINGS: &[(&str, &str)] = &[
    ("Claude is blocked", "Claude заблокирован"),
    ("The IP address is in {0}.", "IP-адрес — {0}."),
    (
        "Flux couldn't find out the country of the IP address.",
        "Flux не смог определить страну IP-адреса.",
    ),
    (
        "Claude is blocked: the IP address is in {0}.",
        "Claude заблокирован: IP-адрес — {0}.",
    ),
    (
        "Claude is blocked: Flux couldn't find out the country of the IP address.",
        "Claude заблокирован: Flux не смог определить страну IP-адреса.",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
