//! Палитра команд (cmd-shift-p): все действия, доступные там, где был фокус, по имени.
//!
//! Список собирается при открытии — до того, как фокус уйдёт в поле палитры: gpui
//! знает, какие действия обрабатывает путь от сфокусированного элемента до корня.
//! Выбор возвращает фокус на место и отправляет туда действие.

use flux_search::{FuzzyMatch, match_list};
use gpui::{
    Action, AnyElement, App, Context, DismissEvent, FocusHandle, FontWeight, Hsla, KeyBinding,
    Modifiers, SharedString, Window, actions, div, prelude::*, px,
};

use crate::picker::{Picker, PickerDelegate, highlighted_text};
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_SM};
use crate::workspace::Workspace;

/// Колонка чипа пространства имён: названия команд встают ровным столбцом.
const NAMESPACE_WIDTH: f32 = 108.;

actions!(command_palette, [Toggle]);

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-shift-p", Toggle, Some("Workspace"))]);
}

pub fn toggle(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    workspace.toggle_modal(window, cx, |window, cx| {
        let palette = CommandPalette::new(window, cx);
        Picker::new(palette, window, cx)
    });
}

struct Command {
    /// Полная подпись — по ней нечёткий поиск: «editor: move word left».
    label: String,
    /// Длина пространства имён в символах (до «: »): позиции совпадений делятся между чипом
    /// и названием.
    namespace_chars: usize,
    /// Сочетание клавиш значками macOS, если есть.
    keys: Option<SharedString>,
    action: Box<dyn Action>,
}

pub struct CommandPalette {
    /// По алфавиту подписей.
    commands: Vec<Command>,
    matches: Vec<FuzzyMatch>,
    /// Запрос пуст — в подвале всего команд, иначе «N of M».
    query_empty: bool,
    /// Где был фокус до открытия: туда вернётся фокус и уйдёт действие.
    previous_focus: Option<FocusHandle>,
}

impl CommandPalette {
    /// Вызывать до переноса фокуса в палитру.
    fn new(window: &mut Window, cx: &mut App) -> Self {
        let previous_focus = window.focused(cx);
        let mut commands: Vec<Command> = window
            .available_actions(cx)
            .into_iter()
            .filter(|action| action.name() != Toggle.name())
            .map(|action| {
                let bindings = match &previous_focus {
                    Some(focus) => window.bindings_for_action_in(action.as_ref(), focus),
                    None => window.bindings_for_action(action.as_ref()),
                };
                // Основное сочетание у нас привязывается первым, запасные (`home` к cmd-left,
                // `shift-backspace` к backspace) — после него.
                let keys = bindings.first().map(|binding| {
                    binding
                        .keystrokes()
                        .iter()
                        .map(|k| keystroke_label(k.modifiers(), k.key()))
                        .collect::<Vec<_>>()
                        .join(" ")
                        .into()
                });
                let label = humanize_action_name(action.name());
                Command {
                    namespace_chars: label
                        .find(": ")
                        .map_or(0, |end| label[..end].chars().count()),
                    label,
                    keys,
                    action,
                }
            })
            .collect();
        commands.sort_by(|a, b| a.label.cmp(&b.label));
        Self {
            commands,
            matches: Vec::new(),
            query_empty: true,
            previous_focus,
        }
    }
}

impl PickerDelegate for CommandPalette {
    fn placeholder(&self) -> SharedString {
        "Run a command…".into()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn update_matches(&mut self, query: &str, _: &mut Window, _: &mut Context<Picker<Self>>) {
        let labels: Vec<&str> = self.commands.iter().map(|c| c.label.as_str()).collect();
        self.matches = match_list(query, &labels);
        self.query_empty = query.trim().is_empty();
    }

    fn confirm(&mut self, index: usize, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(found) = self.matches.get(index) else {
            return;
        };
        let action = self.commands[found.index].action.boxed_clone();
        // `dispatch_action` берёт фокус в момент вызова — сначала вернуть его на место.
        if let Some(focus) = &self.previous_focus {
            window.focus(focus);
        }
        window.dispatch_action(action, cx);
        cx.emit(DismissEvent);
    }

    fn render_match(
        &mut self,
        index: usize,
        _selected: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let found = &self.matches[index];
        let command = &self.commands[found.index];
        let parts = LabelParts::new(&command.label, command.namespace_chars, &found.positions);
        let chip = (!parts.namespace.is_empty()).then(|| {
            let color = namespace_color(&parts.namespace, &ui);
            div()
                .flex_none()
                .max_w_full()
                .h(px(20.))
                .px_1p5()
                .flex()
                .items_center()
                .rounded(px(RADIUS_SM))
                .bg(UiColors::tint(color, 0.15))
                .text_size(px(theme::TEXT_XS))
                .font_weight(FontWeight::MEDIUM)
                .text_color(color)
                .whitespace_nowrap()
                .overflow_hidden()
                .child(highlighted_text(
                    title_case(&parts.namespace),
                    &parts.namespace_positions,
                    ui.foreground,
                ))
        });
        div()
            .w_full()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .w(px(NAMESPACE_WIDTH))
                    .flex()
                    .children(chip),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(highlighted_text(
                        title_case(&parts.title),
                        &parts.title_positions,
                        ui.match_text,
                    )),
            )
            .children(command.keys.as_ref().map(|keys| ui::keys(keys, ui)))
            .into_any_element()
    }

    fn render_footer(&self, _: &mut Window, _: &mut Context<Picker<Self>>) -> Option<AnyElement> {
        let total = self.commands.len();
        let text = if self.query_empty {
            format!("{total} commands")
        } else {
            format!("{} of {total} commands", self.matches.len())
        };
        Some(text.into_any_element())
    }

    fn empty_message(&self) -> SharedString {
        "No matching commands".into()
    }

    fn confirm_label(&self) -> &'static str {
        "run"
    }
}

/// Подпись, разделённая на пространство имён (чип) и название; позиции совпадений — в
/// символах каждой части.
#[derive(Debug, PartialEq, Eq)]
struct LabelParts {
    namespace: String,
    namespace_positions: Vec<usize>,
    title: String,
    title_positions: Vec<usize>,
}

impl LabelParts {
    /// `label` — «пространство: название»; `namespace_chars` — длина пространства имён
    /// (0 — его нет); `positions` — символы подписи, совпавшие с запросом.
    fn new(label: &str, namespace_chars: usize, positions: &[usize]) -> Self {
        let title_start = if namespace_chars == 0 {
            0
        } else {
            namespace_chars + 2
        };
        let namespace = label.chars().take(namespace_chars).collect();
        let title = label.chars().skip(title_start).collect();
        Self {
            namespace,
            namespace_positions: positions
                .iter()
                .copied()
                .filter(|&p| p < namespace_chars)
                .collect(),
            title,
            title_positions: positions
                .iter()
                .filter_map(|&p| p.checked_sub(title_start))
                .collect(),
        }
    }
}

/// Цвет чипа: свой оттенок у каждой области приложения.
fn namespace_color(namespace: &str, ui: &UiColors) -> Hsla {
    match namespace {
        "editor" => ui.blue,
        "workspace" => ui.violet,
        "file tree" => ui.green,
        "find bar" => ui.amber,
        "project search" => ui.orange,
        "command palette" | "file finder" | "go to line" | "picker" => ui.teal,
        _ => ui.indigo,
    }
}

/// «move word left» → «Move Word Left». Длина в символах не меняется — позиции совпадений
/// остаются верными (буква, которая при переводе в заглавную разрастается, остаётся как есть).
fn title_case(text: &str) -> String {
    let mut title = String::with_capacity(text.len());
    let mut word_start = true;
    for c in text.chars() {
        let mut upper = c.to_uppercase();
        match (word_start, upper.next(), upper.next()) {
            (true, Some(single), None) => title.push(single),
            _ => title.push(c),
        }
        word_start = c == ' ';
    }
    title
}

/// `editor::MoveWordLeft` → «editor: move word left», `command_palette::Toggle` →
/// «command palette: toggle»: пространство имён, двоеточие, слова из CamelCase.
pub fn humanize_action_name(name: &str) -> String {
    let (namespace, action) = name.rsplit_once("::").unwrap_or(("", name));
    let mut label = namespace.replace("::", " ").replace('_', " ");
    if !label.is_empty() {
        label.push_str(": ");
    }
    let chars: Vec<char> = action.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            label.push(' ');
            continue;
        }
        let prev = i.checked_sub(1).map(|p| chars[p]);
        let next = chars.get(i + 1).copied();
        // Граница слова: «moveWord», «move2Word», конец аббревиатуры «HTMLParser».
        let boundary = c.is_uppercase()
            && prev.is_some_and(|p| {
                p.is_lowercase()
                    || p.is_ascii_digit()
                    || (p.is_uppercase() && next.is_some_and(char::is_lowercase))
            });
        if boundary && !label.ends_with(' ') {
            label.push(' ');
        }
        label.extend(c.to_lowercase());
    }
    label
}

/// Нажатие значками macOS в принятом порядке ⌃⌥⇧⌘: `cmd-shift-p` → «⇧⌘P».
pub fn keystroke_label(modifiers: &Modifiers, key: &str) -> String {
    let mut label = String::new();
    if modifiers.function {
        label.push_str("fn ");
    }
    for (on, symbol) in [
        (modifiers.control, '⌃'),
        (modifiers.alt, '⌥'),
        (modifiers.shift, '⇧'),
        (modifiers.platform, '⌘'),
    ] {
        if on {
            label.push(symbol);
        }
    }
    label.push_str(&key_label(key));
    label
}

fn key_label(key: &str) -> String {
    let symbol = match key {
        "enter" => "↵",
        "escape" => "⎋",
        "backspace" => "⌫",
        "delete" => "⌦",
        "tab" => "⇥",
        "space" => "␣",
        "left" => "←",
        "right" => "→",
        "up" => "↑",
        "down" => "↓",
        "pageup" => "PgUp",
        "pagedown" => "PgDn",
        "home" => "Home",
        "end" => "End",
        key => {
            // Буква — заглавной, `f12` → «F12», остальное как есть.
            let mut chars = key.chars();
            return match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            };
        }
    };
    symbol.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_names_become_words() {
        assert_eq!(
            humanize_action_name("editor::MoveWordLeft"),
            "editor: move word left"
        );
        assert_eq!(
            humanize_action_name("workspace::NewFile"),
            "workspace: new file"
        );
        assert_eq!(
            humanize_action_name("command_palette::Toggle"),
            "command palette: toggle"
        );
        assert_eq!(humanize_action_name("editor::PageUp"), "editor: page up");
        assert_eq!(humanize_action_name("a::HTMLParser"), "a: html parser");
        assert_eq!(humanize_action_name("a::b::SaveAs"), "a b: save as");
        assert_eq!(humanize_action_name("Quit"), "quit");
        assert_eq!(humanize_action_name("x::Utf16Range"), "x: utf16 range");
    }

    #[test]
    fn titles_are_capitalized_word_by_word() {
        assert_eq!(title_case("move word left"), "Move Word Left");
        assert_eq!(title_case("file tree"), "File Tree");
        assert_eq!(title_case("utf16 range"), "Utf16 Range");
        assert_eq!(title_case(""), "");
        // ß в заглавной — «SS»: длина изменилась бы, поэтому буква остаётся.
        assert_eq!(title_case("ßa b"), "ßa B");
    }

    #[test]
    fn match_positions_split_between_chip_and_title() {
        // «editor: move left»: 0–5 — «editor», 6–7 — «: », с 8 — «move left».
        let parts = LabelParts::new("editor: move left", 6, &[0, 1, 7, 8, 13]);
        assert_eq!(parts.namespace, "editor");
        assert_eq!(parts.namespace_positions, [0, 1]);
        assert_eq!(parts.title, "move left");
        assert_eq!(parts.title_positions, [0, 5]);
        // Без пространства имён всё — название.
        let parts = LabelParts::new("quit", 0, &[0, 3]);
        assert_eq!(parts.namespace, "");
        assert_eq!(parts.title, "quit");
        assert_eq!(parts.title_positions, [0, 3]);
    }

    fn label(keystroke: &str) -> String {
        let keystroke = gpui::Keystroke::parse(keystroke).unwrap();
        keystroke_label(&keystroke.modifiers, &keystroke.key)
    }

    #[test]
    fn keystrokes_use_mac_symbols_in_mac_order() {
        assert_eq!(label("cmd-shift-p"), "⇧⌘P");
        assert_eq!(label("alt-left"), "⌥←");
        assert_eq!(label("ctrl-alt-shift-cmd-z"), "⌃⌥⇧⌘Z");
        assert_eq!(label("cmd-}"), "⌘}");
        assert_eq!(label("ctrl-tab"), "⌃⇥");
        assert_eq!(label("enter"), "↵");
        assert_eq!(label("escape"), "⎋");
        assert_eq!(label("alt-backspace"), "⌥⌫");
        assert_eq!(label("delete"), "⌦");
        assert_eq!(label("pagedown"), "PgDn");
        assert_eq!(label("space"), "␣");
        assert_eq!(label("cmd-9"), "⌘9");
        assert_eq!(label("f12"), "F12");
    }
}
