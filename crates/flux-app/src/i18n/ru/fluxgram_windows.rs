//! FluxGram part 1 (plugin API 0.5): agent D — the left island, tabs of a window, badges, the file dialog.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // A restored tab of a plugin's window with tabs whose plugin can't fill it.
    ("Plugin «{0}» isn't installed", "Плагин «{0}» не установлен"),
    ("Plugin «{0}» is off", "Плагин «{0}» выключен"),
    ("Plugin «{0}» can't run", "Плагин «{0}» не может работать"),
    (
        "Turn it on in Settings → Plugins",
        "Включите его в Настройках → Плагины",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
