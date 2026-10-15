//! Part 10.4: the project popup in the title bar (`project_popup.rs`) — recent projects, Open….

pub(super) const STRINGS: &[(&str, &str)] = &[
    // A project open in another Flux window: choosing it brings that window forward.
    ("in another window", "в другом окне"),
    ("Remove from Recent Projects", "Убрать из недавних проектов"),
    // The footer's keys.
    ("new window", "в новом окне"),
    ("remove", "убрать"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
