//! FluxGram part 4 (plugin API 0.5): agent U — voice messages, videos, animated stickers, the
//! media viewer, what plays (the bar over the feed, the status bar).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // A voice message, the bar over the feed while it plays, the status bar's line.
    ("Voice message", "Голосовое сообщение"),
    ("You", "Вы"),
    ("Play", "Воспроизвести"),
    ("Pause", "Пауза"),
    // The decimal point of a speed: "1.5×".
    (".", ","),
    // A part that can't be played.
    ("Can't be played here", "Здесь не воспроизводится"),
    ("Can't be played", "Не воспроизводится"),
    ("No sound device", "Нет устройства звука"),
    // A video's pill.
    ("GIF", "GIF"),
    // The media viewer's player.
    ("Video", "Видео"),
    (
        "This video can't be played here",
        "Это видео здесь не воспроизводится",
    ),
    ("Volume", "Громкость"),
    ("Speed", "Скорость"),
    ("Full Screen", "Во весь экран"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
