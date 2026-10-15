//! Part 10.4: Settings → Appearance → Interface — the look of the window (Islands, Flat) and the scale.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // Settings → Appearance → Interface.
    ("Interface", "Интерфейс"),
    (
        "The look of the window and its scale. Both change at once, in every window.",
        "Вид окна и его масштаб. Меняются сразу, во всех окнах.",
    ),
    ("Style", "Стиль"),
    ("Islands", "Острова"),
    (
        "Rounded panels on the window’s glass",
        "Скруглённые панели на стекле окна",
    ),
    ("Flat", "Плоский"),
    (
        "Panels edge to edge, divided by lines",
        "Панели вплотную, разделены линиями",
    ),
    ("Scale", "Масштаб"),
    (
        "Everything in the window — panels, icons, the code — larger or smaller.",
        "Всё в окне — панели, значки, код — крупнее или мельче.",
    ),
    // Quick Switch (⌃`), and its command in the palette (`quick_switch::SelectInterfaceStyle`).
    ("Interface Style", "Стиль интерфейса"),
    ("Select Interface Style", "Выбрать стиль интерфейса"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
