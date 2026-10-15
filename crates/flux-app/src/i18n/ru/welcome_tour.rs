//! The welcome guide's tour (part 10.7): five slides — their headings, keys and live tasks — and
//! the pictures of the search, the code and the tool windows.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The slides' frame and the live task.
    ("Tour · {0} of {1}", "Знакомство · {0} из {1}"),
    ("Try it yourself: press", "Попробуйте сами: нажмите"),
    ("You got it.", "Получилось."),
    // Everything from the keyboard.
    ("Everything from the Keyboard", "Всё — с клавиатуры"),
    (
        "A file, a line in the project, a command, a theme — all found from the keyboard. Every action that has keys shows them.",
        "Файл, строка в проекте, команда, тема — всё ищется с клавиатуры. У каждого действия с сочетанием оно на виду.",
    ),
    ("Theme, Look and Language", "Тема, вид и язык"),
    (
        "{0} opens the file search in any Flux window.",
        "{0} открывает поиск файла в любом окне Flux.",
    ),
    // The code understood.
    ("Flux Understands Your Code", "Flux понимает ваш код"),
    (
        "Flux installs and runs the language servers itself: problems as you type, completion, going to a definition, renaming across the project.",
        "Языковые серверы Flux ставит и запускает сам: ошибки на лету, подсказки, переход к определению, переименование по всему проекту.",
    ),
    ("Fixes and Actions", "Исправления и действия"),
    (
        "{0} shows the fixes and actions at the caret.",
        "{0} показывает исправления и действия у каретки.",
    ),
    // Version control.
    ("Version Control in Windows", "Git — в окнах Flux"),
    (
        "Committing in parts, branches, the log with its graph, stash, rebase, conflicts and annotations — in Flux's windows, without a terminal.",
        "Коммит по частям, ветки, лог с графом, stash, rebase, конфликты и аннотации — в окнах Flux, без терминала.",
    ),
    (
        "{0} opens the commit window with the focus in the message.",
        "{0} открывает окно коммита с фокусом в сообщении.",
    ),
    // Claude Code.
    ("Claude Code Beside Your Code", "Claude Code рядом с кодом"),
    (
        "Sessions in tabs, edits as cards with their diff, and at the end of a turn — a summary with review and rollback. Subagents are seen live.",
        "Сессии — вкладками, правки — карточками с диффом, в конце хода — итог с обзором и откатом. Субагенты видны вживую.",
    ),
    ("Open the Chat", "Открыть чат"),
    ("Selection to the Chat", "Выделенное — в чат"),
    (
        "{0} opens the chat, and takes you back to the editor.",
        "{0} открывает чат и возвращает в редактор.",
    ),
    // The tool windows.
    (
        "On the left is the strip of tool windows: the project, the commit, search, the terminal, version control. Each opens and hides with its keys; {0} hides the one you're in.",
        "Слева — полоса окон инструментов: проект, коммит, поиск, терминал, Git. Каждое открывается и прячется своим сочетанием, {0} прячет то, в котором вы.",
    ),
    (
        "On the left is the strip of tool windows: the project, the commit, search, the terminal, version control. Each opens and hides with its keys.",
        "Слева — полоса окон инструментов: проект, коммит, поиск, терминал, Git. Каждое открывается и прячется своим сочетанием.",
    ),
    (
        "{0} opens and hides the project tree.",
        "{0} открывает и прячет дерево проекта.",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
