//! What's New after an update (part 10.7): the cards of a version, Try It, the release notes.

pub(super) const STRINGS: &[(&str, &str)] = &[
    ("What's New in Flux {0}", "Что нового во Flux {0}"),
    ("Update · {0}", "Обновление · {0}"),
    ("Try It", "Попробовать"),
    ("Release Notes", "Заметки к релизу"),
    ("Show after updates", "Показывать после обновлений"),
    (
        "There are no notes for this version yet.",
        "К этой версии заметок пока нет.",
    ),
    // The release's date: «15 October 2026» → «15 октября 2026».
    ("{0} January {1}", "{0} января {1}"),
    ("{0} February {1}", "{0} февраля {1}"),
    ("{0} March {1}", "{0} марта {1}"),
    ("{0} April {1}", "{0} апреля {1}"),
    ("{0} May {1}", "{0} мая {1}"),
    ("{0} June {1}", "{0} июня {1}"),
    ("{0} July {1}", "{0} июля {1}"),
    ("{0} August {1}", "{0} августа {1}"),
    ("{0} September {1}", "{0} сентября {1}"),
    ("{0} October {1}", "{0} октября {1}"),
    ("{0} November {1}", "{0} ноября {1}"),
    ("{0} December {1}", "{0} декабря {1}"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
