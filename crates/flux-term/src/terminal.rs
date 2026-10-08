//! A terminal session: the shell on a pseudoterminal, the screen it draws on, and the I/O thread
//! between them.
//!
//! alacritty_terminal's event loop owns the PTY: on its own thread it reads the output, parses it
//! into the [`Term`] (behind a fair mutex shared with us), and writes our input. It reports through
//! [`Listener`], which turns its events into [`TerminalEvent`]s and answers the program's queries
//! (colors, text area size) itself. The UI locks the term only briefly: to take a [`Content`]
//! snapshot, to change the selection or the scroll.

use std::borrow::Cow;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, ready};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side as GridSide};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Term, TermMode as AlacrittyMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{
    ClearMode, Color, CursorShape as AnsiCursorShape, Handler, NamedColor,
};
use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use futures::{Stream, StreamExt};

use crate::content::{
    CellFlags, Content, Cursor, CursorShape, GridPoint, Palette, RenderCell, Rgb, SelectionBounds,
    TermMode,
};
use crate::links::{self, Link};
use crate::search::{self, SearchError, SearchOptions, SearchResults};
use crate::{process, shell};

/// Lines of scrollback by default.
const SCROLLBACK: usize = 10_000;

/// The size of the terminal: the grid in cells and one cell in pixels (programs may ask for the
/// latter, and the kernel passes it on with the window size).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermSize {
    pub columns: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl Default for TermSize {
    fn default() -> Self {
        Self {
            columns: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        }
    }
}

impl TermSize {
    fn window_size(self) -> WindowSize {
        WindowSize {
            num_lines: self.rows,
            num_cols: self.columns,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
        }
    }
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.rows).max(alacritty_terminal::term::MIN_SCREEN_LINES)
    }

    fn columns(&self) -> usize {
        usize::from(self.columns).max(alacritty_terminal::term::MIN_COLUMNS)
    }
}

/// How to start a terminal.
#[derive(Debug, Clone)]
pub struct TerminalOptions {
    /// The program and its arguments; `None` — the user's login shell.
    pub command: Option<(String, Vec<String>)>,
    /// The starting directory; `None` — Flux's own.
    pub cwd: Option<PathBuf>,
    /// Extra environment variables (on top of Flux's environment and the terminal's own).
    pub env: Vec<(String, String)>,
    pub size: TermSize,
    /// Lines of scrollback.
    pub scrollback: usize,
    pub palette: Palette,
}

impl Default for TerminalOptions {
    fn default() -> Self {
        Self {
            command: None,
            cwd: None,
            env: Vec::new(),
            size: TermSize::default(),
            scrollback: SCROLLBACK,
            palette: Palette::default(),
        }
    }
}

/// What happened in the terminal, for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    /// The screen changed: redraw. Coalesced: at most one is waiting until it is received.
    Wakeup,
    /// The program set the title (OSC 0, 2); `None` — reset it.
    Title(Option<String>),
    Bell,
    /// The program put text on the clipboard (OSC 52).
    Clipboard(String),
    /// The program exited; its code, if it exited normally. Nothing follows.
    Exit(Option<i32>),
}

/// Scrolling the view through the scrollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollDelta {
    /// Lines; positive is up, into the history.
    Lines(i32),
    PageUp,
    PageDown,
    Top,
    /// Back to the live screen.
    Bottom,
}

/// What a mouse selection takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    /// Cell by cell (a drag).
    Simple,
    /// Whole words (a double click).
    Word,
    /// Whole lines (a triple click).
    Line,
    /// A rectangle (⌥-drag).
    Block,
}

/// Which half of a cell the mouse is over: selecting from the right half of a cell starts after
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl From<Side> for GridSide {
    fn from(side: Side) -> Self {
        match side {
            Side::Left => GridSide::Left,
            Side::Right => GridSide::Right,
        }
    }
}

/// A process running in the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    /// "zsh", "cargo", "vim".
    pub name: String,
}

/// A running terminal. Dropping it shuts the I/O thread down and hangs up the shell (SIGHUP).
pub struct Terminal {
    term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
    shared: Arc<Shared>,
    /// A duplicate of the controlling side of the PTY: asks for its foreground process group.
    pty: OwnedFd,
    shell_pid: u32,
}

impl Terminal {
    /// Starts the program on a new PTY. Events come out of the returned stream until the program
    /// exits ([`TerminalEvent::Exit`]) or the terminal is dropped.
    pub fn spawn(options: TerminalOptions) -> io::Result<(Self, TerminalEvents)> {
        let (events, receiver) = mpsc::unbounded();
        let wakeup_pending = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared {
            events,
            wakeup_pending: wakeup_pending.clone(),
            sender: OnceLock::new(),
            palette: Mutex::new(options.palette.clone()),
            size: Mutex::new(options.size),
            exit_code: Mutex::new(None),
        });
        let listener = Listener(shared.clone());
        let config = Config {
            scrolling_history: options.scrollback,
            ..Config::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(
            config,
            &options.size,
            listener.clone(),
        )));

        let (program, args) = options.command.clone().unwrap_or_else(shell::login_shell);
        let pty_options = tty::Options {
            shell: Some(tty::Shell::new(program, args)),
            working_directory: options.cwd.clone(),
            drain_on_exit: false,
            env: shell::environment(&options.env),
        };
        let pty = tty::new(&pty_options, options.size.window_size(), 0)?;
        let shell_pid = pty.child().id();
        let pty_fd = OwnedFd::from(pty.file().try_clone()?);

        let event_loop = EventLoop::new(term.clone(), listener, pty, false, false)?;
        let sender = event_loop.channel();
        shared.sender.set(sender.clone()).ok();
        // The thread ends on Msg::Shutdown (sent on drop) or when the program exits.
        event_loop.spawn();

        let terminal = Self {
            term,
            sender,
            shared,
            pty: pty_fd,
            shell_pid,
        };
        let events = TerminalEvents {
            receiver,
            wakeup_pending,
        };
        Ok((terminal, events))
    }

    // --- Input ---

    /// Bytes to the program as they are: answers, mouse reports.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        if !bytes.is_empty() {
            let _ = self.sender.send(Msg::Input(bytes));
        }
    }

    /// Typed input (keys, text): like a key press in any terminal, it drops the selection and
    /// returns the view to the live screen.
    pub fn input(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        {
            let mut term = self.term.lock();
            term.selection = None;
            term.scroll_display(Scroll::Bottom);
        }
        self.write(bytes);
    }

    /// Pastes text: in bracketed paste mode wrapped in its markers (and without escape characters,
    /// so the text can't end the paste early); otherwise line breaks become Enter (CR).
    pub fn paste(&self, text: &str) {
        let bracketed = self
            .term
            .lock()
            .mode()
            .contains(AlacrittyMode::BRACKETED_PASTE);
        let text = if bracketed {
            format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', ""))
        } else {
            text.replace("\r\n", "\r").replace('\n', "\r")
        };
        self.input(text.into_bytes());
    }

    /// Tells the program about focus changes, if it asked (focus reporting mode).
    pub fn report_focus(&self, focused: bool) {
        if self.mode().focus_reporting {
            self.write(if focused {
                &b"\x1b[I"[..]
            } else {
                &b"\x1b[O"[..]
            });
        }
    }

    // --- Screen ---

    /// The new size of the grid; nothing happens if it is the same.
    pub fn resize(&self, size: TermSize) {
        {
            let mut current = self.shared.size.lock().unwrap();
            if *current == size {
                return;
            }
            *current = size;
        }
        self.term.lock().resize(size);
        let _ = self.sender.send(Msg::Resize(size.window_size()));
    }

    pub fn size(&self) -> TermSize {
        *self.shared.size.lock().unwrap()
    }

    /// The colors cells are resolved with (from the theme).
    pub fn set_palette(&self, palette: Palette) {
        *self.shared.palette.lock().unwrap() = palette;
    }

    pub fn mode(&self) -> TermMode {
        (*self.term.lock().mode()).into()
    }

    /// A snapshot of the visible rows.
    pub fn content(&self) -> Content {
        let palette = self.shared.palette.lock().unwrap().clone();
        let term = self.term.lock();
        let rows = term.screen_lines();
        let columns = term.columns();
        let history_size = term.history_size();
        let mode = (*term.mode()).into();
        let content = term.renderable_content();
        let display_offset = content.display_offset;
        let colors = content.colors;

        let mut cells = Vec::with_capacity(rows * columns / 4);
        for indexed in content.display_iter {
            let cell = indexed.cell;
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let (fg, bg) = cell_colors(cell, &palette, colors);
            let flags = CellFlags::from_cell(cell.flags);
            let c = if cell.flags.contains(Flags::HIDDEN) {
                ' '
            } else {
                cell.c
            };
            let zerowidth = cell
                .zerowidth()
                .filter(|chars| !chars.is_empty())
                .map(<[char]>::to_vec);
            if c == ' ' && bg.is_none() && flags.is_plain() && zerowidth.is_none() {
                continue;
            }
            let row = indexed.point.line.0 + display_offset as i32;
            cells.push(RenderCell {
                row: row as usize,
                column: indexed.point.column.0,
                c,
                zerowidth,
                fg,
                bg,
                flags,
            });
        }

        let cursor = {
            let point = content.cursor.point;
            let row = point.line.0 + display_offset as i32;
            let shape = match content.cursor.shape {
                AnsiCursorShape::Hidden => None,
                AnsiCursorShape::Underline => Some(CursorShape::Underline),
                AnsiCursorShape::Beam => Some(CursorShape::Beam),
                AnsiCursorShape::Block | AnsiCursorShape::HollowBlock => Some(CursorShape::Block),
            };
            shape
                .filter(|_| (0..rows as i32).contains(&row))
                .map(|shape| Cursor {
                    row: row as usize,
                    column: point.column.0,
                    shape,
                    wide: term.grid()[point].flags.contains(Flags::WIDE_CHAR),
                })
        };
        let selection = content.selection.map(|range| SelectionBounds {
            start: grid_point(range.start),
            end: grid_point(range.end),
            block: range.is_block,
        });

        Content {
            columns,
            rows,
            display_offset,
            history_size,
            cells,
            cursor,
            selection,
            mode,
        }
    }

    /// Scrolls the view through the scrollback.
    pub fn scroll(&self, delta: ScrollDelta) {
        let scroll = match delta {
            ScrollDelta::Lines(lines) => Scroll::Delta(lines),
            ScrollDelta::PageUp => Scroll::PageUp,
            ScrollDelta::PageDown => Scroll::PageDown,
            ScrollDelta::Top => Scroll::Top,
            ScrollDelta::Bottom => Scroll::Bottom,
        };
        self.term.lock().scroll_display(scroll);
    }

    /// Scrolls so that the line of `point` is visible (a search match).
    pub fn scroll_to(&self, point: GridPoint) {
        let mut term = self.term.lock();
        let point = clamp(&term, point);
        term.scroll_to_point(point);
    }

    /// Erases the scrollback (⌘K). While the shell waits for a command, the screen goes too: it is
    /// cleared here, and the shell redraws its prompt at the top on `^L` (its clear-screen key,
    /// which keeps what was typed). A running command keeps its screen; a full-screen program has
    /// no scrollback anyway.
    pub fn clear(&self) {
        let shell_waits = self.running_process().is_none();
        let mut term = self.term.lock();
        let main_screen = !term.mode().contains(AlacrittyMode::ALT_SCREEN);
        let redraw = shell_waits && main_screen;
        if redraw {
            // Clearing the main screen scrolls its lines into the history, so it goes first.
            term.clear_screen(ClearMode::All);
        }
        term.grid_mut().clear_history();
        term.selection = None;
        drop(term);
        if redraw {
            self.write(&b"\x0c"[..]);
        }
    }

    // --- Selection ---

    /// Starts a selection at `point` (a mouse press).
    pub fn start_selection(&self, kind: SelectionKind, point: GridPoint, side: Side) {
        let mut term = self.term.lock();
        let kind = match kind {
            SelectionKind::Simple => SelectionType::Simple,
            SelectionKind::Word => SelectionType::Semantic,
            SelectionKind::Line => SelectionType::Lines,
            SelectionKind::Block => SelectionType::Block,
        };
        let point = clamp(&term, point);
        term.selection = Some(Selection::new(kind, point, side.into()));
    }

    /// Moves the free end of the selection (dragging, ⇧-click).
    pub fn update_selection(&self, point: GridPoint, side: Side) {
        let mut term = self.term.lock();
        let point = clamp(&term, point);
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, side.into());
        }
    }

    pub fn clear_selection(&self) {
        self.term.lock().selection = None;
    }

    /// Selects the scrollback and the screen.
    pub fn select_all(&self) {
        let mut term = self.term.lock();
        let start = Point::new(term.topmost_line(), Column(0));
        let end = Point::new(term.bottommost_line(), term.last_column());
        let mut selection = Selection::new(SelectionType::Simple, start, GridSide::Left);
        selection.update(end, GridSide::Right);
        term.selection = Some(selection);
    }

    /// The selected text; `None` if nothing is selected.
    pub fn selection_text(&self) -> Option<String> {
        self.term
            .lock()
            .selection_to_string()
            .filter(|text| !text.is_empty())
    }

    // --- Search and links ---

    /// All matches of `query` in the scrollback and on the screen.
    pub fn search(
        &self,
        query: &str,
        options: SearchOptions,
    ) -> Result<SearchResults, SearchError> {
        search::search(&self.term.lock(), query, options)
    }

    /// The link under `point`: an OSC 8 hyperlink, a URL, or a file location in the line's text
    /// (lines wrapped by the terminal count as one).
    pub fn link_at(&self, point: GridPoint) -> Option<Link> {
        let term = self.term.lock();
        let point = clamp(&term, point);
        links::in_grid(&term, point)
    }

    // --- Processes ---

    /// The process id of the program the terminal started (the shell).
    pub fn shell_pid(&self) -> u32 {
        self.shell_pid
    }

    /// The process in the foreground: the shell itself while it waits for a command.
    pub fn foreground_process(&self) -> Option<ProcessInfo> {
        let pid = process::foreground_pid(self.pty.as_raw_fd())?;
        let name = process::name(pid)?;
        Some(ProcessInfo { pid, name })
    }

    /// The command running in the shell, if any: closing the terminal would kill it.
    pub fn running_process(&self) -> Option<ProcessInfo> {
        self.foreground_process()
            .filter(|process| process.pid != self.shell_pid)
    }

    /// The shell's current directory (it follows `cd`).
    pub fn cwd(&self) -> Option<PathBuf> {
        process::cwd(self.shell_pid)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}

/// The events of a terminal. Receiving a [`TerminalEvent::Wakeup`] re-arms it: until then, new
/// output doesn't queue another one, so a flood of output costs the UI one redraw per frame it
/// manages, not one per read.
pub struct TerminalEvents {
    receiver: UnboundedReceiver<TerminalEvent>,
    wakeup_pending: Arc<AtomicBool>,
}

impl Stream for TerminalEvents {
    type Item = TerminalEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<TerminalEvent>> {
        let event = ready!(self.receiver.poll_next_unpin(cx));
        if event == Some(TerminalEvent::Wakeup) {
            self.wakeup_pending.store(false, Ordering::Release);
        }
        Poll::Ready(event)
    }
}

/// State shared with the I/O thread through [`Listener`].
struct Shared {
    events: UnboundedSender<TerminalEvent>,
    /// A Wakeup is in the channel and hasn't been received yet.
    wakeup_pending: Arc<AtomicBool>,
    /// The event loop's input channel: answers to the program's queries go there.
    sender: OnceLock<EventLoopSender>,
    palette: Mutex<Palette>,
    size: Mutex<TermSize>,
    /// The exit code, between ChildExit and Exit.
    exit_code: Mutex<Option<i32>>,
}

impl Shared {
    fn send(&self, event: TerminalEvent) {
        let _ = self.events.unbounded_send(event);
    }

    fn wake(&self) {
        if !self.wakeup_pending.swap(true, Ordering::AcqRel) {
            self.send(TerminalEvent::Wakeup);
        }
    }

    fn write(&self, text: String) {
        if let Some(sender) = self.sender.get()
            && !text.is_empty()
        {
            let _ = sender.send(Msg::Input(text.into_bytes().into()));
        }
    }
}

/// Receives alacritty's events on the I/O thread, with the term locked: it must not lock the term
/// itself.
#[derive(Clone)]
struct Listener(Arc<Shared>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let shared = &self.0;
        match event {
            Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange => shared.wake(),
            Event::Title(title) => shared.send(TerminalEvent::Title(Some(title))),
            Event::ResetTitle => shared.send(TerminalEvent::Title(None)),
            Event::Bell => shared.send(TerminalEvent::Bell),
            Event::ClipboardStore(_, text) => shared.send(TerminalEvent::Clipboard(text)),
            // Reading the clipboard through OSC 52 is off: a program can't see what was copied.
            Event::ClipboardLoad(..) => {}
            Event::ColorRequest(index, format) => {
                let rgb = shared.palette.lock().unwrap().requested(index);
                shared.write(format(rgb.into()));
            }
            Event::PtyWrite(text) => shared.write(text),
            Event::TextAreaSizeRequest(format) => {
                let size = *shared.size.lock().unwrap();
                shared.write(format(size.window_size()));
            }
            Event::ChildExit(status) => *shared.exit_code.lock().unwrap() = status.code(),
            Event::Exit => {
                let code = *shared.exit_code.lock().unwrap();
                shared.send(TerminalEvent::Exit(code));
            }
        }
    }
}

/// The foreground and background of a cell: dim text darkened, inverse video swapped, hidden text
/// invisible. A cell on the default background has none.
fn cell_colors(cell: &Cell, palette: &Palette, overrides: &Colors) -> (Rgb, Option<Rgb>) {
    let mut fg = palette.resolve(cell.fg, overrides);
    if cell.flags.contains(Flags::DIM) {
        fg = fg.scaled(0.66);
    }
    let mut bg = (cell.bg != Color::Named(NamedColor::Background))
        .then(|| palette.resolve(cell.bg, overrides));
    if cell.flags.contains(Flags::INVERSE) {
        let background =
            bg.unwrap_or_else(|| palette.resolve(Color::Named(NamedColor::Background), overrides));
        bg = Some(fg);
        fg = background;
    }
    (fg, bg)
}

fn grid_point(point: Point) -> GridPoint {
    GridPoint::new(point.line.0, point.column.0)
}

/// A grid point limited to the existing lines and columns.
fn clamp<T>(term: &Term<T>, point: GridPoint) -> Point {
    let line = point
        .line
        .clamp(term.topmost_line().0, term.bottommost_line().0);
    let column = point.column.min(term.last_column().0);
    Point::new(Line(line), Column(column))
}
