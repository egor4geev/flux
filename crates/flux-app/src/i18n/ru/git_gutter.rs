//! Git in the editor: gutter markers, the change popup, rollback of lines.

pub(super) const STRINGS: &[(&str, &str)] = &[
    ("Previous Change", "Предыдущее изменение"),
    ("Next Change", "Следующее изменение"),
    ("Rollback", "Откатить"),
    ("Show Diff", "Показать дифф"),
    ("Copy the previous text", "Скопировать прежний текст"),
    ("No changes at the cursor", "У курсора нет изменений"),
    // Command palette titles of the actions.
    ("Rollback Lines", "Откатить строки"),
    ("Show Change", "Показать изменение"),
    ("Hide Change", "Скрыть изменение"),
];
