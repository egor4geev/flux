//! Fetch, Pull, Update Project, push of a chosen branch, comparing branches, the Version Control
//! settings (part E of stage 6.2).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // Update Project.
    ("Update", "Обновить"),
    (
        "How should the incoming commits join your branch?",
        "Как присоединить входящие коммиты к вашей ветке?",
    ),
    (
        "Merge incoming changes into the current branch",
        "Слить входящие изменения в текущую ветку",
    ),
    (
        "A merge commit joins them, unless your branch can simply move forward",
        "Их соединит коммит слияния, если ветку нельзя просто перемотать вперёд",
    ),
    (
        "Rebase the current branch on top of incoming changes",
        "Перебазировать текущую ветку поверх входящих изменений",
    ),
    (
        "Your local commits are replayed on top: the history stays a line",
        "Ваши коммиты переносятся поверх: история остаётся линией",
    ),
    ("Don't show again", "Больше не показывать"),
    (
        "You can change this later in Settings → Version Control",
        "Это можно изменить в Настройках → Контроль версий",
    ),
    ("Project updated", "Проект обновлён"),
    ("All files are up to date", "Все файлы актуальны"),
    ("Update failed", "Обновление не удалось"),
    ("Update of {0} failed", "Обновление {0} не удалось"),
    (
        "Update stopped on conflicts",
        "Обновление остановилось на конфликтах",
    ),
    ("stopped on conflicts", "остановлено на конфликтах"),
    ("up to date", "актуально"),
    (
        "no tracked branch for {0}",
        "нет отслеживаемой ветки для {0}",
    ),
    ("Nothing to update from", "Не из чего обновлять"),
    (
        "{0} — push the branch first, or set its upstream (git branch --set-upstream-to).",
        "{0} — сначала запушьте ветку или задайте ей upstream (git branch --set-upstream-to).",
    ),
    ("and {0} more", "и ещё {0}"),
    ("Resolve…", "Разрешить…"),
    ("Abort", "Прервать"),
    // Pull.
    ("Pull to {0}", "Pull в {0}"),
    ("Another repository", "Другой репозиторий"),
    ("Merge", "Слияние"),
    ("Rebase", "Rebase"),
    ("Fast-forward only", "Только перемотка вперёд"),
    (
        "No such branch on the remote yet — fetch first",
        "На сервере такой ветки пока нет — сделайте fetch",
    ),
    ("Pull failed", "Pull не удался"),
    (
        "Pull stopped on conflicts",
        "Pull остановился на конфликтах",
    ),
    (
        "Resolve the conflicts, then commit or continue",
        "Разрешите конфликты, затем закоммитьте или продолжите",
    ),
    ("Already up to date", "Уже актуально"),
    (
        "{0} has nothing {1} doesn't have",
        "В {0} нет ничего, чего нет в {1}",
    ),
    ("Pulled {0} into {1}", "{0} получена в {1}"),
    (
        "{0} isn't a branch of a remote",
        "{0} — не ветка удалённого репозитория",
    ),
    // Fetch.
    ("Fetched", "Fetch выполнен"),
    ("Fetch failed", "Fetch не удался"),
    ("Fetch of {0} failed", "Fetch {0} не удался"),
    (
        "Fetch: nothing new on the remotes",
        "Fetch: на серверах ничего нового",
    ),
    ("new branch {0}", "новая ветка {0}"),
    ("{0} is gone", "{0} удалена"),
    // Update of another branch.
    ("{0} is up to date", "{0} актуальна"),
    ("Updated {0}", "{0} обновлена"),
    ("Fast-forwarded to {0}", "Перемотана вперёд до {0}"),
    (
        "No tracked branch for {0}",
        "Нет отслеживаемой ветки для {0}",
    ),
    ("Can't update {0}", "Не удаётся обновить {0}"),
    (
        "It has commits of its own: check it out and update it (merge or rebase)",
        "В ней есть свои коммиты: перейдите на неё и обновите (слиянием или rebase)",
    ),
    // Push of a chosen branch, a rejected push.
    ("Push failed", "Пуш не удался"),
    ("Push of {0} was rejected", "Пуш {0} отклонён"),
    (
        "{0}/{1} has commits that {2} doesn't have (someone pushed first). Update {2} — rebase your commits on top of them or merge them in — and Flux pushes again.",
        "В {0}/{1} есть коммиты, которых нет в {2} (кто-то запушил раньше). Обновите {2} — перенесите свои коммиты поверх них (rebase) или слейте их — и Flux запушит снова.",
    ),
    ("Merge and Push", "Слияние и пуш"),
    ("Rebase and Push", "Rebase и пуш"),
    (
        "Resolve them, then push again",
        "Разрешите их и запушьте снова",
    ),
    // Comparing branches.
    ("Compare {0} with {1}", "Сравнение {0} с {1}"),
    (
        "Diff of {0} with the working tree",
        "Различия {0} с рабочей копией",
    ),
    ("Commits", "Коммиты"),
    ("Files", "Файлы"),
    ("In {0}, not in {1}", "Есть в {0}, нет в {1}"),
    ("commits / files", "коммиты / файлы"),
    ("← {0}", "← {0}"),
    // Settings → Version Control.
    ("Version Control", "Контроль версий"),
    (
        "Git: how Update Project (⌘T) brings the commits of the upstream into the current branch. Local changes are stashed for the update and come back after it.",
        "Git: как «Обновить проект» (⌘T) приносит коммиты upstream в текущую ветку. Локальные изменения на время обновления уходят в stash и возвращаются после.",
    ),
    (
        "Incoming commits are merged into the current branch",
        "Входящие коммиты сливаются в текущую ветку",
    ),
    (
        "Your local commits are replayed on top of the incoming ones",
        "Ваши коммиты переносятся поверх входящих",
    ),
    ("Ask every time", "Спрашивать каждый раз"),
    (
        "Update Project asks before it starts",
        "«Обновить проект» спрашивает перед началом",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[
    (
        "{n} file updated",
        [
            "Обновлён {n} файл",
            "Обновлено {n} файла",
            "Обновлено {n} файлов",
        ],
    ),
    (
        "in {n} commit",
        ["в {n} коммите", "в {n} коммитах", "в {n} коммитах"],
    ),
    (
        "{n} new commit",
        [
            "{n} новый коммит",
            "{n} новых коммита",
            "{n} новых коммитов",
        ],
    ),
];
