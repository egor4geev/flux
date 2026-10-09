//! The Conflicts dialog, the merge tool, continue / abort of an operation (part D of stage 6.2).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The merge tool.
    ("Result", "Результат"),
    ("Current", "Текущая"),
    ("Accept", "Принять"),
    ("Ignore", "Игнорировать"),
    ("Accept Left", "Принять левую сторону"),
    ("Accept Right", "Принять правую сторону"),
    ("Apply", "Применить"),
    ("Continue Merging", "Продолжить слияние"),
    ("Discard", "Отбросить"),
    ("Discard the merge result?", "Отбросить результат слияния?"),
    (
        "What you did in the merge tool is lost; the file stays conflicted.",
        "Сделанное в инструменте слияния пропадёт; конфликт в файле останется.",
    ),
    (
        "Save the result and mark the file resolved anyway?",
        "Всё равно сохранить результат и отметить файл как разрешённый?",
    ),
    ("Previous Conflict", "Предыдущий конфликт"),
    ("Next Conflict", "Следующий конфликт"),
    ("No conflicts", "Конфликтов нет"),
    ("All conflicts resolved", "Все конфликты разрешены"),
    ("Resolve Simple Conflicts", "Разрешить простые конфликты"),
    (
        "No simple conflicts: each one needs a decision",
        "Простых конфликтов нет: каждый требует решения",
    ),
    ("next conflict", "следующий конфликт"),
    ("accept", "принять"),
    ("The file has no conflicts", "В файле нет конфликтов"),
    (
        "The file is not in a Git repository",
        "Файл не в репозитории Git",
    ),
    (
        "One side has no such file",
        "На одной из сторон такого файла нет",
    ),
    (
        "A binary file: take one side whole",
        "Двоичный файл: примите одну из сторон целиком",
    ),
    (
        "Deleted on the left side, changed on the right one",
        "Слева файл удалён, справа — изменён",
    ),
    (
        "Changed on the left side, deleted on the right one",
        "Слева файл изменён, справа — удалён",
    ),
    ("Deleted on both sides", "Удалён с обеих сторон"),
    (
        "Can't read the file's versions: {0}",
        "Не удалось прочитать версии файла: {0}",
    ),
    (
        "Couldn't resolve the conflict",
        "Не удалось разрешить конфликт",
    ),
    // The palette: the merge tool's section and commands.
    ("Merge Tool", "Инструмент слияния"),
    ("Accept Left Change", "Принять изменение слева"),
    ("Accept Right Change", "Принять изменение справа"),
    ("Apply Result", "Применить результат"),
    // The Conflicts dialog.
    ("Conflicts", "Конфликты"),
    ("Name", "Имя"),
    ("Modified", "Изменён"),
    ("Added", "Добавлен"),
    ("Accept Yours", "Принять ваши"),
    ("Accept Theirs", "Принять их"),
    ("Accept Upstream", "Принять upstream"),
    ("Accept Current", "Принять текущую"),
    ("Accept Stash", "Принять stash"),
    ("Merge…", "Слить…"),
    ("several", "несколько"),
    ("merge…", "слить…"),
    ("There are no conflicts", "Конфликтов нет"),
    ("All conflicts are resolved", "Все конфликты разрешены"),
    (
        "Commit to finish the merge",
        "Закоммитьте, чтобы завершить слияние",
    ),
    (
        "Continue the rebase with the next commit",
        "Продолжите rebase со следующего коммита",
    ),
    (
        "Continue to finish the operation",
        "Продолжите, чтобы завершить операцию",
    ),
    (
        "The stash is kept: drop it in the Stash tab when you no longer need it",
        "Stash сохранён: удалите его на вкладке Stash, когда он станет не нужен",
    ),
    ("Show Stashes", "Показать stash"),
    ("Merging {0} into {1}", "Слияние {0} в {1}"),
    ("Rebasing {0} onto {1}", "Rebase {0} на {1}"),
    ("Cherry-picking {0}", "Cherry-pick {0}"),
    ("Reverting {0}", "Revert {0}"),
    ("Unstash conflicts", "Конфликты после возврата из stash"),
    // Continue, skip, abort.
    ("Continue", "Продолжить"),
    ("Continue Rebase", "Продолжить rebase"),
    ("Abort", "Прервать"),
    ("Abort…", "Прервать…"),
    ("Skip", "Пропустить"),
    ("Skip Commit", "Пропустить коммит"),
    ("Resolve…", "Разрешить…"),
    ("No operation in progress", "Нет операции в процессе"),
    ("Resolve the conflicts first", "Сначала разрешите конфликты"),
    (
        "Only a rebase can skip a commit",
        "Пропустить коммит можно только в rebase",
    ),
    ("Couldn't continue", "Не удалось продолжить"),
    ("Couldn't abort", "Не удалось прервать"),
    ("Couldn't skip the commit", "Не удалось пропустить коммит"),
    (
        "The rebase stopped on conflicts",
        "Rebase остановился на конфликтах",
    ),
    (
        "The cherry-pick stopped on conflicts",
        "Cherry-pick остановился на конфликтах",
    ),
    (
        "The revert stopped on conflicts",
        "Revert остановился на конфликтах",
    ),
    ("The rebase paused", "Rebase приостановлен"),
    ("The operation paused", "Операция приостановлена"),
    ("Rebase finished", "Rebase завершён"),
    ("{0} is on top of {1}", "{0} теперь поверх {1}"),
    ("Cherry-pick finished", "Cherry-pick завершён"),
    ("Revert finished", "Revert завершён"),
    ("Done", "Готово"),
    ("Abort the merge?", "Прервать слияние?"),
    ("Abort the rebase?", "Прервать rebase?"),
    ("Abort the cherry-pick?", "Прервать cherry-pick?"),
    ("Abort the revert?", "Прервать revert?"),
    (
        "The branch and the working tree go back to where they were before the merge; local changes stashed for it come back.",
        "Ветка и рабочая копия вернутся к состоянию до слияния; локальные изменения, убранные для него в stash, вернутся.",
    ),
    (
        "The branch goes back to where it was before the rebase; local changes stashed for it come back.",
        "Ветка вернётся к состоянию до rebase; локальные изменения, убранные для него в stash, вернутся.",
    ),
    (
        "The branch and the working tree go back to where they were before it.",
        "Ветка и рабочая копия вернутся к прежнему состоянию.",
    ),
    ("Merge aborted", "Слияние прервано"),
    ("Rebase aborted", "Rebase прерван"),
    ("Cherry-pick aborted", "Cherry-pick прерван"),
    ("Revert aborted", "Revert прерван"),
    ("Skip the commit {0}?", "Пропустить коммит {0}?"),
    (
        "Its changes are left out of the rebased branch.",
        "Его изменений не будет в ветке после rebase.",
    ),
    // The palette: the dialog's section and commands.
    ("Conflicts Dialog", "Диалог конфликтов"),
    ("Extend Next", "Расширить выбор вниз"),
    ("Extend Previous", "Расширить выбор вверх"),
    ("Merge Selected", "Слить выбранный"),
    ("Accept Left Column", "Принять левый столбец"),
    ("Accept Right Column", "Принять правый столбец"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[
    (
        "{n} conflict is unresolved",
        [
            "{n} конфликт не разрешён",
            "{n} конфликта не разрешены",
            "{n} конфликтов не разрешено",
        ],
    ),
    (
        "{n} conflict left",
        [
            "Остался {n} конфликт",
            "Осталось {n} конфликта",
            "Осталось {n} конфликтов",
        ],
    ),
    (
        "{n} conflict resolved",
        [
            "Разрешён {n} конфликт",
            "Разрешено {n} конфликта",
            "Разрешено {n} конфликтов",
        ],
    ),
];
