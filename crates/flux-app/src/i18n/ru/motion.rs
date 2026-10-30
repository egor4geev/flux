//! Motion (2026-10-28): Settings → Appearance → Interface → Animations.

pub(super) const STRINGS: &[(&str, &str)] = &[
    ("Motion", "Движение"),
    ("Animations", "Анимации"),
    (
        "Popovers grow out of where they were called, panels glide, buttons spring back. Off — everything simply appears.",
        "Попапы вырастают из места вызова, панели скользят, кнопки пружинят. Выключено — всё просто появляется.",
    ),
    (
        "Reduce motion is on in macOS’s Accessibility settings: everything simply appears.",
        "В универсальном доступе macOS включено «Уменьшение движения»: всё просто появляется.",
    ),
    (
        "Animation effects are off in the Accessibility settings of Windows: everything simply appears.",
        "В специальных возможностях Windows выключены «Эффекты анимации»: всё просто появляется.",
    ),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
