//! Part 8.2: plugins' terminals and proposed edits (agent D).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // A terminal a plugin ran a command in: how the command ended, as JetBrains' Run window.
    (
        "Process finished with exit code {0}",
        "Процесс завершился с кодом {0}",
    ),
    ("Process terminated", "Процесс прерван"),
    // A plugin's proposed edit: the banner and the caption of the right side.
    ("{0} proposes changes to {1}", "{0} предлагает изменить {1}"),
    ("Proposed by {0}", "Предлагает {0}"),
    ("Couldn't write {0}", "Не удалось записать {0}"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
