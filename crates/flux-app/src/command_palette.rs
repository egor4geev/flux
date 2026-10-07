//! Палитра команд (cmd-shift-p): все действия, доступные там, где был фокус, по имени.
//!
//! Список собирается при открытии — до того, как фокус уйдёт в поле палитры: gpui
//! знает, какие действия обрабатывает путь от сфокусированного элемента до корня.
//! Выбор возвращает фокус на место и отправляет туда действие.

use flux_search::{FuzzyMatch, match_list};
use gpui::{
    Action, AnyElement, App, Context, DismissEvent, FocusHandle, KeyBinding, Modifiers,
    SharedString, Window, actions, div, prelude::*,
};

use crate::picker::{Picker, PickerDelegate, highlighted_text};
use crate::theme::Theme;
use crate::workspace::Workspace;

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
    label: String,
    /// Сочетание клавиш значками macOS, если есть.
    keys: Option<SharedString>,
    action: Box<dyn Action>,
}

pub struct CommandPalette {
    /// По алфавиту подписей.
    commands: Vec<Command>,
    matches: Vec<FuzzyMatch>,
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
                Command {
                    label: humanize_action_name(action.name()),
                    keys,
                    action,
                }
            })
            .collect();
        commands.sort_by(|a, b| a.label.cmp(&b.label));
        Self {
            commands,
            matches: Vec::new(),
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
        div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .child(div().whitespace_nowrap().child(highlighted_text(
                command.label.clone(),
                &found.positions,
                ui.match_text,
            )))
            .children(command.keys.clone().map(|keys| {
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_color(ui.dim)
                    .child(keys)
            }))
            .into_any_element()
    }

    fn empty_message(&self) -> SharedString {
        "No matching commands".into()
    }
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
