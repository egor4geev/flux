//! Part 8.2: the plugin API 0.2 calls the window does (Git, problems, tabs), command contexts (agent B).

pub(super) const STRINGS: &[(&str, &str)] = &[
    (
        "The document has unsaved changes: they are the user's to decide about",
        "В документе есть несохранённые изменения: решать о них пользователю",
    ),
    (
        "The document has no tab of its own: it is shown in a diff",
        "У документа нет своей вкладки: он открыт в диффе",
    ),
    (
        "{0} isn't in a Git repository of the project",
        "{0} не в Git-репозитории проекта",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
