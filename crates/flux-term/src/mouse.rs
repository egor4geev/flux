//! Mouse reports for programs that asked for them (vim with `mouse=a`, htop, tmux), and the wheel
//! on the alternate screen.

use crate::content::{MouseMode, TermMode};
use crate::keys::Modifiers;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

impl MouseButton {
    fn is_wheel(self) -> bool {
        matches!(self, MouseButton::WheelUp | MouseButton::WheelDown)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    /// Motion; with `button` set — dragging with that button held.
    Move,
}

/// A mouse event at a cell of the viewport (0-based column and row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub button: Option<MouseButton>,
    pub action: MouseAction,
    pub column: usize,
    pub row: usize,
    pub modifiers: Modifiers,
}

/// The largest coordinate the X10 encoding can carry in a byte (255 − 32); the UTF-8 one goes to
/// 2015.
const X10_MAX: usize = 223;
const UTF8_MAX: usize = 2015;

/// Whether the program receives this event instead of the terminal handling it itself (selection,
/// scrollback). Holding ⇧ keeps the mouse for selection, as in xterm.
pub fn is_reported(event: &MouseEvent, mode: TermMode) -> bool {
    if mode.mouse == MouseMode::None || event.modifiers.shift {
        return false;
    }
    match (event.action, event.button) {
        (MouseAction::Move, None) => mode.mouse == MouseMode::Motion,
        (MouseAction::Move, Some(_)) => matches!(mode.mouse, MouseMode::Drag | MouseMode::Motion),
        (MouseAction::Release, Some(button)) => !button.is_wheel(),
        (_, _) => true,
    }
}

/// The report: SGR (`ESC [ < b ; x ; y M` / `m`) when the program enabled it, otherwise the X10 form
/// (`ESC [ M b x y`, UTF-8 coordinates in mode 1005). `None` if the current mode doesn't report this
/// event or the position can't be encoded.
pub fn encode(event: &MouseEvent, mode: TermMode) -> Option<Vec<u8>> {
    if !is_reported(event, mode) {
        return None;
    }
    let modifiers = 4 * u32::from(event.modifiers.shift)
        + 8 * u32::from(event.modifiers.alt)
        + 16 * u32::from(event.modifiers.control);
    let motion = if event.action == MouseAction::Move {
        32
    } else {
        0
    };
    let button = match event.button {
        Some(MouseButton::Left) => 0,
        Some(MouseButton::Middle) => 1,
        Some(MouseButton::Right) => 2,
        Some(MouseButton::WheelUp) => 64,
        Some(MouseButton::WheelDown) => 65,
        // Motion without a button.
        None => 3,
    };
    let (x, y) = (event.column + 1, event.row + 1);

    if mode.mouse_sgr {
        let code = button + modifiers + motion;
        let end = if event.action == MouseAction::Release {
            'm'
        } else {
            'M'
        };
        return Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes());
    }

    // X10 can't tell which button was released: release is button 3.
    let button = if event.action == MouseAction::Release {
        3
    } else {
        button
    };
    let mut bytes = b"\x1b[M".to_vec();
    bytes.push((32 + button + modifiers + motion) as u8);
    for coordinate in [x, y] {
        if mode.mouse_utf8 {
            if coordinate > UTF8_MAX {
                return None;
            }
            let c = char::from_u32((32 + coordinate) as u32)?;
            let mut buffer = [0; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
        } else {
            if coordinate > X10_MAX {
                return None;
            }
            bytes.push((32 + coordinate) as u8);
        }
    }
    Some(bytes)
}

/// The wheel on the alternate screen when the program doesn't take the mouse: arrow keys instead
/// (less, man, git log). `lines` > 0 scrolls up. `None` when the wheel should scroll the
/// scrollback instead.
pub fn alternate_scroll(lines: i32, mode: TermMode) -> Option<Vec<u8>> {
    if lines == 0 || !mode.alt_screen || !mode.alternate_scroll || mode.mouse != MouseMode::None {
        return None;
    }
    let arrow: &[u8] = match (lines > 0, mode.app_cursor) {
        (true, true) => b"\x1bOA",
        (true, false) => b"\x1b[A",
        (false, true) => b"\x1bOB",
        (false, false) => b"\x1b[B",
    };
    Some(arrow.repeat(lines.unsigned_abs() as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(
        button: Option<MouseButton>,
        action: MouseAction,
        column: usize,
        row: usize,
    ) -> MouseEvent {
        MouseEvent {
            button,
            action,
            column,
            row,
            modifiers: Modifiers::default(),
        }
    }

    fn mode(mouse: MouseMode, sgr: bool) -> TermMode {
        TermMode {
            mouse,
            mouse_sgr: sgr,
            ..TermMode::default()
        }
    }

    #[test]
    fn nothing_is_reported_without_a_mouse_mode() {
        let press = event(Some(MouseButton::Left), MouseAction::Press, 0, 0);
        assert!(!is_reported(&press, TermMode::default()));
        assert_eq!(encode(&press, TermMode::default()), None);
    }

    #[test]
    fn shift_keeps_the_mouse_for_selection() {
        let mut press = event(Some(MouseButton::Left), MouseAction::Press, 0, 0);
        press.modifiers.shift = true;
        assert!(!is_reported(&press, mode(MouseMode::Click, true)));
    }

    #[test]
    fn motion_depends_on_the_mode() {
        let drag = event(Some(MouseButton::Left), MouseAction::Move, 1, 1);
        let hover = event(None, MouseAction::Move, 1, 1);
        assert!(!is_reported(&drag, mode(MouseMode::Click, true)));
        assert!(is_reported(&drag, mode(MouseMode::Drag, true)));
        assert!(!is_reported(&hover, mode(MouseMode::Drag, true)));
        assert!(is_reported(&hover, mode(MouseMode::Motion, true)));
    }

    #[test]
    fn sgr_reports() {
        let sgr = mode(MouseMode::Drag, true);
        let press = event(Some(MouseButton::Left), MouseAction::Press, 4, 9);
        assert_eq!(encode(&press, sgr).unwrap(), b"\x1b[<0;5;10M");
        let release = event(Some(MouseButton::Left), MouseAction::Release, 4, 9);
        assert_eq!(encode(&release, sgr).unwrap(), b"\x1b[<0;5;10m");
        let drag = event(Some(MouseButton::Right), MouseAction::Move, 0, 0);
        assert_eq!(encode(&drag, sgr).unwrap(), b"\x1b[<34;1;1M");
        let mut wheel = event(Some(MouseButton::WheelDown), MouseAction::Press, 299, 0);
        wheel.modifiers.control = true;
        assert_eq!(encode(&wheel, sgr).unwrap(), b"\x1b[<81;300;1M");
        // The wheel has no release.
        let wheel_up = event(Some(MouseButton::WheelUp), MouseAction::Release, 0, 0);
        assert_eq!(encode(&wheel_up, sgr), None);
    }

    #[test]
    fn x10_reports() {
        let x10 = mode(MouseMode::Click, false);
        let mut press = event(Some(MouseButton::Middle), MouseAction::Press, 0, 2);
        press.modifiers.alt = true;
        assert_eq!(
            encode(&press, x10).unwrap(),
            [0x1b, b'[', b'M', 32 + 1 + 8, 33, 35]
        );
        let release = event(Some(MouseButton::Middle), MouseAction::Release, 0, 2);
        assert_eq!(
            encode(&release, x10).unwrap(),
            [0x1b, b'[', b'M', 32 + 3, 33, 35]
        );
        // Too far right for a byte.
        let far = event(Some(MouseButton::Left), MouseAction::Press, 223, 0);
        assert_eq!(encode(&far, x10), None);
        let utf8 = TermMode {
            mouse_utf8: true,
            ..x10
        };
        let bytes = encode(&far, utf8).unwrap();
        assert_eq!(&bytes[..4], [0x1b, b'[', b'M', 32]);
        // Column 224 → 256 → two bytes of UTF-8.
        assert_eq!(&bytes[4..], [0xc4, 0x80, 33]);
    }

    #[test]
    fn the_wheel_on_the_alternate_screen_sends_arrows() {
        let less = TermMode {
            alt_screen: true,
            alternate_scroll: true,
            ..TermMode::default()
        };
        assert_eq!(alternate_scroll(2, less).unwrap(), b"\x1b[A\x1b[A");
        assert_eq!(alternate_scroll(-1, less).unwrap(), b"\x1b[B");
        let app_cursor = TermMode {
            app_cursor: true,
            ..less
        };
        assert_eq!(alternate_scroll(1, app_cursor).unwrap(), b"\x1bOA");
        // On the main screen, or when the program takes the mouse, the wheel isn't arrows.
        assert_eq!(alternate_scroll(1, TermMode::default()), None);
        let vim = TermMode {
            mouse: MouseMode::Click,
            ..less
        };
        assert_eq!(alternate_scroll(1, vim), None);
        assert_eq!(alternate_scroll(0, less), None);
    }
}
