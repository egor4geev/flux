//! Part 10.1: projects and windows — the project's window state, opening another project in this
//! window or a new one, reopening the projects on startup (`project_state.rs`, `workspace.rs`,
//! `main.rs`).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // "Open Project": where another project opens (as JetBrains IDEs ask).
    (
        "Where would you like to open the project “{0}”?",
        "Где открыть проект «{0}»?",
    ),
    ("This Window", "В этом окне"),
    ("New Window", "В новом окне"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
