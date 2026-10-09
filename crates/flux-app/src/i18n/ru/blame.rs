//! Annotations and the commit graph (part E of stage 6.3).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // Annotations in the gutter.
    ("Annotate with Git Blame", "Аннотировать с Git Blame"),
    ("Close Annotations", "Закрыть аннотации"),
    ("Annotating…", "Аннотирование…"),
    (
        "No annotations: the file isn’t committed yet",
        "Нет аннотаций: файл ещё не закоммичен",
    ),
    (
        "Open a file to annotate it",
        "Откройте файл, чтобы аннотировать его",
    ),
    (
        "The file isn’t in a Git repository",
        "Файл не в репозитории Git",
    ),
    (
        "The file isn’t saved: nothing to annotate",
        "Файл не сохранён: аннотировать нечего",
    ),
    ("Show in Git Log", "Показать в логе Git"),
    ("Revision number copied", "Хеш ревизии скопирован"),
    ("In {0}", "В {0}"),
    // History.
    ("Show History", "Показать историю"),
    ("Show History for Selection", "Показать историю выделенного"),
    (
        "Open a file to see its history",
        "Откройте файл, чтобы увидеть его историю",
    ),
    (
        "The selected lines aren’t in the last commit",
        "Выделенных строк нет в последнем коммите",
    ),
    // Palette titles.
    ("Annotate", "Аннотировать"),
    ("Show File History", "Показать историю файла"),
    ("Show Selection History", "Показать историю выделенного"),
    (
        "Copy Annotation Revision",
        "Копировать хеш ревизии аннотации",
    ),
    ("Show Annotation Diff", "Показать дифф аннотации"),
    ("Show Annotation In Log", "Показать аннотацию в логе"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
