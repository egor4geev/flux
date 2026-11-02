//! Flux's keys on each system (part 10.9). Flux grew up on macOS, and every binding is written
//! once, in macOS keys, by its module; [`bind_keys`] registers it as written on macOS and in
//! Windows keys elsewhere ([`windows_keys`]):
//!
//! - where the macOS key is the JetBrains macOS one for the action, the JetBrains Windows key for
//!   the same action ([`WINDOWS_KEYS`]: ⌘⌫ Delete Line → Ctrl+Y, ⌘, Settings → Ctrl+Alt+S, ⌘1 →
//!   Alt+1);
//! - text editing in the editor and the inputs by the system's conventions: Ctrl+←/→ by word,
//!   Home/End, Ctrl+Home/End, Ctrl+Backspace (the same table);
//! - no binding for what Windows has no place for (⌘H Hide) or keeps for itself (Alt+Tab,
//!   Ctrl+Esc);
//! - the rest by rule: ⌘ → Ctrl, ⌥ → Alt, ⌃ stays Ctrl, ⇧ stays Shift.
//!
//! The labels follow the same system: [`label`] turns a macOS label ("⇧⌘F", "⌘↵") into the
//! system's ("Ctrl+Shift+F", "Ctrl+Enter"). Linux will get its own keys; until then it gets the
//! Windows ones, as gpui's `secondary` is Ctrl there too.

use std::borrow::Cow;

use gpui::{App, DummyKeyboardMapper, KeyBinding};

/// Whose keys Flux uses: macOS's or Windows'. A value rather than `cfg!` so that the Windows keys
/// and labels are tested on a Mac too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyOs {
    Mac,
    Windows,
}

impl KeyOs {
    /// The system Flux runs on.
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else {
            Self::Windows
        }
    }
}

/// Registers bindings written in macOS keys: as they are on macOS, in Windows keys elsewhere (each
/// one through [`windows_keys`]; a binding may become none or several). Every Flux module binds its
/// keys through here; keys that are already the system's (copied from a registered binding) go to
/// `cx.bind_keys` directly.
pub fn bind_keys(cx: &mut App, bindings: impl IntoIterator<Item = KeyBinding>) {
    match KeyOs::current() {
        KeyOs::Mac => cx.bind_keys(bindings),
        KeyOs::Windows => cx.bind_keys(
            bindings
                .into_iter()
                .flat_map(|binding| windows_bindings(&binding)),
        ),
    }
}

/// A binding in macOS keys → its bindings in Windows keys: the same action and key context.
fn windows_bindings(binding: &KeyBinding) -> Vec<KeyBinding> {
    let mac = binding
        .keystrokes()
        .iter()
        .map(|keystroke| keystroke.inner().unparse())
        .collect::<Vec<_>>()
        .join(" ");
    windows_keys(binding.action().name(), &mac)
        .iter()
        .filter(|keys| !reserved_on_windows(keys))
        .filter_map(|keys| {
            KeyBinding::load(
                keys,
                binding.action().boxed_clone(),
                binding.predicate(),
                false,
                binding.action_input(),
                &DummyKeyboardMapper,
            )
            .ok()
        })
        .collect()
}

/// Keys written in macOS keys (a plugin's `keys`) in the keys of the system Flux runs on: as they
/// are on macOS, by rule elsewhere ([`windows_keys`] without an action of Flux).
pub fn system_keys(mac: &str) -> String {
    match KeyOs::current() {
        KeyOs::Mac => mac.to_string(),
        KeyOs::Windows => windows_keys("", mac).into_iter().next().unwrap_or_default(),
    }
}

/// The bindings whose Windows keys are not ⌘ → Ctrl, ⌥ → Alt: (the action's name, its macOS
/// keys, its Windows keys — none, one, or the main one and alternates). JetBrains' Windows keymap
/// where the macOS key is JetBrains'; the system's conventions for text editing and for the
/// terminal (as in Windows Terminal); a free key where the rule would give one that Windows keeps
/// for itself.
pub const WINDOWS_KEYS: &[(&str, &str, &[&str])] = &[
    // --- macOS only: hiding apps and minimizing are the system's business on Windows. ---
    ("flux::Hide", "cmd-h", &[]),
    ("flux::HideOthers", "alt-cmd-h", &[]),
    ("window::Minimize", "cmd-m", &[]),
    // JetBrains: Next / Previous Project Window (⌘` / ⇧⌘` on macOS). ⌘~ is how macOS may send ⇧⌘`.
    ("window::NextWindow", "cmd-`", &["ctrl-alt-]"]),
    ("window::PreviousWindow", "cmd-shift-`", &["ctrl-alt-["]),
    ("window::PreviousWindow", "cmd-~", &[]),
    // Quit: JetBrains has no key for Exit on Windows (Ctrl+Q is Quick Documentation); Alt+F4
    // closes the window by the system.
    ("workspace::Quit", "cmd-q", &[]),
    // JetBrains: Close Tab is Ctrl+F4; Ctrl+W as well, as everywhere on Windows (JetBrains' Extend
    // Selection, which Flux has not).
    ("workspace::CloseTab", "cmd-w", &["ctrl-f4", "ctrl-w"]),
    // JetBrains: Select Next / Previous Tab (⇧⌘] / ⇧⌘[ on macOS) — Alt+→ / Alt+←. macOS may send
    // ⇧⌘] as ⌘}; one binding is enough on Windows.
    ("workspace::NextTab", "cmd-}", &["alt-right"]),
    ("workspace::PrevTab", "cmd-{", &["alt-left"]),
    ("workspace::NextTab", "cmd-shift-]", &[]),
    ("workspace::PrevTab", "cmd-shift-[", &[]),
    // JetBrains: Settings (⌘,) — Ctrl+Alt+S.
    ("settings::Toggle", "cmd-,", &["ctrl-alt-s"]),
    (
        "lsp_servers_popup::ConfigureSelected",
        "cmd-,",
        &["ctrl-alt-s"],
    ),
    ("claude::OpenSettings", "cmd-,", &["ctrl-alt-s"]),
    // JetBrains: tool windows ⌘1 Project, ⌘9 Git, ⌘0 Commit — Alt+1, Alt+9, Alt+0.
    ("file_tree::ToggleOpen", "cmd-1", &["alt-1"]),
    ("git::ToggleGitWindow", "cmd-9", &["alt-9"]),
    ("git::ToggleCommitWindow", "cmd-0", &["alt-0"]),
    // JetBrains: VCS Operations Popup (⌃V) — Alt+`. The terminal's ⌃V guard isn't needed then:
    // Ctrl+V pastes there.
    ("git::VcsOperations", "ctrl-v", &["alt-`"]),
    ("zed::NoAction", "ctrl-v", &[]),
    // JetBrains: Go to Line (⌘L) — Ctrl+G.
    ("go_to_line::Toggle", "cmd-l", &["ctrl-g"]),
    // JetBrains: Find Next / Previous (⌘G / ⇧⌘G) — F3 / Shift+F3.
    ("find_bar::FindNext", "cmd-g", &["f3"]),
    ("find_bar::FindPrevious", "cmd-shift-g", &["shift-f3"]),
    // The search options (⌥⌘C/W/R): Alt+C/W/R, the Windows keys of the same set; Ctrl+Alt+C
    // opens the Claude console.
    ("find_bar::ToggleCaseSensitive", "alt-cmd-c", &["alt-c"]),
    ("find_bar::ToggleWholeWord", "alt-cmd-w", &["alt-w"]),
    ("find_bar::ToggleRegex", "alt-cmd-r", &["alt-r"]),
    (
        "project_search::ToggleCaseSensitive",
        "alt-cmd-c",
        &["alt-c"],
    ),
    ("project_search::ToggleWholeWord", "alt-cmd-w", &["alt-w"]),
    ("project_search::ToggleRegex", "alt-cmd-r", &["alt-r"]),
    // Go to File: ⌘P as on macOS, plus JetBrains' Ctrl+Shift+N.
    ("file_finder::Toggle", "cmd-p", &["ctrl-p", "ctrl-shift-n"]),
    // JetBrains: Quick Documentation (F1 on macOS) — Ctrl+Q.
    ("editor::ShowHover", "f1", &["ctrl-q"]),
    // JetBrains: Back / Forward (⌘[ / ⌘]) — Ctrl+Alt+← / →; ⌥⌘←/→ map there too, once is enough.
    ("navigation::NavigateBack", "cmd-[", &["ctrl-alt-left"]),
    ("navigation::NavigateForward", "cmd-]", &["ctrl-alt-right"]),
    ("navigation::NavigateBack", "cmd-alt-left", &[]),
    ("navigation::NavigateForward", "cmd-alt-right", &[]),
    // JetBrains: Jump to Source (⌘↓) — F4, which these lists already have.
    ("commit_panel::JumpToSource", "cmd-down", &[]),
    ("file_tree::Open", "cmd-down", &["f4"]),
    // JetBrains: Copy Path (⇧⌘C) — Ctrl+Shift+C; Copy Path From Content Root — Ctrl+Alt+Shift+C.
    ("file_tree::CopyPath", "alt-cmd-c", &["ctrl-shift-c"]),
    ("commit_panel::CopyPath", "alt-cmd-c", &["ctrl-shift-c"]),
    (
        "file_tree::CopyRelativePath",
        "alt-shift-cmd-c",
        &["ctrl-alt-shift-c"],
    ),
    // JetBrains: New… in the project view (⌘N) — Alt+Insert; Ctrl+N there too, as on macOS.
    ("file_tree::NewFile", "cmd-n", &["alt-insert", "ctrl-n"]),
    // JetBrains: Delete (⌘⌫ in lists on macOS) — Delete.
    ("file_tree::MoveToTrash", "cmd-backspace", &["delete"]),
    ("commit_panel::Delete", "cmd-backspace", &["delete"]),
    ("stash_panel::Drop", "cmd-backspace", &["delete"]),
    (
        "claude_console::CloseSelected",
        "cmd-backspace",
        &["delete"],
    ),
    // A picker's entry: Shift+Delete, as a suggestion is removed on Windows; the picker's input
    // keeps Delete for its text.
    ("picker::DeleteSelected", "cmd-backspace", &["shift-delete"]),
    // The Claude console: Ctrl+Esc is the Start menu (Ctrl+Shift+Esc the Task Manager, Alt+Esc
    // switches windows); C for Claude.
    ("claude::ToggleClaude", "cmd-escape", &["ctrl-alt-c"]),
    // Diff and merge: accept a change (⌃⌘→ / ⌃⌘←). The rule would give Ctrl+→ — a word.
    (
        "diff::RevertChange",
        "cmd-ctrl-right",
        &["ctrl-alt-shift-right"],
    ),
    (
        "merge_tool::AcceptLeftChange",
        "cmd-ctrl-right",
        &["ctrl-alt-shift-right"],
    ),
    (
        "merge_tool::AcceptRightChange",
        "cmd-ctrl-left",
        &["ctrl-alt-shift-left"],
    ),
    // A Markdown preview: the top / the bottom (⌘↑ / ⌘↓) — Ctrl+Home / Ctrl+End.
    ("markdown::ScrollToTop", "cmd-up", &["ctrl-home"]),
    ("markdown::ScrollToBottom", "cmd-down", &["ctrl-end"]),
    // --- The terminal, as in Windows Terminal: Ctrl+letters go to the program (^C, ^D, ^W, ^K…);
    // Ctrl+C copies a selection (`terminal_view`), Ctrl+V pastes. ---
    ("terminal::Copy", "cmd-c", &["ctrl-shift-c"]),
    ("terminal::Paste", "cmd-v", &["ctrl-v", "ctrl-shift-v"]),
    ("terminal::SelectAll", "cmd-a", &["ctrl-shift-a"]),
    // Clear: no key on Windows (Ctrl+K is the program's; Ctrl+L clears the screen in shells).
    ("terminal::Clear", "cmd-k", &[]),
    ("terminal::ScrollToTop", "cmd-home", &["ctrl-shift-home"]),
    ("terminal::ScrollToBottom", "cmd-end", &["ctrl-shift-end"]),
    ("terminal::NewTerminal", "cmd-t", &["ctrl-shift-t"]),
    ("terminal::SplitRight", "cmd-d", &["ctrl-shift-d"]),
    ("terminal::SplitDown", "cmd-shift-d", &["ctrl-alt-shift-d"]),
    ("terminal::ClosePane", "cmd-w", &["ctrl-shift-w"]),
    // JetBrains: Next / Previous Splitter (⌥⇥ / ⌥⇧⇥) is Alt+Tab on Windows — the system's.
    (
        "terminal::FocusNextPane",
        "alt-tab",
        &["ctrl-shift-]", "ctrl-}"],
    ),
    (
        "terminal::FocusPreviousPane",
        "alt-shift-tab",
        &["ctrl-shift-[", "ctrl-{"],
    ),
    // --- Text editing: the editor (JetBrains) and the inputs (the system's). ---
    ("editor::MoveWordLeft", "alt-left", &["ctrl-left"]),
    ("editor::MoveWordRight", "alt-right", &["ctrl-right"]),
    (
        "editor::SelectWordLeft",
        "alt-shift-left",
        &["ctrl-shift-left"],
    ),
    (
        "editor::SelectWordRight",
        "alt-shift-right",
        &["ctrl-shift-right"],
    ),
    ("editor::MoveLineStart", "cmd-left", &[]),
    ("editor::MoveLineEnd", "cmd-right", &[]),
    ("editor::SelectLineStart", "cmd-shift-left", &[]),
    ("editor::SelectLineEnd", "cmd-shift-right", &[]),
    ("editor::MoveDocumentStart", "cmd-up", &["ctrl-home"]),
    ("editor::MoveDocumentEnd", "cmd-down", &["ctrl-end"]),
    (
        "editor::SelectDocumentStart",
        "cmd-shift-up",
        &["ctrl-shift-home"],
    ),
    (
        "editor::SelectDocumentEnd",
        "cmd-shift-down",
        &["ctrl-shift-end"],
    ),
    (
        "editor::DeleteWordBackward",
        "alt-backspace",
        &["ctrl-backspace"],
    ),
    // JetBrains: Delete Line (⌘⌫) — Ctrl+Y; Redo stays Ctrl+Shift+Z.
    ("editor::DeleteLine", "cmd-backspace", &["ctrl-y"]),
    ("input::MoveWordLeft", "alt-left", &["ctrl-left"]),
    ("input::MoveWordRight", "alt-right", &["ctrl-right"]),
    (
        "input::SelectWordLeft",
        "alt-shift-left",
        &["ctrl-shift-left"],
    ),
    (
        "input::SelectWordRight",
        "alt-shift-right",
        &["ctrl-shift-right"],
    ),
    ("input::MoveLineStart", "cmd-left", &[]),
    ("input::MoveLineEnd", "cmd-right", &[]),
    ("input::SelectLineStart", "cmd-shift-left", &[]),
    ("input::SelectLineEnd", "cmd-shift-right", &[]),
    (
        "input::DeleteWordBackward",
        "alt-backspace",
        &["ctrl-backspace"],
    ),
    // Delete to the line's start (⌘⌫): Windows has no key for it, and Ctrl+Backspace is a word.
    ("input::DeleteToLineStart", "cmd-backspace", &[]),
    // Redo in an input: Ctrl+Shift+Z as in the editor, and Windows' Ctrl+Y (no Delete Line here).
    ("input::Redo", "cmd-shift-z", &["ctrl-shift-z", "ctrl-y"]),
    ("prompt_input::MoveWordLeft", "alt-left", &["ctrl-left"]),
    ("prompt_input::MoveWordRight", "alt-right", &["ctrl-right"]),
    (
        "prompt_input::SelectWordLeft",
        "alt-shift-left",
        &["ctrl-shift-left"],
    ),
    (
        "prompt_input::SelectWordRight",
        "alt-shift-right",
        &["ctrl-shift-right"],
    ),
    ("prompt_input::MoveLineStart", "cmd-left", &[]),
    ("prompt_input::MoveLineEnd", "cmd-right", &[]),
    ("prompt_input::SelectLineStart", "cmd-shift-left", &[]),
    ("prompt_input::SelectLineEnd", "cmd-shift-right", &[]),
    ("prompt_input::MoveToStart", "cmd-up", &["ctrl-home"]),
    ("prompt_input::MoveToEnd", "cmd-down", &["ctrl-end"]),
    (
        "prompt_input::SelectToStart",
        "cmd-shift-up",
        &["ctrl-shift-home"],
    ),
    (
        "prompt_input::SelectToEnd",
        "cmd-shift-down",
        &["ctrl-shift-end"],
    ),
    (
        "prompt_input::DeleteWordBackward",
        "alt-backspace",
        &["ctrl-backspace"],
    ),
    ("prompt_input::DeleteToLineStart", "cmd-backspace", &[]),
    (
        "prompt_input::Redo",
        "cmd-shift-z",
        &["ctrl-shift-z", "ctrl-y"],
    ),
];

/// The Windows keys of a binding written in macOS keys (`mac`, a chord's keystrokes apart) for the
/// action named `action` (`editor::DeleteLine`; empty for a plugin's): the table's
/// ([`WINDOWS_KEYS`]), else by rule — ⌘ → Ctrl, ⌥ → Alt, ⌃ and ⇧ as they are (⌃⌘ together →
/// Ctrl+Alt). The first is the one the labels show.
pub fn windows_keys(action: &str, mac: &str) -> Vec<String> {
    let Some(strokes) = parse(mac) else {
        return vec![mac.to_string()];
    };
    if let Some((_, _, keys)) = WINDOWS_KEYS
        .iter()
        .find(|(name, keys, _)| *name == action && parse(keys).as_ref() == Some(&strokes))
    {
        return keys.iter().map(|keys| keys.to_string()).collect();
    }
    let strokes: Vec<String> = strokes
        .into_iter()
        .map(|stroke| {
            Stroke {
                control: stroke.control || stroke.platform,
                alt: stroke.alt || (stroke.control && stroke.platform),
                platform: false,
                ..stroke
            }
            .unparse()
        })
        .collect();
    vec![strokes.join(" ")]
}

/// Whether Windows keeps `keys` for itself (the Start menu, switching windows, the Task Manager…)
/// or they hold the Windows key: Flux never binds these on Windows.
pub fn reserved_on_windows(keys: &str) -> bool {
    const RESERVED: &[&str] = &[
        "ctrl-escape",
        "ctrl-shift-escape",
        "alt-escape",
        "alt-shift-escape",
        "alt-tab",
        "alt-shift-tab",
        "ctrl-alt-tab",
        "alt-f4",
        "alt-space",
        "ctrl-alt-delete",
        "f10",
        "printscreen",
        "alt-printscreen",
    ];
    let Some(strokes) = parse(keys) else {
        return false;
    };
    strokes.iter().any(|stroke| {
        stroke.platform
            || RESERVED
                .iter()
                .any(|reserved| parse(reserved).as_deref() == Some(std::slice::from_ref(stroke)))
    })
}

/// A keystroke as text: its modifiers and its key. A small parser of its own (rather than gpui's
/// `Keystroke::parse`, which reads `cmd` as the Windows key on Windows) so that macOS keys read the
/// same on every system.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stroke {
    control: bool,
    alt: bool,
    shift: bool,
    platform: bool,
    function: bool,
    key: String,
}

impl Stroke {
    /// `ctrl-alt-shift-key`, gpui's notation, in this order.
    fn unparse(&self) -> String {
        let mut text = String::new();
        for (on, name) in [
            (self.control, "ctrl-"),
            (self.alt, "alt-"),
            (self.shift, "shift-"),
            (self.platform, "cmd-"),
            (self.function, "fn-"),
        ] {
            if on {
                text.push_str(name);
            }
        }
        text.push_str(&self.key);
        text
    }
}

/// `alt-shift-cmd-c`, `cmd--`, `cmd-k cmd-s` → keystrokes; none if a keystroke has no key.
fn parse(keys: &str) -> Option<Vec<Stroke>> {
    keys.split_whitespace()
        .map(|source| {
            let mut stroke = Stroke {
                control: false,
                alt: false,
                shift: false,
                platform: false,
                function: false,
                key: String::new(),
            };
            let mut rest = source;
            while let Some((modifier, tail)) = rest.split_once('-') {
                if tail.is_empty() {
                    break;
                }
                match modifier {
                    "ctrl" | "control" => stroke.control = true,
                    "alt" | "option" => stroke.alt = true,
                    "shift" => stroke.shift = true,
                    "cmd" | "super" | "win" | "platform" => stroke.platform = true,
                    "fn" => stroke.function = true,
                    _ => break,
                }
                rest = tail;
            }
            (!rest.is_empty()).then(|| Stroke {
                key: rest.to_string(),
                ..stroke
            })
        })
        .collect()
}

/// Whether the modifiers are ⌘ alone on macOS, Ctrl alone on Windows (Fn aside): ⌘-click goes to a
/// definition or opens a link, Ctrl+click on Windows.
pub fn secondary_alone(os: KeyOs, modifiers: &gpui::Modifiers) -> bool {
    let (secondary, other) = match os {
        KeyOs::Mac => (modifiers.platform, modifiers.control),
        KeyOs::Windows => (modifiers.control, modifiers.platform),
    };
    secondary && !other && !modifiers.shift && !modifiers.alt
}

/// A label written in macOS symbols ("⇧⌘F", "⌘↵", "⇧↑↓", "esc") in the system's notation: as it is
/// on macOS; on Windows the modifiers by name in the order Ctrl, Alt, Shift, joined by `+`, and the
/// keys by name ("Ctrl+Shift+F", "Ctrl+Enter", "Shift+↑↓", "Esc"). A label already in Windows
/// notation stays as it is. Groups (a chord's keystrokes) stay apart.
pub fn label(os: KeyOs, mac: &str) -> Cow<'_, str> {
    if os == KeyOs::Mac {
        return Cow::Borrowed(mac);
    }
    let groups: Vec<String> = mac
        .split(' ')
        .map(|group| {
            let mut modifiers = [false; 3];
            let mut rest = group;
            while let Some(c) = rest.chars().next().filter(|c| "⌃⌥⇧⌘".contains(*c)) {
                let index = match c {
                    '⌃' | '⌘' => 0,
                    '⌥' => 1,
                    _ => 2,
                };
                modifiers[index] = true;
                rest = &rest[c.len_utf8()..];
            }
            let key = match rest {
                "↵" => "Enter",
                "⌫" => "Backspace",
                "⌦" => "Delete",
                "⇥" => "Tab",
                "␣" => "Space",
                "esc" | "⎋" => "Esc",
                key => key,
            };
            let mut parts: Vec<&str> = ["Ctrl", "Alt", "Shift"]
                .into_iter()
                .zip(modifiers)
                .filter_map(|(name, on)| on.then_some(name))
                .collect();
            if !key.is_empty() {
                parts.push(key);
            }
            parts.join("+")
        })
        .collect();
    Cow::Owned(groups.join(" "))
}

/// The label of the keys `mac` (macOS keys) of the action named `action` on the system Flux runs
/// on, from [`WINDOWS_KEYS`] — for text that names a key where no `App` is at hand; with one,
/// `ui::shortcut_for` reads the keymap itself.
pub fn shortcut(action: &str, mac: &str) -> String {
    let os = KeyOs::current();
    let keys = match os {
        KeyOs::Mac => mac.to_string(),
        KeyOs::Windows => windows_keys(action, mac)
            .into_iter()
            .next()
            .unwrap_or_default(),
    };
    keys_label(os, &keys)
}

/// Keys in gpui's notation (`cmd-shift-p`, `cmd-k cmd-s`) as a label of `os`.
pub fn keys_label(os: KeyOs, keys: &str) -> String {
    keys.split_whitespace()
        .filter_map(|keys| gpui::Keystroke::parse(keys).ok())
        .map(|keystroke| {
            crate::command_palette::keystroke_label_for(os, &keystroke.modifiers, &keystroke.key)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn windows(action: &str, mac: &str) -> Vec<String> {
        windows_keys(action, mac)
    }

    #[test]
    fn jetbrains_keys_come_from_the_table_and_the_rest_by_rule() {
        assert_eq!(windows("editor::DeleteLine", "cmd-backspace"), ["ctrl-y"]);
        assert_eq!(windows("settings::Toggle", "cmd-,"), ["ctrl-alt-s"]);
        assert_eq!(windows("file_tree::ToggleOpen", "cmd-1"), ["alt-1"]);
        assert_eq!(
            windows("workspace::CloseTab", "cmd-w"),
            ["ctrl-f4", "ctrl-w"]
        );
        assert!(windows("flux::Hide", "cmd-h").is_empty());
        // The order of the modifiers in the source doesn't matter.
        assert_eq!(
            windows("file_tree::CopyRelativePath", "shift-alt-cmd-c"),
            ["ctrl-alt-shift-c"]
        );
        // By rule.
        assert_eq!(windows("git::Commit", "cmd-k"), ["ctrl-k"]);
        assert_eq!(windows("git::Push", "cmd-shift-k"), ["ctrl-shift-k"]);
        assert_eq!(windows("navigation::FindUsages", "alt-f7"), ["alt-f7"]);
        assert_eq!(windows("x::ZoomOut", "cmd--"), ["ctrl--"]);
        assert_eq!(windows("x::Chord", "cmd-k cmd-s"), ["ctrl-k ctrl-s"]);
        assert_eq!(windows("x::Both", "ctrl-cmd-x"), ["ctrl-alt-x"]);
        assert_eq!(windows("x::Tab", "ctrl-tab"), ["ctrl-tab"]);
        assert_eq!(windows("", "alt-cmd-shift-t"), ["ctrl-alt-shift-t"]);
        // Another action with the same macOS keys goes by rule.
        assert_eq!(windows("input::Paste", "cmd-v"), ["ctrl-v"]);
    }

    #[test]
    fn windows_keeps_its_own_keys() {
        for keys in [
            "ctrl-escape",
            "ctrl-shift-escape",
            "alt-tab",
            "alt-f4",
            "alt-space",
            "f10",
            "cmd-e",
            "win-d",
        ] {
            assert!(reserved_on_windows(keys), "{keys}");
        }
        for keys in [
            "ctrl-alt-c",
            "shift-f10",
            "ctrl-f4",
            "alt-f12",
            "ctrl-shift-tab",
        ] {
            assert!(!reserved_on_windows(keys), "{keys}");
        }
    }

    #[test]
    fn cmd_click_is_ctrl_click_on_windows() {
        let modifiers = |keys: &str| gpui::Keystroke::parse(keys).unwrap().modifiers;
        assert!(secondary_alone(KeyOs::Mac, &modifiers("cmd-a")));
        assert!(!secondary_alone(KeyOs::Mac, &modifiers("ctrl-a")));
        assert!(!secondary_alone(KeyOs::Mac, &modifiers("ctrl-cmd-a")));
        assert!(secondary_alone(KeyOs::Windows, &modifiers("ctrl-a")));
        assert!(!secondary_alone(KeyOs::Windows, &modifiers("ctrl-shift-a")));
        assert!(!secondary_alone(KeyOs::Windows, &modifiers("alt-a")));
    }

    #[test]
    fn mac_labels_read_the_windows_way() {
        let windows = |mac| label(KeyOs::Windows, mac).into_owned();
        assert_eq!(windows("⇧⌘F"), "Ctrl+Shift+F");
        assert_eq!(windows("⌘↵"), "Ctrl+Enter");
        assert_eq!(windows("⌥⇧↑"), "Alt+Shift+↑");
        assert_eq!(windows("⇧↑↓"), "Shift+↑↓");
        assert_eq!(windows("⇥"), "Tab");
        assert_eq!(windows("esc"), "Esc");
        assert_eq!(windows("⌃⌘→"), "Ctrl+→");
        assert_eq!(windows("⌘K ⌘S"), "Ctrl+K Ctrl+S");
        assert_eq!(windows("F7"), "F7");
        // Already Windows notation: as it is.
        assert_eq!(windows("Ctrl+Shift+P"), "Ctrl+Shift+P");
        assert_eq!(label(KeyOs::Mac, "⇧⌘F"), "⇧⌘F");
    }

    #[test]
    fn keys_read_as_labels_of_each_system() {
        assert_eq!(keys_label(KeyOs::Mac, "cmd-shift-p"), "⇧⌘P");
        assert_eq!(keys_label(KeyOs::Windows, "ctrl-shift-p"), "Ctrl+Shift+P");
        assert_eq!(keys_label(KeyOs::Windows, "ctrl-k ctrl-s"), "Ctrl+K Ctrl+S");
    }

    /// A binding of Flux's sources: its file, macOS keys, action (as written) and key context
    /// (the argument as written).
    struct Source {
        file: String,
        keys: String,
        action: String,
        context: String,
        namespace: Option<String>,
    }

    /// Every `KeyBinding::new("keys", Action, context)` of Flux's sources, with the namespace of
    /// the action where the file declares it (`actions!(namespace, [...])`) or names it (`a::B`).
    fn sources() -> Vec<Source> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = vec![root];
        let mut found = Vec::new();
        while let Some(path) = files.pop() {
            if path.is_dir() {
                for entry in std::fs::read_dir(&path).unwrap() {
                    files.push(entry.unwrap().path());
                }
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let declared = declared_actions(&text);
            let file = path.file_name().unwrap().to_string_lossy().to_string();
            for (start, _) in text.match_indices("KeyBinding::new(") {
                let rest = text[start + "KeyBinding::new(".len()..].trim_start();
                let Some(rest) = rest.strip_prefix('"') else {
                    continue;
                };
                let Some((keys, rest)) = rest.split_once('"') else {
                    continue;
                };
                let rest = rest.trim_start().trim_start_matches(',').trim_start();
                let action: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
                    .collect();
                let after = rest[action.len()..].trim_start();
                let after = match after.strip_prefix('(') {
                    // An action with a value: `ActivateTab(n - 1)`.
                    Some(inner) => inner.split_once(')').map_or("", |(_, tail)| tail),
                    None => after,
                };
                let context: String = after
                    .trim_start()
                    .trim_start_matches(',')
                    .trim_start()
                    .split([')', ','])
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let short = action.rsplit("::").next().unwrap_or(&action);
                let namespace = match action.rsplit_once("::") {
                    Some((path, _)) => path.rsplit("::").next().map(str::to_string),
                    None => declared
                        .iter()
                        .find(|(_, name)| name == short)
                        .map(|(namespace, _)| namespace.clone()),
                };
                found.push(Source {
                    file: file.clone(),
                    keys: keys.to_string(),
                    action,
                    context,
                    namespace,
                });
            }
        }
        found
    }

    /// The (namespace, name) pairs of a file's `actions!` declarations.
    fn declared_actions(text: &str) -> Vec<(String, String)> {
        let mut found = Vec::new();
        for (start, _) in text.match_indices("actions!(") {
            let rest = &text[start + "actions!(".len()..];
            let Some((namespace, rest)) = rest.split_once(',') else {
                continue;
            };
            let Some((list, _)) = rest.split_once(']') else {
                continue;
            };
            for line in list.trim_start().trim_start_matches('[').lines() {
                let line = line.trim();
                if line.starts_with("//") {
                    continue;
                }
                for name in line.split(',') {
                    let name = name.trim();
                    if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        found.push((namespace.trim().to_string(), name.to_string()));
                    }
                }
            }
        }
        found
    }

    fn full_name(source: &Source) -> String {
        let short = source.action.rsplit("::").next().unwrap_or(&source.action);
        match (&source.namespace, short) {
            (_, "NoAction") => "zed::NoAction".to_string(),
            (Some(namespace), short) => format!("{namespace}::{short}"),
            (None, short) => short.to_string(),
        }
    }

    #[test]
    fn the_sources_are_read() {
        let sources = sources();
        assert!(sources.len() > 500, "{}", sources.len());
        let delete_line = sources
            .iter()
            .find(|source| source.action == "DeleteLine")
            .unwrap();
        assert_eq!(full_name(delete_line), "editor::DeleteLine");
        assert_eq!(delete_line.keys, "cmd-backspace");
        assert_eq!(delete_line.context, "context");
    }

    /// No Windows binding of Flux holds the Windows key or a key Windows keeps for itself, each
    /// parses, and every entry of the table is a binding of Flux (a renamed action or changed
    /// macOS keys would leave the entry behind).
    #[test]
    fn windows_bindings_are_clean() {
        let sources = sources();
        for source in &sources {
            for keys in windows_keys(&full_name(source), &source.keys) {
                assert!(
                    !reserved_on_windows(&keys) && !keys.contains("cmd"),
                    "{} {}: {} → {keys}",
                    source.file,
                    source.action,
                    source.keys
                );
                for keystroke in keys.split_whitespace() {
                    assert!(gpui::Keystroke::parse(keystroke).is_ok(), "{keys}");
                }
            }
        }
        for (action, mac, _) in WINDOWS_KEYS {
            assert!(
                sources.iter().any(|source| full_name(source) == *action
                    && parse(&source.keys) == parse(mac)),
                "{action} {mac} is not bound in the sources"
            );
        }
    }

    /// Two actions of one key context don't get the same Windows keys unless their macOS keys were
    /// the same as well (as `enter` on a list and in its input).
    #[test]
    fn windows_bindings_dont_collide() {
        use std::collections::HashMap;
        let sources = sources();
        let mut seen: HashMap<(String, String), (String, String)> = HashMap::new();
        for source in &sources {
            // A literal context is shared by every file; a variable is the file's own.
            let context = if source.context.starts_with("Some(\"") || source.context == "None" {
                source.context.clone()
            } else {
                format!("{}:{}", source.file, source.context)
            };
            for keys in windows_keys(&full_name(source), &source.keys) {
                let keys = parse(&keys).unwrap();
                let keys: Vec<String> = keys.iter().map(Stroke::unparse).collect();
                let key = (context.clone(), keys.join(" "));
                if let Some((action, mac)) = seen.get(&key) {
                    assert!(
                        action == &source.action || parse(mac) == parse(&source.keys),
                        "{}: {} ({}) and {} ({}) → {}",
                        context,
                        action,
                        mac,
                        source.action,
                        source.keys,
                        key.1
                    );
                }
                seen.insert(key, (source.action.clone(), source.keys.clone()));
            }
        }
    }
}
