//! The window's terminals, as the manifest's `terminal` permission allows: a tab with a command
//! (or the user's shell), text typed into it, `Event::TerminalExited((id, code))` when the command
//! ends (its tab stays, with the exit code) and `Event::TerminalClosed(id)` when the tab closes.
//! The tab is the user's like any other.
//!
//! ```ignore
//! let tests = terminal::command(["cargo", "test"]).title("Tests").env("RUST_BACKTRACE", "1").open()?;
//! let shell = terminal::shell().in_editor().focus().open()?;
//! terminal::run_line(shell, "git status")?;
//! ```

use crate::host::terminal as raw;
pub use crate::host::terminal::TerminalLocation;

/// A terminal tab to open: [`shell`] or [`command`], then the methods add to it.
#[derive(Debug, Clone)]
pub struct Terminal(raw::TerminalOptions);

fn options(command: Option<Vec<String>>) -> Terminal {
    Terminal(raw::TerminalOptions {
        title: None,
        command,
        cwd: None,
        env: Vec::new(),
        location: TerminalLocation::Panel,
        focus: false,
    })
}

/// The user's shell.
pub fn shell() -> Terminal {
    options(None)
}

/// A command: the program and its arguments. Its tab stays after it ends, with the exit code.
pub fn command<I, S>(command: I) -> Terminal
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    options(Some(
        command
            .into_iter()
            .map(|part| part.as_ref().to_string())
            .collect(),
    ))
}

impl Terminal {
    /// The tab's title (the command's name by default).
    pub fn title(mut self, title: &str) -> Self {
        self.0.title = Some(title.to_string());
        self
    }

    /// The starting folder (the project root by default).
    pub fn cwd(mut self, dir: &str) -> Self {
        self.0.cwd = Some(dir.to_string());
        self
    }

    /// A variable added to the environment.
    pub fn env(mut self, name: &str, value: &str) -> Self {
        self.0.env.push((name.to_string(), value.to_string()));
        self
    }

    /// Among the editor's tabs instead of the terminal panel at the bottom.
    pub fn in_editor(mut self) -> Self {
        self.0.location = TerminalLocation::Editor;
        self
    }

    /// The terminal takes the keyboard (by default the keyboard stays where the user is).
    pub fn focus(mut self) -> Self {
        self.0.focus = true;
        self
    }

    /// Opens the tab; returns the terminal's id.
    pub fn open(&self) -> Result<u64, String> {
        raw::open(&self.0)
    }

    /// The options as the API has them.
    pub fn as_raw(&self) -> &raw::TerminalOptions {
        &self.0
    }
}

/// Types text into the terminal as if the user did ("\r" presses Enter).
pub fn send_text(terminal: u64, text: &str) -> Result<(), String> {
    raw::send_text(terminal, text)
}

/// Types a line and presses Enter: runs it in the terminal's shell.
pub fn run_line(terminal: u64, line: &str) -> Result<(), String> {
    raw::send_text(terminal, &format!("{line}\r"))
}

/// Brings the terminal's tab forward; `focus` — it takes the keyboard.
pub fn show(terminal: u64, focus: bool) -> Result<(), String> {
    raw::show(terminal, focus)
}

/// Closes the terminal's tab: what runs in it ends.
pub fn close(terminal: u64) {
    raw::close(terminal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_options() {
        let tests = command(["cargo", "test"]).title("Tests").env("A", "1").in_editor().focus();
        let raw = tests.as_raw();
        assert_eq!(raw.command.as_deref(), Some(&["cargo".to_string(), "test".to_string()][..]));
        assert_eq!(raw.title.as_deref(), Some("Tests"));
        assert_eq!(raw.location, TerminalLocation::Editor);
        assert!(raw.focus);
        assert_eq!(shell().as_raw().command, None);
        assert_eq!(shell().as_raw().location, TerminalLocation::Panel);
    }
}
