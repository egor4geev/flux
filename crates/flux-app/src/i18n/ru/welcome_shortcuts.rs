//! The welcome guide's keys to remember (Help → Keyboard Shortcuts) and the pictures of version
//! control and Claude Code (part 10.7). The commands are named as the palette names them: those
//! names are in `commands.rs`, `common.rs` and the tables of their areas.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The keys slide.
    ("Tour · Keys", "Знакомство · клавиши"),
    (
        "Keys Worth Remembering",
        "Сочетания, которые стоит запомнить",
    ),
    (
        "Everything else is in the command palette ({0}): every command there shows its keys. This page opens again from Help → Keyboard Shortcuts.",
        "Всё остальное — в палитре команд ({0}): у каждой команды там видно её сочетание. Снова открыть эту страницу — Справка → Сочетания клавиш.",
    ),
    // The groups and the one command named for the slide.
    ("Code", "Код"),
    ("Windows", "Окна"),
    ("Context Actions", "Контекстные действия"),
    // The palette in the picture: what is typed in it.
    ("comm", "комм"),
    // The version control picture: the commit made, the log's older commits.
    (
        "Short #rgb form in from_hex",
        "Короткая форма #rgb в from_hex",
    ),
    ("Theme: warmer colors", "Тема: тёплые цвета"),
    ("Palette: shades", "Палитра: оттенки"),
    ("Tests for from_hex", "Тесты from_hex"),
    ("First commit", "Первый коммит"),
    // The Claude Code picture: the session and the message.
    ("Short #rgb in from_hex", "Короткий #rgb в from_hex"),
    (
        "Make from_hex accept the short #rgb form too",
        "Пусть from_hex понимает и короткую форму #rgb",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
