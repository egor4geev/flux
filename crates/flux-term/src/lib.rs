//! Terminal emulation without UI.
//!
//! A [`Terminal`] runs the user's shell on a pseudoterminal and keeps its screen: alacritty_terminal
//! parses the output on a background thread into a grid with scrollback. The UI draws a [`Content`]
//! snapshot of the visible rows and sends input bytes back; events (new output, title, bell, exit)
//! come out as a `futures` stream, so any executor (gpui in flux-app) can await them.
//!
//! Pure helpers for the UI: [`keys`] turns keystrokes into the bytes a terminal program expects,
//! [`mouse`] encodes mouse reports, [`links`] finds URLs and file locations in a line of output.

pub mod content;
pub mod keys;
pub mod links;
pub mod mouse;
mod process;
mod search;
mod shell;
mod terminal;

pub use content::{
    CellFlags, Content, Cursor, CursorShape, GridPoint, MouseMode, Palette, RenderCell, Rgb,
    SelectionBounds, TermMode, Underline,
};
pub use links::{Link, LinkTarget};
pub use search::{SearchError, SearchMatch, SearchOptions, SearchResults};
pub use shell::login_shell;
pub use terminal::{
    ProcessInfo, ScrollDelta, SelectionKind, Side, TermSize, Terminal, TerminalEvent,
    TerminalEvents, TerminalOptions,
};
