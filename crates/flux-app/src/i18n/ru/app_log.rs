//! Flux's log and crash reports (part 10.6): Show Log in Finder, the notice after a crash.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The notice of a crash (its actions — "Report a Bug…" of the Help menu, "Show in Finder").
    ("Flux quit unexpectedly", "Flux неожиданно завершил работу"),
    ("Flux ran into an error", "Во Flux произошла ошибка"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
