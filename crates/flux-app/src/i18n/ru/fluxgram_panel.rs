//! FluxGram part 4 (plugin API 0.5): agent E — the field's panel: emoji (categories, the search,
//! skin tones), stickers, GIFs.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The ☺ button by the field, the panel's tabs and its search.
    ("Emoji, Stickers and GIFs", "Эмодзи, стикеры и GIF"),
    ("Emoji", "Эмодзи"),
    ("Stickers", "Стикеры"),
    ("Search Emoji", "Поиск эмодзи"),
    ("Search Stickers", "Поиск стикеров"),
    ("Search GIFs", "Поиск GIF"),
    ("Nothing here yet", "Здесь пока пусто"),
    // The emoji's categories (Unicode's groups).
    ("Smileys & People", "Смайлики и люди"),
    ("Animals & Nature", "Животные и природа"),
    ("Food & Drink", "Еда и напитки"),
    ("Activity", "Занятия"),
    ("Travel & Places", "Путешествия и места"),
    ("Objects", "Предметы"),
    ("Symbols", "Символы"),
    ("Flags", "Флаги"),
    // The skin tones' circles.
    ("No Skin Tone", "Без оттенка кожи"),
    ("Light Skin Tone", "Светлый тон кожи"),
    ("Medium-Light Skin Tone", "Умеренно светлый тон кожи"),
    ("Medium Skin Tone", "Средний тон кожи"),
    ("Medium-Dark Skin Tone", "Умеренно тёмный тон кожи"),
    ("Dark Skin Tone", "Тёмный тон кожи"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
