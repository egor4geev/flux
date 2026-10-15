//! Part 10.4: the command palette's frequent and recent commands (`command_usage.rs`).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The groups of the palette with an empty query.
    ("Recently Used", "Недавно использованные"),
    ("Other Commands", "Остальные команды"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
