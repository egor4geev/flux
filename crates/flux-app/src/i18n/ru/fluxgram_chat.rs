//! FluxGram part 1 (plugin API 0.5): agent B — the conversation, pictures, avatars, QR codes.

pub(super) const STRINGS: &[(&str, &str)] = &[
    // A message's menu.
    ("Copy Text", "Копировать текст"),
    // Selecting messages (part 5): the menu's item, the bar's count.
    ("Select", "Выбрать"),
    ("Selected: {0}", "Выбрано: {0}"),
    ("Save As…", "Сохранить как…"),
    // A message.
    ("Forwarded from {0}", "Переслано от {0}"),
    ("edited", "изменено"),
    ("New messages", "Новые сообщения"),
    // The days between messages: "17 November".
    ("Today", "Сегодня"),
    ("January {0}", "{0} января"),
    ("February {0}", "{0} февраля"),
    ("March {0}", "{0} марта"),
    ("April {0}", "{0} апреля"),
    ("May {0}", "{0} мая"),
    ("June {0}", "{0} июня"),
    ("July {0}", "{0} июля"),
    ("August {0}", "{0} августа"),
    ("September {0}", "{0} сентября"),
    ("October {0}", "{0} октября"),
    ("November {0}", "{0} ноября"),
    ("December {0}", "{0} декабря"),
    // A file's size.
    ("B", "Б"),
    ("KB", "КБ"),
    ("MB", "МБ"),
    ("GB", "ГБ"),
    // The field.
    ("Attach Files", "Приложить файлы"),
    // The bar over the field while a message is edited.
    ("Editing", "Редактирование"),
    // The field's ⌨ of a bot's keyboard (part 5).
    ("Show the Bot's Keyboard", "Показать клавиатуру бота"),
    ("Hide the Bot's Keyboard", "Скрыть клавиатуру бота"),
    // A poll (part 5): Vote for several options, a quiz's 💡.
    ("Vote", "Голосовать"),
    ("Explanation", "Пояснение"),
    // The reactions (part 5): the hover bar's ☺, the row's ⌄.
    ("Add Reaction", "Добавить реакцию"),
    ("All Reactions", "Все реакции"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
