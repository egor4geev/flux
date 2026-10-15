//! Part 10.4: the interface scale (`ui_scale.rs`) — Zoom In / Zoom Out / Reset Zoom.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The scale: "110%" in English, "110 %" in Russian.
    ("{0}%", "{0} %"),
    // The status bar after ⌃⌥= / ⌃⌥- / ⌃⌥0.
    ("Zoom: {0}", "Масштаб: {0}"),
    // The command palette: the section of `interface::*` actions and the title of Reset Zoom («Zoom
    // In», «Zoom Out» are the image view's).
    ("Interface", "Интерфейс"),
    ("Reset Zoom", "Сбросить масштаб"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
