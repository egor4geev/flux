//! Part «Флаг страны»: the flag in the status bar and the console, notifications, settings (fork «status»).

pub(super) const STRINGS: &[(&str, &str)] = &[
    // The words and marks of the check.
    ("{0} s ago", "{0} с назад"),
    ("Checked {0}", "Проверено {0}"),
    ("checked {0}", "проверено {0}"),
    ("The country check is off", "Проверка страны выключена"),
    ("Checking the country…", "Проверка страны…"),
    ("Claude's requests are allowed", "Запросы Claude разрешены"),
    (
        "Claude's requests go without a check.",
        "Запросы Claude уходят без проверки.",
    ),
    (
        "Claude waits for the answer: nothing new starts or is sent meanwhile.",
        "Claude ждёт ответа: пока ничего нового не запускается и не отправляется.",
    ),
    (
        "{0} isn't on your list of blocked countries.",
        "Страны нет в вашем списке заблокированных: {0}.",
    ),
    (
        "{0} is on your list of blocked countries. Flux stopped Claude and sends nothing until the country changes.",
        "Страна в вашем списке заблокированных: {0}. Flux остановил Claude и ничего не отправляет, пока страна не сменится.",
    ),
    (
        "Flux couldn't find out the country of the IP address: the network may be down. Nothing goes to Claude until the country is known.",
        "Flux не смог определить страну IP-адреса — возможно, нет сети. В Claude ничего не уходит, пока страна неизвестна.",
    ),
    (
        "The country of the IP address is unknown",
        "Страна IP-адреса неизвестна",
    ),
    (
        "Checking the country of the IP address…",
        "Проверка страны IP-адреса…",
    ),
    (
        "Claude is blocked: the country is on your list",
        "Claude заблокирован: страна в вашем списке",
    ),
    (
        "Claude is blocked until the country is known",
        "Claude заблокирован, пока страна неизвестна",
    ),
    ("Claude waits for the answer", "Claude ждёт ответа"),
    // The popover.
    ("Country unknown", "Страна неизвестна"),
    ("The check got no answer", "Проверка не получила ответа"),
    (
        "Asking a neutral service where the network sees this Mac",
        "Спрашиваем нейтральный сервис, где сеть видит этот Mac",
    ),
    ("Country Check", "Проверка страны"),
    ("Off in Settings", "Выключена в настройках"),
    ("Checking now…", "Проверяем…"),
    (
        "Ask the country of the IP address again",
        "Снова определить страну IP-адреса",
    ),
    ("Settings → Claude Code", "Настройки → Claude Code"),
    // The notifications.
    ("Flux stopped Claude", "Flux остановил Claude"),
    (
        "Nothing goes to Claude until the country changes.",
        "В Claude ничего не уйдёт, пока страна не сменится.",
    ),
    (
        "Nothing goes to Claude until the country is known.",
        "В Claude ничего не уйдёт, пока страна неизвестна.",
    ),
    (
        "Claude is available again · {0}",
        "Claude снова доступен · {0}",
    ),
    ("Claude is available again", "Claude снова доступен"),
    // Settings → Claude Code → Country Check.
    (
        "Block Claude by the country of the IP address",
        "Блокировать Claude по стране IP-адреса",
    ),
    (
        "Flux finds out which country the network shows for this Mac and stops Claude while that country is on the list below or can't be found out.",
        "Flux определяет, из какой страны сеть видит этот Mac, и останавливает Claude, пока эта страна в списке ниже или её не удаётся определить.",
    ),
    (
        "The stop is at once: running sessions end, and nothing is sent to Claude until the country changes. Flux checks as it starts, every minute (every 10 s while Claude works), when the network changes and before a new request. It asks a neutral service, never Claude's servers.",
        "Остановка мгновенная: идущие сессии завершаются, и в Claude ничего не уходит, пока страна не сменится. Flux проверяет при запуске, каждую минуту (каждые 10 с, пока Claude работает), при смене сети и перед новым запросом. Спрашивает нейтральный сервис, никогда — серверы Claude.",
    ),
    ("Blocked countries", "Заблокированные страны"),
    (
        "Two-letter country codes, separated by commas: RU, BY, IR.",
        "Двухбуквенные коды стран через запятую: RU, BY, IR.",
    ),
    (
        "No countries on the list: Claude is blocked only when the country can't be found out.",
        "Список пуст: Claude блокируется, только когда страну не удаётся определить.",
    ),
    (
        "Not country codes: {0}. The list stays as it was until they are fixed.",
        "Не коды стран: {0}. Список остаётся прежним, пока их не исправить.",
    ),
    ("Not checked", "Не проверяется"),
    ("checking now…", "проверяем…"),
];

pub(super) const PLURALS: &[(&str, [&str; 3])] = &[];
