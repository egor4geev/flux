//! FluxGram part 1 (plugin API 0.5): agent C — Claude's tools from plugins, the confirmation card.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // A plugin's card in Claude's chat (`claude_cards.rs`)
    ("waiting for you", "ждёт вас"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[
    // A group of a plugin's steps in Claude's chat: "FluxGram · 3 steps"
    ("{n} step", ["{n} шаг", "{n} шага", "{n} шагов"]),
];
