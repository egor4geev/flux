//! The welcome guide (part 10.7): its window, the chapters and the buttons every slide shares, the
//! Help menu's items, the greeting.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The window and its bar.
    ("Welcome to Flux", "Добро пожаловать во Flux"),
    ("What's New in Flux", "Что нового во Flux"),
    ("Setup", "Настройка"),
    ("Tour", "Знакомство"),
    ("Get Started", "Начать"),
    ("Skip Setup", "Пропустить настройку"),
    ("Skip Tour", "Пропустить знакомство"),
    // Closed before "All set" on the first launch: where the guide lives.
    ("The guide is always at hand", "Гайд всегда под рукой"),
    ("Help → Welcome Guide", "Справка → Гайд по Flux"),
    ("Open Guide", "Открыть гайд"),
    // The greeting.
    ("Everything from the keyboard", "Всё — с клавиатуры"),
    ("Version control in windows", "Git — в окнах"),
    ("Claude Code inside", "Claude Code внутри"),
    (
        "A fast native code editor for macOS — with code understanding, version control and Claude Code built in.",
        "Быстрый нативный редактор кода для macOS — с пониманием кода, Git и Claude Code.",
    ),
    (
        "A fast native code editor for Windows — with code understanding, version control and Claude Code built in.",
        "Быстрый нативный редактор кода для Windows — с пониманием кода, Git и Claude Code.",
    ),
    ("Interface language", "Язык интерфейса"),
    // Help.
    ("Welcome Guide", "Гайд по Flux"),
    ("Keyboard Shortcuts", "Сочетания клавиш"),
    ("What's New", "Что нового"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
