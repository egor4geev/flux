//! The notification center: groups, display settings, cards (stage 7, part B).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // Groups and their display (Settings → Notifications).
    ("General", "Общие"),
    ("Balloon", "Карточка"),
    ("Sticky balloon", "Липкая карточка"),
    ("Log only", "Только журнал"),
    ("Don't show", "Не показывать"),
    ("Notifications", "Уведомления"),
    (
        "Results of operations, errors and background tasks go to the Notifications window; a card in the corner shows them as they happen. Choose how each group shows.",
        "Итоги операций, ошибки и фоновые задачи попадают в окно «Уведомления»; карточка в углу показывает их в момент события. Выберите, как показывать каждую группу.",
    ),
    ("Do not disturb", "Не беспокоить"),
    (
        "No cards in the corner; notifications still go to the Notifications window.",
        "Карточек в углу нет; уведомления по-прежнему попадают в окно «Уведомления».",
    ),
    ("Groups", "Группы"),
];
