//! Real programs on a real PTY: output reaches the grid, input reaches the program, exit is
//! reported; processes, selection, scrolling, paste, search, links, and clearing on live output.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use flux_term::{
    Content, GridPoint, LinkTarget, ScrollDelta, SearchOptions, SelectionKind, Side, TermSize,
    Terminal, TerminalEvent, TerminalEvents, TerminalOptions,
};
use futures::StreamExt;
use futures::executor::block_on;

/// A terminal of `columns` × 10 running `program` with `args` in `cwd`.
fn spawn_in(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    columns: u16,
) -> (Terminal, TerminalEvents) {
    let options = TerminalOptions {
        command: Some((
            program.into(),
            args.iter().map(|arg| arg.to_string()).collect(),
        )),
        cwd: cwd.map(Path::to_path_buf),
        size: TermSize {
            columns,
            rows: 10,
            ..TermSize::default()
        },
        ..TerminalOptions::default()
    };
    Terminal::spawn(options).expect("spawn")
}

fn spawn(script: &str) -> (Terminal, TerminalEvents) {
    spawn_in("/bin/sh", &["-c", script], None, 40)
}

/// An interactive zsh without the user's startup files: it puts commands into their own process
/// groups (job control), as the user's shell does.
fn zsh(cwd: &Path) -> (Terminal, TerminalEvents) {
    let (terminal, events) = spawn_in("/bin/zsh", &["-f", "-i"], Some(cwd), 60);
    wait_until("the prompt", || {
        screen(&terminal.content())
            .iter()
            .any(|row| row.ends_with('%'))
    });
    (terminal, events)
}

/// The visible text, row by row (trailing blanks trimmed).
fn screen(content: &Content) -> Vec<String> {
    let mut rows = vec![vec![' '; content.columns]; content.rows];
    for cell in &content.cells {
        rows[cell.row][cell.column] = cell.c;
    }
    rows.into_iter()
        .map(|row| row.into_iter().collect::<String>().trim_end().to_string())
        .collect()
}

/// Waits until `done` holds, or fails after a few seconds.
fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Waits until the screen satisfies `done`, or fails after a few seconds.
fn wait_for(terminal: &Terminal, done: impl Fn(&[String]) -> bool) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let rows = screen(&terminal.content());
        if done(&rows) {
            return rows;
        }
        assert!(Instant::now() < deadline, "timed out; screen: {rows:#?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap()
}

#[test]
fn output_reaches_the_screen() {
    let (terminal, _events) = spawn("printf 'hello\\nworld'; sleep 2");
    let rows = wait_for(&terminal, |rows| rows[1] == "world");
    assert_eq!(rows[0], "hello");
}

#[test]
fn input_reaches_the_program() {
    let (terminal, _events) = spawn("read line; echo \"got:$line\"; sleep 2");
    terminal.input(b"abc\r".to_vec());
    wait_for(&terminal, |rows| rows.iter().any(|row| row == "got:abc"));
}

#[test]
fn exit_is_reported_with_its_code() {
    let (_terminal, mut events) = spawn("exit 3");
    let code = block_on(async {
        while let Some(event) = events.next().await {
            if let TerminalEvent::Exit(code) = event {
                return code;
            }
        }
        panic!("no exit event")
    });
    assert_eq!(code, Some(3));
}

#[test]
fn colors_and_attributes_are_resolved() {
    let (terminal, _events) =
        spawn("printf '\\033[31mred\\033[0m \\033[1;44mbold\\033[0m'; sleep 2");
    wait_for(&terminal, |rows| rows[0] == "red bold");
    let content = terminal.content();
    let palette = flux_term::Palette::default();
    let r = content.cells.iter().find(|cell| cell.c == 'r').unwrap();
    assert_eq!(r.fg, palette.ansi[1]);
    assert_eq!(r.bg, None);
    let b = content.cells.iter().find(|cell| cell.c == 'b').unwrap();
    assert!(b.flags.bold);
    assert_eq!(b.bg, Some(palette.ansi[4]));
}

#[test]
fn resize_reaches_the_program() {
    let (terminal, _events) = spawn("sleep 0.3; stty size; sleep 2");
    terminal.resize(TermSize {
        columns: 33,
        rows: 7,
        ..TermSize::default()
    });
    wait_for(&terminal, |rows| rows.iter().any(|row| row == "7 33"));
}

#[test]
fn the_foreground_process_follows_commands() {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, _events) = zsh(dir.path());
    let shell = terminal
        .foreground_process()
        .expect("the shell in the foreground");
    assert_eq!(shell.name, "zsh");
    assert_eq!(shell.pid, terminal.shell_pid());
    assert_eq!(terminal.running_process(), None);

    terminal.input(b"sleep 5\r".to_vec());
    wait_until("sleep in the foreground", || {
        terminal
            .running_process()
            .is_some_and(|process| process.name == "sleep")
    });
    terminal.input(b"\x03".to_vec());
    wait_until("the shell back in the foreground", || {
        terminal.running_process().is_none()
    });
}

#[test]
fn the_directory_follows_cd() {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, _events) = zsh(dir.path());
    assert_eq!(
        terminal.cwd().map(|cwd| canonical(&cwd)),
        Some(canonical(dir.path()))
    );
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    terminal.input(b"cd sub\r".to_vec());
    let sub = canonical(&dir.path().join("sub"));
    wait_until("the shell in sub", || {
        terminal.cwd().as_deref() == Some(sub.as_path())
    });
}

#[test]
fn selection_by_cells_words_and_lines() {
    let (terminal, _events) = spawn("printf 'hello world\\nsecond line'; sleep 2");
    wait_for(&terminal, |rows| rows[1] == "second line");

    terminal.start_selection(SelectionKind::Simple, GridPoint::new(0, 0), Side::Left);
    terminal.update_selection(GridPoint::new(0, 4), Side::Right);
    assert_eq!(terminal.selection_text().as_deref(), Some("hello"));
    let selection = terminal.content().selection.unwrap();
    assert_eq!(
        (selection.start, selection.end),
        (GridPoint::new(0, 0), GridPoint::new(0, 4))
    );

    terminal.start_selection(SelectionKind::Word, GridPoint::new(0, 7), Side::Left);
    assert_eq!(terminal.selection_text().as_deref(), Some("world"));

    terminal.start_selection(SelectionKind::Line, GridPoint::new(1, 3), Side::Left);
    assert_eq!(terminal.selection_text().as_deref(), Some("second line\n"));

    terminal.select_all();
    let all = terminal.selection_text().unwrap();
    assert!(all.starts_with("hello world\nsecond line"), "{all:?}");

    // Typing drops the selection.
    terminal.input(b"x".to_vec());
    assert_eq!(terminal.selection_text(), None);
    terminal.clear_selection();
}

#[test]
fn scrolling_through_the_history() {
    let (terminal, _events) = spawn("for i in $(seq 1 50); do echo line$i; done; sleep 2");
    wait_for(&terminal, |rows| rows.iter().any(|row| row == "line50"));
    let content = terminal.content();
    assert_eq!(content.display_offset, 0);
    assert!(content.history_size >= 40, "{}", content.history_size);

    terminal.scroll(ScrollDelta::Lines(5));
    let content = terminal.content();
    assert_eq!(content.display_offset, 5);
    assert_eq!(content.grid_line(0), -5);

    terminal.scroll(ScrollDelta::Top);
    let rows = screen(&terminal.content());
    assert_eq!(rows[0], "line1");

    // Input brings the live screen back.
    terminal.input(b"x".to_vec());
    assert_eq!(terminal.content().display_offset, 0);
}

#[test]
fn scroll_to_shows_a_line_from_the_history() {
    let (terminal, _events) = spawn("for i in $(seq 1 50); do echo line$i; done; sleep 2");
    wait_for(&terminal, |rows| rows.iter().any(|row| row == "line50"));
    let results = terminal.search("line3", SearchOptions::default()).unwrap();
    // line3, line30–line39.
    assert_eq!(results.matches.len(), 11);
    let first = results.matches[0];
    terminal.scroll_to(first.start);
    let content = terminal.content();
    assert!(content.viewport_row(first.start.line).is_some());
}

#[test]
fn paste_is_bracketed_when_the_program_asks() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("pasted");
    let script = format!(
        "stty raw -echo; printf '\\033[?2004h'; head -c 15 > '{}'; sleep 2",
        out.display()
    );
    let (terminal, _events) = spawn(&script);
    wait_until("bracketed paste mode", || terminal.mode().bracketed_paste);
    terminal.paste("a\nb\x1b");
    wait_until("the pasted bytes", || {
        std::fs::read(&out).is_ok_and(|bytes| bytes.len() == 15)
    });
    assert_eq!(std::fs::read(&out).unwrap(), b"\x1b[200~a\nb\x1b[201~");
}

#[test]
fn paste_turns_line_breaks_into_enter_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("pasted");
    let script = format!("stty raw -echo; head -c 4 > '{}'; sleep 2", out.display());
    let (terminal, _events) = spawn(&script);
    // Raw mode is set before the paste: the first line break would end the read otherwise.
    std::thread::sleep(Duration::from_millis(300));
    terminal.paste("a\r\nb\n");
    wait_until("the pasted bytes", || {
        std::fs::read(&out).is_ok_and(|bytes| bytes.len() == 4)
    });
    assert_eq!(std::fs::read(&out).unwrap(), b"a\rb\r");
}

#[test]
fn links_in_live_output() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    let (terminal, _events) = spawn_in(
        "/bin/sh",
        &["-c", "printf 'error at src/main.rs:3:7 here'; sleep 2"],
        Some(dir.path()),
        40,
    );
    wait_for(&terminal, |rows| rows[0].ends_with("here"));

    let link = terminal.link_at(GridPoint::new(0, 12)).unwrap();
    assert_eq!(
        (link.start, link.end),
        (GridPoint::new(0, 9), GridPoint::new(0, 23))
    );
    let LinkTarget::Path { path, line, column } = link.target else {
        panic!("not a path: {:?}", link.target);
    };
    assert_eq!(
        (path.as_str(), line, column),
        ("src/main.rs", Some(3), Some(7))
    );
    let cwd = terminal.cwd();
    let resolved = flux_term::links::resolve(&path, cwd.as_deref(), None).unwrap();
    assert_eq!(
        canonical(&resolved),
        canonical(&dir.path().join("src/main.rs"))
    );

    assert_eq!(terminal.link_at(GridPoint::new(0, 2)), None);
}

#[test]
fn links_through_a_wrapped_line() {
    // 40 columns: the URL starts on the first row and ends on the second.
    let url = "https://example.com/a/very/long/path/to/some/page";
    let (terminal, _events) = spawn(&format!("printf 'see {url} ok'; sleep 2"));
    wait_for(&terminal, |rows| rows[1].ends_with("ok"));
    let link = terminal.link_at(GridPoint::new(1, 2)).unwrap();
    assert_eq!(link.start, GridPoint::new(0, 4));
    assert_eq!(link.end, GridPoint::new(1, 4 + url.len() - 40 - 1));
    assert_eq!(link.target, LinkTarget::Url(url.into()));
}

#[test]
fn search_in_live_output() {
    let (terminal, _events) = spawn("printf 'one Needle\\ntwo needle\\nthree'; sleep 2");
    wait_for(&terminal, |rows| rows[2] == "three");
    let results = terminal.search("needle", SearchOptions::default()).unwrap();
    let starts: Vec<_> = results.matches.iter().map(|m| m.start).collect();
    assert_eq!(starts, [GridPoint::new(0, 4), GridPoint::new(1, 4)]);
    assert_eq!(results.matches[0].end, GridPoint::new(0, 9));
    let sensitive = SearchOptions {
        case_sensitive: true,
        ..SearchOptions::default()
    };
    let results = terminal.search("needle", sensitive).unwrap();
    assert_eq!(results.matches.len(), 1);
}

#[test]
fn clear_keeps_the_prompt_at_the_top() {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, _events) = zsh(dir.path());
    terminal.input(b"seq 1 30\r".to_vec());
    wait_until("the output in the history", || {
        let content = terminal.content();
        content.history_size > 10
            && screen(&content)
                .last()
                .is_some_and(|row| row.ends_with('%'))
    });

    terminal.clear();
    // The shell's own clear (ED 2) on the empty screen leaves one blank line in alacritty's history.
    wait_until("the prompt at the top", || {
        let content = terminal.content();
        let rows = screen(&content);
        content.history_size <= 1
            && rows[0].ends_with('%')
            && rows[1..].iter().all(String::is_empty)
    });
}

#[test]
fn clear_leaves_a_running_command_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (terminal, _events) = zsh(dir.path());
    terminal.input(b"seq 1 30; echo still-here; sleep 5\r".to_vec());
    wait_until("sleep in the foreground", || {
        terminal
            .running_process()
            .is_some_and(|process| process.name == "sleep")
    });
    wait_for(&terminal, |rows| rows.iter().any(|row| row == "still-here"));

    terminal.clear();
    let content = terminal.content();
    assert_eq!(content.history_size, 0);
    assert!(screen(&content).iter().any(|row| row == "still-here"));
    terminal.input(b"\x03".to_vec());
}

#[test]
fn focus_is_reported_when_the_program_asks() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("focus");
    let script = format!(
        "stty raw -echo; printf '\\033[?1004h'; head -c 6 > '{}'; sleep 2",
        out.display()
    );
    let (terminal, _events) = spawn(&script);
    wait_until("focus reporting mode", || terminal.mode().focus_reporting);
    terminal.report_focus(true);
    terminal.report_focus(false);
    wait_until("the focus reports", || {
        std::fs::read(&out).is_ok_and(|bytes| bytes.len() == 6)
    });
    assert_eq!(std::fs::read(&out).unwrap(), b"\x1b[I\x1b[O");
}
