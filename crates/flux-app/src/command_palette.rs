//! Command palette (cmd-shift-p): all actions available where focus was, by name, and the commands
//! and tool windows of the running plugins.
//!
//! The list is built when the palette opens, before focus moves into the palette's input field:
//! gpui knows which actions are handled along the path from the focused element to the root. Making
//! a choice returns focus to where it was and dispatches the action there.

use flux_search::{FuzzyMatch, match_list};
use gpui::{
    Action, AnyElement, App, Context, DismissEvent, FocusHandle, FontWeight, Hsla, KeyBinding,
    Modifiers, SharedString, Window, actions, div, prelude::*, px,
};

use crate::i18n::{tr, trn};
use crate::picker::{Picker, PickerDelegate, highlighted_text};
use crate::plugins::{PaletteCommand, PaletteToolWindow};
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_SM};
use crate::workspace::Workspace;

/// Namespace chip column: command names line up in an even column.
const NAMESPACE_WIDTH: f32 = 108.;

actions!(command_palette, [Toggle]);

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-shift-p", Toggle, Some("Workspace"))]);
}

pub fn toggle(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let plugins = workspace.plugins.read(cx);
    let plugin_commands = plugins.palette_commands();
    let plugin_windows = plugins.palette_tool_windows();
    workspace.toggle_modal(window, cx, |window, cx| {
        let palette = CommandPalette::new(plugin_commands, plugin_windows, window, cx);
        Picker::new(palette, window, cx)
    });
}

struct Command {
    /// Full label, which fuzzy search matches against: "editor: move word left".
    label: String,
    /// Namespace length in characters (up to ": "): match positions are split between the chip and
    /// the name.
    namespace_chars: usize,
    /// English namespace («file tree»): it picks the color of the chip, whatever the language.
    namespace: String,
    /// Key binding in macOS symbols, if there is one.
    keys: Option<SharedString>,
    action: Box<dyn Action>,
    /// A plugin grayed the command out: it is shown dimmed and doesn't run.
    enabled: bool,
}

pub struct CommandPalette {
    /// Alphabetical by label.
    commands: Vec<Command>,
    matches: Vec<FuzzyMatch>,
    /// If the query is empty, the footer shows the total number of commands; otherwise "N of M".
    query_empty: bool,
    /// Where focus was before opening: focus returns there, and the action is dispatched there.
    previous_focus: Option<FocusHandle>,
}

impl CommandPalette {
    /// Call before moving focus into the palette. The plugins' commands and tool windows come
    /// translated from their hub.
    fn new(
        plugin_commands: Vec<PaletteCommand>,
        plugin_windows: Vec<PaletteToolWindow>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let previous_focus = window.focused(cx);
        let mut commands: Vec<Command> = window
            .available_actions(cx)
            .into_iter()
            .filter(|action| action.name() != Toggle.name())
            .map(|action| {
                let keys = keys_label(action.as_ref(), previous_focus.as_ref(), window);
                let humanized = humanize_action_name(action.name());
                let (namespace, title) = humanized.split_once(": ").unwrap_or(("", &humanized));
                // The chip and the title are looked up as «Editor» and «Move Word Left».
                let chip = tr(&title_case(namespace)).to_string();
                let title = tr(&title_case(title)).to_string();
                let label = if chip.is_empty() {
                    title
                } else {
                    format!("{chip}: {title}")
                };
                Command {
                    namespace_chars: chip.chars().count(),
                    namespace: namespace.to_string(),
                    label,
                    keys,
                    action,
                    enabled: true,
                }
            })
            .collect();
        let plugin_command = |category: String, title: String, action: Box<dyn Action>| Command {
            namespace_chars: category.chars().count(),
            label: format!("{category}: {title}"),
            namespace: PLUGIN_NAMESPACE.to_string(),
            keys: keys_label(action.as_ref(), previous_focus.as_ref(), window),
            action,
            enabled: true,
        };
        for command in plugin_commands {
            let enabled = command.enabled;
            let action = Box::new(command.action);
            commands.push(Command {
                enabled,
                ..plugin_command(command.category, command.title, action)
            });
        }
        for tool in plugin_windows {
            commands.push(plugin_command(
                tool.category,
                tool.title,
                Box::new(tool.action),
            ));
        }
        commands.sort_by_cached_key(|command| command.label.to_lowercase());
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
        tr("Run a command…").into()
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
        if !self.commands[found.index].enabled {
            return;
        }
        let action = self.commands[found.index].action.boxed_clone();
        // `dispatch_action` takes focus at the moment of the call, so restore focus to its place
        // first.
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
        let LabelParts {
            namespace,
            namespace_positions,
            title,
            title_positions,
        } = LabelParts::new(&command.label, command.namespace_chars, &found.positions);
        let chip = (!namespace.is_empty()).then(|| {
            let color = namespace_color(&command.namespace, &ui);
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
                    namespace,
                    &namespace_positions,
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
                    .when(!command.enabled, |title| title.text_color(ui.dim))
                    .child(highlighted_text(
                        title,
                        &title_positions,
                        if command.enabled {
                            ui.match_text
                        } else {
                            ui.dim
                        },
                    )),
            )
            .children(command.keys.as_ref().map(|keys| ui::keys(keys, ui)))
            .into_any_element()
    }

    fn render_footer(&self, _: &mut Window, _: &mut Context<Picker<Self>>) -> Option<AnyElement> {
        let total = self.commands.len();
        let text = if self.query_empty {
            trn(total, "{n} command", "{n} commands")
        } else {
            let shown = self.matches.len();
            format!(
                "{shown} {}",
                trn(total, "of {n} command", "of {n} commands")
            )
        };
        Some(text.into_any_element())
    }

    fn empty_message(&self) -> SharedString {
        tr("No matching commands").into()
    }

    fn confirm_label(&self) -> &'static str {
        tr("run")
    }
}

/// The chip's namespace of the plugins' commands: one color for all of them.
const PLUGIN_NAMESPACE: &str = "plugin";

/// The first key binding of an action where focus was, in macOS symbols. Our primary binding is
/// registered first, with the alternates (`home` for cmd-left, `shift-backspace` for backspace)
/// after it.
fn keys_label(
    action: &dyn Action,
    focus: Option<&FocusHandle>,
    window: &Window,
) -> Option<SharedString> {
    let bindings = match focus {
        Some(focus) => window.bindings_for_action_in(action, focus),
        None => window.bindings_for_action(action),
    };
    bindings.first().map(|binding| {
        binding
            .keystrokes()
            .iter()
            .map(|k| keystroke_label(k.modifiers(), k.key()))
            .collect::<Vec<_>>()
            .join(" ")
            .into()
    })
}

/// A label split into the namespace (chip) and the name; match positions are in characters of each
/// part.
#[derive(Debug, PartialEq, Eq)]
struct LabelParts {
    namespace: String,
    namespace_positions: Vec<usize>,
    title: String,
    title_positions: Vec<usize>,
}

impl LabelParts {
    /// `label` is "namespace: name"; `namespace_chars` is the namespace length (0 if there is
    /// none); `positions` are the label characters that matched the query.
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

/// Chip color: each area of the app has its own shade.
fn namespace_color(namespace: &str, ui: &UiColors) -> Hsla {
    match namespace {
        "editor" => ui.blue,
        "workspace" => ui.violet,
        "file tree" => ui.green,
        "find bar" => ui.amber,
        "project search" => ui.orange,
        "command palette" | "file finder" | "go to line" | "picker" => ui.teal,
        PLUGIN_NAMESPACE | "plugins" => ui.pink,
        "claude" | "claude composer" | "claude panel" => ui.orange,
        _ => ui.indigo,
    }
}

/// "move word left" → "Move Word Left". The length in characters doesn't change, so match positions
/// stay valid (a letter that would expand when uppercased is left as is).
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

/// `editor::MoveWordLeft` → "editor: move word left", `command_palette::Toggle` → "command palette:
/// toggle": the namespace, a colon, then the words from the CamelCase name.
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
        // Word boundary: "moveWord", "move2Word", the end of an acronym as in "HTMLParser".
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

/// A keystroke in macOS symbols, in the standard order ⌃⌥⇧⌘: `cmd-shift-p` → "⇧⌘P".
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
            // A letter is uppercased, `f12` → "F12", anything else is left as is.
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
        // ß uppercases to "SS", which would change the length, so the letter is left as is.
        assert_eq!(title_case("ßa b"), "ßa B");
    }

    #[test]
    fn match_positions_split_between_chip_and_title() {
        // "editor: move left": 0–5 is "editor", 6–7 is ": ", from 8 on is "move left".
        let parts = LabelParts::new("editor: move left", 6, &[0, 1, 7, 8, 13]);
        assert_eq!(parts.namespace, "editor");
        assert_eq!(parts.namespace_positions, [0, 1]);
        assert_eq!(parts.title, "move left");
        assert_eq!(parts.title_positions, [0, 5]);
        // Without a namespace, everything is the name.
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
