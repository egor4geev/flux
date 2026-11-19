//! FluxGram part 1 (plugin API 0.5): agent A — the service, helpers, the 0.3 compatibility, the install question's new lines.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The install question and the Permissions tab (`plugin_manager::manifest_lines`).
    (
        "Download and run its own program {0}",
        "Скачивать и запускать свою программу {0}",
    ),
    (
        "Download and run its own program {0} ({1})",
        "Скачивать и запускать свою программу {0} ({1})",
    ),
    (
        "Keep working in the background while Flux is open",
        "Работать в фоне, пока Flux открыт",
    ),
    ("Give Claude tools: {0}", "Дать Claude инструменты: {0}"),
    (
        "Give Claude tools that ask you before acting: {0}",
        "Дать Claude инструменты, которые спрашивают вас перед действием: {0}",
    ),
    // A helper's download (`plugin_services::helper_progress`).
    ("Downloading {0}…", "Скачивается {0}…"),
    ("Downloading {0}… {1} %", "Скачивается {0}… {1} %"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
