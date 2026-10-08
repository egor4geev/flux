//! Timings on a real PTY, by hand:
//! `cargo test -p flux-term --release --test bench -- --ignored --nocapture` (and without
//! `--release`: the dev profile builds alacritty_terminal and the regex engine optimized).

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use flux_term::{SearchOptions, TermSize, Terminal, TerminalEvents, TerminalOptions};

fn cat(file: &Path, columns: u16, rows: u16) -> (Terminal, TerminalEvents) {
    let script = format!("cat '{}'; sleep 60", file.display());
    let options = TerminalOptions {
        command: Some(("/bin/sh".into(), vec!["-c".into(), script])),
        size: TermSize {
            columns,
            rows,
            ..TermSize::default()
        },
        ..TerminalOptions::default()
    };
    Terminal::spawn(options).expect("spawn")
}

fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The fastest of `runs` runs.
fn time<T>(runs: usize, mut f: impl FnMut() -> T) -> (Duration, T) {
    let mut best = Duration::MAX;
    let mut result = None;
    for _ in 0..runs {
        let start = Instant::now();
        result = Some(f());
        best = best.min(start.elapsed());
    }
    (best, result.unwrap())
}

#[test]
#[ignore]
fn search_in_a_full_scrollback() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("log.txt");
    let mut text = String::new();
    for i in 0..12_000 {
        let word = if i % 100 == 0 { "needle" } else { "hay" };
        writeln!(text, "line {i:05} {word} lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod").unwrap();
    }
    std::fs::write(&file, text).unwrap();
    let (terminal, _events) = cat(&file, 120, 50);
    wait_until("the full scrollback", || {
        let content = terminal.content();
        content.history_size >= 10_000
            && terminal
                .search("line 11999", SearchOptions::default())
                .is_ok_and(|results| !results.matches.is_empty())
    });

    let cases = [
        ("needle", SearchOptions::default()),
        ("nothing-like-this", SearchOptions::default()),
        ("e", SearchOptions::default()),
        (
            r"line \d+5 needle",
            SearchOptions {
                regex: true,
                ..SearchOptions::default()
            },
        ),
        (
            "needle",
            SearchOptions {
                whole_word: true,
                case_sensitive: true,
                ..SearchOptions::default()
            },
        ),
    ];
    for (query, options) in cases {
        let (elapsed, results) = time(5, || terminal.search(query, options).unwrap());
        println!(
            "search {query:?} {options:?}: {} matches{} in {elapsed:?}",
            results.matches.len(),
            if results.truncated {
                " (truncated)"
            } else {
                ""
            }
        );
    }
}

#[test]
#[ignore]
fn snapshot_of_a_full_colored_screen() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("colors.txt");
    let mut text = String::new();
    for line in 0..60 {
        for chunk in 0..40 {
            let color = 31 + (line + chunk) % 7;
            write!(text, "\x1b[{color}mab{chunk:02}\x1b[0m ").unwrap();
        }
        text.push('\n');
    }
    std::fs::write(&file, text).unwrap();
    let (terminal, _events) = cat(&file, 200, 50);
    wait_until("a full screen", || terminal.content().cells.len() > 7_000);

    let (elapsed, content) = time(200, || terminal.content());
    println!(
        "content() of {}×{} with {} cells: {elapsed:?}",
        content.columns,
        content.rows,
        content.cells.len()
    );
}

#[test]
#[ignore]
fn parsing_a_burst_of_output() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("big.txt");
    let mut text = String::new();
    for i in 0..40_000 {
        writeln!(
            text,
            "\x1b[32m{i:06}\x1b[0m fn item_{i}() -> Result<Vec<u8>, Error> {{ todo!(\"{i}\") }}"
        )
        .unwrap();
    }
    text.push_str("END-OF-OUTPUT\n");
    std::fs::write(&file, &text).unwrap();
    let start = Instant::now();
    let (terminal, _events) = cat(&file, 120, 50);
    wait_until("the end of the output", || {
        terminal
            .search("END-OF-OUTPUT", SearchOptions::default())
            .is_ok_and(|results| !results.matches.is_empty())
    });
    println!(
        "{:.1} MB of output parsed in {:?}",
        text.len() as f64 / 1e6,
        start.elapsed()
    );
}
