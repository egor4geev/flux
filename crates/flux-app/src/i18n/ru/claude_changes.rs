//! Part 9.2: Claude Code — the "Claude" changelist of the commit window, the diff against the text before Claude, the commit message Claude writes (`commit_panel.rs`, `diff_view.rs`).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The changelists of the commit window.
    ("Show Diff with HEAD", "Показать дифф с HEAD"),
    ("Rollback to Before Claude…", "Откатить к состоянию до Claude…"),
    ("Rollback All Claude Changes…", "Откатить все изменения Claude…"),
    ("Move to Changes", "Перенести в Changes"),
    ("Show Session", "Показать сессию"),
    ("Roll back Claude's changes in “{0}”?", "Откатить изменения Claude в «{0}»?"),
    ("Roll back Claude's changes {0}?", "Откатить изменения Claude {0}?"),
    (
        "The files get back their text from before Claude changed them.",
        "Файлам вернётся текст, который был до изменений Claude.",
    ),
    (
        "Files Claude created are moved to the Trash.",
        "Файлы, созданные Claude, отправятся в Корзину.",
    ),
    ("Rolled back Claude's changes: {0}", "Изменения Claude откачены: {0}"),
    ("Rolled back Claude's changes in {0}", "Изменения Claude откачены: {0}"),
    ("Couldn't roll back Claude's changes", "Не удалось откатить изменения Claude"),
    // The diff against the text before Claude.
    ("Before Claude", "До Claude"),
    ("{0}: the file didn't exist", "{0}: файла не было"),
    // The commit message Claude writes.
    ("Generate Commit Message with Claude", "Написать сообщение коммита с помощью Claude"),
    ("Stop Generating", "Остановить"),
    ("Claude Code isn't ready", "Claude Code не готов"),
    ("Claude's answer was empty", "Claude ответил пустым сообщением"),
    ("Claude couldn't write the commit message", "Claude не смог написать сообщение коммита"),
    // The command palette (action names).
    ("Show Diff With Head", "Показать дифф с HEAD"),
    ("Move To Changes", "Перенести в Changes"),
    ("Generate Commit Message", "Написать сообщение коммита"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
