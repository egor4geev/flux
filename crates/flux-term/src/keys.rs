//! Keystrokes → the bytes a terminal program expects, by xterm conventions.
//!
//! Only keys that are not plain text are encoded here: Enter, Backspace, Tab, Escape, arrows,
//! Home/End, PageUp/PageDown, Insert/Delete, F-keys, control combinations, and the macOS
//! conveniences. Plain characters, including those typed with Option, arrive from the input method
//! as text and are written as UTF-8 by the UI.
//!
//! macOS conveniences on the shell's line (not on the alternate screen, where full-screen programs
//! get the real keys): ⌥←/⌥→ — a word back and forward (`ESC b`, `ESC f`), ⌘←/⌘→ — the start and the
//! end of the line (`^A`, `^E`), ⌥⌫ / ⌥⌦ — delete a word back / forward (`ESC DEL`, `ESC d`), ⌘⌫ /
//! ⌘⌦ — delete to the start / the end of the line (`^U`, `^K`), ⇧↵ — a new line without running the
//! command (`ESC CR`).
//!
//! The kitty keyboard protocol is off (alacritty's default): programs that ask for it get the
//! legacy encoding above.

use crate::content::TermMode;

/// Modifier keys held with a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    /// ⌘: never sent to the program; a few ⌘ combinations map to line editing keys.
    pub command: bool,
}

impl Modifiers {
    /// The xterm modifier parameter: 1 + shift + 2·alt + 4·control; 1 means none.
    fn xterm(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.control)
    }

    fn any(self) -> bool {
        self.shift || self.alt || self.control
    }
}

/// The bytes for `key` (a gpui key name: `enter`, `left`, `f5`, `a`) with `modifiers`, in the
/// current terminal `mode`. `None` — the key is text (the input method delivers it) or not meant for
/// the terminal (a ⌘ shortcut).
pub fn encode(key: &str, modifiers: Modifiers, mode: TermMode) -> Option<Vec<u8>> {
    let shell_line = !mode.alt_screen;
    if modifiers.command {
        return match key {
            "left" if shell_line => Some(b"\x01".to_vec()),
            "right" if shell_line => Some(b"\x05".to_vec()),
            "left" => Some(cursor_key(b'H', Modifiers::default(), mode)),
            "right" => Some(cursor_key(b'F', Modifiers::default(), mode)),
            "backspace" => Some(b"\x15".to_vec()),
            "delete" if shell_line => Some(b"\x0b".to_vec()),
            _ => None,
        };
    }
    let bytes = match key {
        "enter" => {
            if modifiers.alt || (modifiers.shift && shell_line) {
                b"\x1b\r".to_vec()
            } else if mode.line_feed_new_line {
                b"\r\n".to_vec()
            } else {
                b"\r".to_vec()
            }
        }
        "tab" if modifiers.shift => b"\x1b[Z".to_vec(),
        "tab" => b"\t".to_vec(),
        "backspace" if modifiers.control => b"\x08".to_vec(),
        "backspace" if modifiers.alt => b"\x1b\x7f".to_vec(),
        "backspace" => b"\x7f".to_vec(),
        "escape" if modifiers.alt => b"\x1b\x1b".to_vec(),
        "escape" => b"\x1b".to_vec(),
        "space" if modifiers.control => b"\x00".to_vec(),
        // ⌥Space types a no-break space on macOS, which breaks commands typed in a hurry.
        "space" if modifiers.alt => b" ".to_vec(),
        "left" if modifiers.alt && !modifiers.shift && !modifiers.control && shell_line => {
            b"\x1bb".to_vec()
        }
        "right" if modifiers.alt && !modifiers.shift && !modifiers.control && shell_line => {
            b"\x1bf".to_vec()
        }
        "up" => cursor_key(b'A', modifiers, mode),
        "down" => cursor_key(b'B', modifiers, mode),
        "right" => cursor_key(b'C', modifiers, mode),
        "left" => cursor_key(b'D', modifiers, mode),
        "home" => cursor_key(b'H', modifiers, mode),
        "end" => cursor_key(b'F', modifiers, mode),
        "delete" if modifiers.alt && !modifiers.shift && !modifiers.control && shell_line => {
            b"\x1bd".to_vec()
        }
        "insert" => tilde_key(2, modifiers),
        "delete" => tilde_key(3, modifiers),
        "pageup" => tilde_key(5, modifiers),
        "pagedown" => tilde_key(6, modifiers),
        _ => return function_key(key, modifiers).or_else(|| control_key(key, modifiers)),
    };
    Some(bytes)
}

/// Arrows, Home, End: `ESC [ A`, or `ESC O A` in application cursor mode; with modifiers
/// `ESC [ 1 ; m A`.
fn cursor_key(letter: u8, modifiers: Modifiers, mode: TermMode) -> Vec<u8> {
    if modifiers.any() {
        format!("\x1b[1;{}{}", modifiers.xterm(), letter as char).into_bytes()
    } else if mode.app_cursor {
        vec![0x1b, b'O', letter]
    } else {
        vec![0x1b, b'[', letter]
    }
}

/// `ESC [ code ~`, with modifiers `ESC [ code ; m ~`.
fn tilde_key(code: u8, modifiers: Modifiers) -> Vec<u8> {
    if modifiers.any() {
        format!("\x1b[{code};{}~", modifiers.xterm()).into_bytes()
    } else {
        format!("\x1b[{code}~").into_bytes()
    }
}

/// F1–F4 are `ESC O P…S` (with modifiers `ESC [ 1 ; m P`), F5–F20 use `~` codes.
fn function_key(key: &str, modifiers: Modifiers) -> Option<Vec<u8>> {
    let number: u8 = key.strip_prefix('f')?.parse().ok()?;
    match number {
        1..=4 => {
            let letter = b'P' + number - 1;
            Some(if modifiers.any() {
                format!("\x1b[1;{}{}", modifiers.xterm(), letter as char).into_bytes()
            } else {
                vec![0x1b, b'O', letter]
            })
        }
        5..=20 => {
            const CODES: [u8; 16] = [
                15, 17, 18, 19, 20, 21, 23, 24, 25, 26, 28, 29, 31, 32, 33, 34,
            ];
            Some(tilde_key(CODES[(number - 5) as usize], modifiers))
        }
        _ => None,
    }
}

/// ⌃ with a letter or a symbol: the C0 control character; with ⌥ as well, prefixed with ESC.
fn control_key(key: &str, modifiers: Modifiers) -> Option<Vec<u8>> {
    if !modifiers.control {
        return None;
    }
    let mut chars = key.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let byte = match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | '2' => 0x00,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '-' | '/' | '7' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    };
    Some(if modifiers.alt {
        vec![0x1b, byte]
    } else {
        vec![byte]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(control: bool, alt: bool, shift: bool, command: bool) -> Modifiers {
        Modifiers {
            control,
            alt,
            shift,
            command,
        }
    }

    fn key(name: &str, modifiers: Modifiers) -> Option<Vec<u8>> {
        encode(name, modifiers, TermMode::default())
    }

    #[test]
    fn plain_keys() {
        let none = Modifiers::default();
        assert_eq!(key("enter", none).unwrap(), b"\r");
        assert_eq!(key("backspace", none).unwrap(), b"\x7f");
        assert_eq!(key("tab", none).unwrap(), b"\t");
        assert_eq!(key("escape", none).unwrap(), b"\x1b");
        assert_eq!(key("up", none).unwrap(), b"\x1b[A");
        assert_eq!(key("f1", none).unwrap(), b"\x1bOP");
        assert_eq!(key("f5", none).unwrap(), b"\x1b[15~");
        assert_eq!(key("a", none), None);
        assert_eq!(key("space", none), None);
    }

    #[test]
    fn application_cursor_mode_uses_ss3() {
        let mode = TermMode {
            app_cursor: true,
            ..Default::default()
        };
        assert_eq!(
            encode("left", Modifiers::default(), mode).unwrap(),
            b"\x1bOD"
        );
    }

    #[test]
    fn control_combinations() {
        let ctrl = mods(true, false, false, false);
        assert_eq!(key("c", ctrl).unwrap(), b"\x03");
        assert_eq!(key("d", ctrl).unwrap(), b"\x04");
        assert_eq!(key("[", ctrl).unwrap(), b"\x1b");
        assert_eq!(key("space", ctrl).unwrap(), b"\x00");
        assert_eq!(key("up", ctrl).unwrap(), b"\x1b[1;5A");
    }

    #[test]
    fn modifiers_on_special_keys() {
        let shift = mods(false, false, true, false);
        let ctrl_alt = mods(true, true, false, false);
        let alt = mods(false, true, false, false);
        assert_eq!(key("tab", shift).unwrap(), b"\x1b[Z");
        assert_eq!(key("delete", Modifiers::default()).unwrap(), b"\x1b[3~");
        assert_eq!(key("pageup", shift).unwrap(), b"\x1b[5;2~");
        assert_eq!(key("home", Modifiers::default()).unwrap(), b"\x1b[H");
        assert_eq!(key("end", shift).unwrap(), b"\x1b[1;2F");
        assert_eq!(key("f4", shift).unwrap(), b"\x1b[1;2S");
        assert_eq!(key("f12", ctrl_alt).unwrap(), b"\x1b[24;7~");
        assert_eq!(key("f13", Modifiers::default()).unwrap(), b"\x1b[25~");
        assert_eq!(key("f20", Modifiers::default()).unwrap(), b"\x1b[34~");
        assert_eq!(key("f21", Modifiers::default()), None);
        assert_eq!(key("escape", alt).unwrap(), b"\x1b\x1b");
        assert_eq!(key("enter", alt).unwrap(), b"\x1b\r");
        let lnm = TermMode {
            line_feed_new_line: true,
            ..Default::default()
        };
        assert_eq!(encode("enter", Modifiers::default(), lnm).unwrap(), b"\r\n");
    }

    #[test]
    fn control_with_shift_and_alt() {
        assert_eq!(key("a", mods(true, false, true, false)).unwrap(), b"\x01");
        assert_eq!(
            key("r", mods(true, true, false, false)).unwrap(),
            b"\x1b\x12"
        );
        assert_eq!(
            key("backspace", mods(true, false, false, false)).unwrap(),
            b"\x08"
        );
        assert_eq!(
            key("enter", mods(true, false, false, false)).unwrap(),
            b"\r"
        );
        assert_eq!(key("2", mods(true, false, false, false)).unwrap(), b"\x00");
        assert_eq!(key("-", mods(true, false, false, false)).unwrap(), b"\x1f");
        assert_eq!(key("/", mods(true, false, false, false)).unwrap(), b"\x1f");
        // ⌥ with a letter is text: the input method types what the layout gives.
        assert_eq!(key("s", mods(false, true, false, false)), None);
    }

    #[test]
    fn macos_line_editing_on_the_shell_line() {
        let alt = mods(false, true, false, false);
        let cmd = mods(false, false, false, true);
        assert_eq!(key("left", alt).unwrap(), b"\x1bb");
        assert_eq!(key("right", alt).unwrap(), b"\x1bf");
        assert_eq!(key("left", cmd).unwrap(), b"\x01");
        assert_eq!(key("backspace", cmd).unwrap(), b"\x15");
        assert_eq!(key("backspace", alt).unwrap(), b"\x1b\x7f");
        assert_eq!(key("delete", alt).unwrap(), b"\x1bd");
        assert_eq!(key("delete", cmd).unwrap(), b"\x0b");
        assert_eq!(key("p", cmd), None);
        // ⇧↵ adds a line to the command instead of running it.
        let shift = mods(false, false, true, false);
        assert_eq!(key("enter", shift).unwrap(), b"\x1b\r");
        let full_screen = TermMode {
            alt_screen: true,
            ..Default::default()
        };
        assert_eq!(encode("left", alt, full_screen).unwrap(), b"\x1b[1;3D");
        assert_eq!(encode("left", cmd, full_screen).unwrap(), b"\x1b[H");
        assert_eq!(encode("delete", cmd, full_screen), None);
        assert_eq!(encode("delete", alt, full_screen).unwrap(), b"\x1b[3;3~");
        assert_eq!(encode("enter", shift, full_screen).unwrap(), b"\r");
    }
}
