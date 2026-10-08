//! Search in terminal output.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The terminal searches upward, from the newest output: the buttons say where they go.
    ("Match Above", "Совпадение выше"),
    ("Match Below", "Совпадение ниже"),
];
