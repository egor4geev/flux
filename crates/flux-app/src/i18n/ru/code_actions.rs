//! Part 9.2: ⌥↵ — the context actions of the language servers (`code_actions.rs`), Flux's tools for Claude (`claude_tools.rs`).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The ⌥↵ popup.
    ("Fix with Claude", "Исправить с помощью Claude"),
    ("Explain with Claude", "Объяснить с помощью Claude"),
    ("Not available: {0}", "Недоступно: {0}"),
    ("No actions here", "Здесь нет действий"),
    ("Couldn't apply “{0}”: {1}", "Не удалось применить «{0}»: {1}"),
    ("“{0}” failed: {1}", "«{0}» не выполнено: {1}"),
    ("Couldn't apply the edit", "Не удалось применить правку"),
    // The command palette.
    ("Show Context Actions", "Показать контекстные действия"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
