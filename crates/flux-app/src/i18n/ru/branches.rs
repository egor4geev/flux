//! The branches popup and the branch dialogs: checkout, new, rename, delete, merge, rebase (part B of stage 6.2).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The popup: sections, actions, rows.
    ("Recent", "Недавние"),
    ("Local", "Локальные"),
    ("Remote", "Удалённые"),
    ("Tags", "Теги"),
    ("Update Project…", "Обновить проект…"),
    ("New Branch…", "Новая ветка…"),
    ("Checkout Tag or Revision…", "Перейти на тег или ревизию…"),
    ("New Branch '{0}'…", "Новая ветка «{0}»…"),
    ("Nothing matches", "Ничего не найдено"),
    ("Search for branches and actions", "Поиск веток и действий"),
    ("Remove from Favorites", "Убрать из избранного"),
    ("Add to Favorites", "В избранное"),
    ("actions", "действия"),
    ("favorite", "избранное"),
    ("back", "назад"),
    // A branch's submenu.
    ("Checkout", "Перейти"),
    ("New Branch from '{0}'…", "Новая ветка от «{0}»…"),
    (
        "Checkout and Rebase onto '{0}'",
        "Перейти и сделать rebase на «{0}»",
    ),
    ("Compare with '{0}'", "Сравнить с «{0}»"),
    ("Show Diff with Working Tree", "Сравнить с рабочей копией"),
    ("Rebase '{0}' onto '{1}'", "Rebase «{0}» на «{1}»"),
    ("Merge '{0}' into '{1}'", "Слить «{0}» в «{1}»"),
    ("Pull into '{0}' Using Rebase", "Pull в «{0}» через rebase"),
    ("Pull into '{0}' Using Merge", "Pull в «{0}» через слияние"),
    ("Rename…", "Переименовать…"),
    // Checkout.
    ("Already on '{0}'", "Вы уже на «{0}»"),
    ("Checkout failed", "Не удалось перейти"),
    (
        "Your local changes would be overwritten by checkout of '{0}'",
        "Переход на «{0}» перезапишет ваши локальные изменения",
    ),
    (
        "Smart Checkout stashes them, checks out and brings them back. Force Checkout throws them away.",
        "«Перейти с изменениями» уберёт их в stash, перейдёт и вернёт их обратно. «Перейти без изменений» их отбросит.",
    ),
    ("Smart Checkout", "Перейти с изменениями"),
    ("Force Checkout", "Перейти без изменений"),
    ("Don’t Checkout", "Не переходить"),
    (
        "Untracked files would be overwritten",
        "Неотслеживаемые файлы были бы перезаписаны",
    ),
    (
        "Move or delete them, then try again.",
        "Переместите или удалите их и попробуйте снова.",
    ),
    (
        "Local changes restored with conflicts",
        "Локальные изменения вернулись с конфликтами",
    ),
    (
        "The stash is kept: drop it once the conflicts are resolved.",
        "Stash сохранён: удалите его, когда разрешите конфликты.",
    ),
    ("Resolve…", "Разрешить…"),
    ("Drop Stash", "Удалить stash"),
    ("HEAD is detached at {0}", "HEAD отсоединён на {0}"),
    (
        "Commits made here belong to no branch: create one to keep them.",
        "Коммиты здесь не попадут ни в одну ветку — создайте ветку, чтобы сохранить их.",
    ),
    ("Checked out '{0}'", "Вы на «{0}»"),
    (
        "Checked out '{0}' and updated it from '{1}'",
        "Вы на «{0}», ветка обновлена из «{1}»",
    ),
    (
        "Local changes were stashed and restored",
        "Локальные изменения убраны в stash и возвращены",
    ),
    // Merge and rebase.
    ("Merge failed", "Слияние не удалось"),
    ("Rebase failed", "Rebase не удался"),
    ("Update failed", "Обновление не удалось"),
    (
        "Merge of '{0}' into '{1}' stopped on conflicts",
        "Слияние «{0}» в «{1}» остановилось на конфликтах",
    ),
    (
        "Rebase of '{0}' onto '{1}' stopped on conflicts",
        "Rebase «{0}» на «{1}» остановился на конфликтах",
    ),
    (
        "Resolve the conflicts, then commit the merge.",
        "Разрешите конфликты и закоммитьте слияние.",
    ),
    (
        "Resolve the conflicts, then continue the rebase.",
        "Разрешите конфликты и продолжите rebase.",
    ),
    ("Abort Merge", "Прервать слияние"),
    ("Abort Rebase", "Прервать rebase"),
    ("Already up to date", "Уже актуально"),
    (
        "'{0}' already has everything from '{1}'",
        "В «{0}» уже есть всё из «{1}»",
    ),
    (
        "Fast-forwarded '{0}' to '{1}'",
        "«{0}» перемотана вперёд до «{1}»",
    ),
    ("Merged '{0}' into '{1}'", "«{0}» слита в «{1}»"),
    (
        "'{0}' is already based on '{1}'",
        "«{0}» уже основана на «{1}»",
    ),
    ("Rebased '{0}' onto '{1}'", "Rebase «{0}» на «{1}» выполнен"),
    // New Branch.
    ("Create New Branch", "Новая ветка"),
    ("from '{0}'", "от «{0}»"),
    ("Branch name", "Имя ветки"),
    ("Checkout branch", "Перейти на неё"),
    (
        "Overwrite existing branch",
        "Перезаписать существующую ветку",
    ),
    ("Create", "Создать"),
    ("'{0}' is the current branch", "«{0}» — текущая ветка"),
    ("Branch '{0}' already exists", "Ветка «{0}» уже есть"),
    (
        "Checked out new branch '{0}' from '{1}'",
        "Новая ветка «{0}» от «{1}», вы на ней",
    ),
    (
        "Created branch '{0}' from '{1}'",
        "Создана ветка «{0}» от «{1}»",
    ),
    ("Branch not created", "Ветка не создана"),
    // Git's rules for a branch name (`flux_git::check_branch_name`).
    ("Enter a branch name", "Введите имя ветки"),
    ("This name is reserved", "Это имя зарезервировано"),
    (
        "A branch name can't start with “-”",
        "Имя ветки не может начинаться с «-»",
    ),
    (
        "Slashes can't start or end a name or come twice",
        "Косая черта не может стоять в начале или конце имени или дважды подряд",
    ),
    (
        "A branch name can't contain “..”, “@{” or end with “.”",
        "Имя ветки не может содержать «..», «@{» или заканчиваться точкой",
    ),
    (
        "A branch name can't contain spaces or ~ ^ : ? * [ \\",
        "Имя ветки не может содержать пробелы и ~ ^ : ? * [ \\",
    ),
    (
        "A part of the name can't start with “.” or end with “.lock”",
        "Часть имени не может начинаться с точки или заканчиваться на «.lock»",
    ),
    // Rename.
    ("Rename '{0}'", "Переименовать «{0}»"),
    ("Enter a new name", "Введите новое имя"),
    ("Unset upstream branch", "Отвязать от ветки на сервере"),
    ("Renamed '{0}' to '{1}'", "«{0}» переименована в «{1}»"),
    ("Rename failed", "Переименовать не удалось"),
    // Checkout Tag or Revision.
    ("Checkout Tag or Revision", "Перейти на тег или ревизию"),
    (
        "A tag, a branch or a commit hash",
        "Тег, ветка или хеш коммита",
    ),
    ("Enter a revision", "Введите ревизию"),
    ("A revision has no spaces", "В ревизии не бывает пробелов"),
    ("Unknown revision '{0}'", "Нет ревизии «{0}»"),
    // Delete and restore.
    (
        "The current branch can't be deleted",
        "Текущую ветку удалить нельзя",
    ),
    (
        "Check out another branch first.",
        "Сначала перейдите на другую ветку.",
    ),
    ("Deleted branch '{0}'", "Ветка «{0}» удалена"),
    ("Restore", "Восстановить"),
    ("Delete Tracked Branch", "Удалить ветку на сервере"),
    ("Branch not deleted", "Ветка не удалена"),
    (
        "Branch '{0}' is not fully merged into '{1}'",
        "Ветка «{0}» не полностью слита в «{1}»",
    ),
    (
        "Deleting it loses these commits (Restore brings the branch back).",
        "Удаление потеряет эти коммиты («Восстановить» вернёт ветку).",
    ),
    (
        "Delete remote branch '{0}'?",
        "Удалить ветку «{0}» на сервере?",
    ),
    (
        "The branch is deleted on the server, for everyone who works with it.",
        "Ветка удалится на сервере — для всех, кто с ней работает.",
    ),
    (
        "Deleted remote branch '{0}'",
        "Ветка «{0}» удалена на сервере",
    ),
    ("Remote branch not deleted", "Ветка на сервере не удалена"),
    ("Deleted tag '{0}'", "Тег «{0}» удалён"),
    ("Tag not deleted", "Тег не удалён"),
    ("Restored branch '{0}'", "Ветка «{0}» восстановлена"),
    (
        "Restored remote branch '{0}'",
        "Ветка «{0}» восстановлена на сервере",
    ),
    ("Restored tag '{0}'", "Тег «{0}» восстановлен"),
    ("Restore failed", "Восстановить не удалось"),
    (
        "Restoring the remote branch…",
        "Восстановление ветки на сервере…",
    ),
    // The ⌃V menu.
    ("Pull…", "Pull…"),
    ("Branches…", "Ветки…"),
    ("Stash Changes…", "Stash изменений…"),
    ("Unstash Changes…", "Вернуть из stash…"),
    ("Resolve Conflicts…", "Разрешить конфликты…"),
    ("Continue Rebase", "Продолжить rebase"),
    ("Continue Cherry-Pick", "Продолжить cherry-pick"),
    ("Abort Cherry-Pick", "Прервать cherry-pick"),
    ("Continue Revert", "Продолжить revert"),
    ("Abort Revert", "Прервать revert"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
