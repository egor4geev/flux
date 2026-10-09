//! Plugins under development: Flux builds the author's cargo project itself (as Zed does for its
//! dev extensions) and reloads the plugin when its component changes.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use crate::log::{Level, PluginLog};
use crate::registry::PluginEntry;

/// The target plugins are built for.
pub const TARGET: &str = "wasm32-wasip2";

/// The lines of a failed build's output that the error keeps.
const ERROR_LINES: usize = 30;

/// The folder is a cargo project Flux builds.
pub fn is_cargo_project(dir: &Path) -> bool {
    dir.join("Cargo.toml").is_file()
}

/// Builds a plugin under development: `cargo build --release --target wasm32-wasip2` in its
/// folder; the output goes to its log. Blocks: call it in the background.
pub fn build(dir: &Path, log: &PluginLog) -> Result<(), String> {
    let cargo = find_cargo().ok_or_else(|| {
        "cargo isn't found: install Rust (https://rustup.rs) and the wasm32-wasip2 target"
            .to_string()
    })?;
    log.write(
        Level::Info,
        &format!("Building: cargo build --release --target {TARGET}"),
    );
    let started = Instant::now();
    let mut command = Command::new(&cargo);
    command
        .args(["build", "--release", "--color", "never", "--target", TARGET])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    clean_env(&mut command);
    // Started from Finder, Flux has a short PATH: cargo needs rustc next to it.
    if let Some(bin) = cargo.parent() {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs = vec![bin.to_path_buf()];
        dirs.extend(std::env::split_paths(&path));
        if let Ok(path) = std::env::join_paths(dirs) {
            command.env("PATH", path);
        }
    }
    let mut child = command
        .spawn()
        .map_err(|err| format!("Can't run {}: {err}", cargo.display()))?;
    // Both streams to the log as they come; the last lines for the error.
    let tail = Arc::new(Mutex::new(VecDeque::new()));
    let readers: Vec<_> = [
        child
            .stdout
            .take()
            .map(|out| Box::new(out) as Box<dyn std::io::Read + Send>),
        child
            .stderr
            .take()
            .map(|err| Box::new(err) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|stream| {
        let log = log.clone();
        let tail = tail.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                log.write(level_of(&line), &line);
                let mut tail = tail.lock().unwrap();
                if tail.len() == ERROR_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        })
    })
    .collect();
    let status = child
        .wait()
        .map_err(|err| format!("cargo didn't finish: {err}"))?;
    for reader in readers {
        let _ = reader.join();
    }
    if status.success() {
        log.write(
            Level::Info,
            &format!("Built in {:.1} s", started.elapsed().as_secs_f64()),
        );
        return Ok(());
    }
    let tail: Vec<String> = tail.lock().unwrap().iter().cloned().collect();
    let output = tail.join("\n");
    if output.contains("target may not be installed")
        || output.contains("can't find crate for `core`")
    {
        return Err(format!(
            "The {TARGET} target isn't installed: run `rustup target add {TARGET}`"
        ));
    }
    Err(format!("cargo build failed:\n{output}"))
}

/// When the plugin's component was last written: a newer one is reloaded.
pub fn component_mtime(entry: &PluginEntry) -> Option<SystemTime> {
    let dir = entry.files.dir()?;
    let wasm = entry.manifest.wasm.as_deref()?;
    std::fs::metadata(dir.join(wasm)).ok()?.modified().ok()
}

/// cargo: on the PATH, or where rustup and Homebrew put it.
fn find_cargo() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(home.map(|home| home.join(".cargo/bin")))
        .chain(
            [
                "/opt/homebrew/opt/rustup/bin",
                "/opt/homebrew/bin",
                "/usr/local/bin",
            ]
            .map(PathBuf::from),
        )
        .map(|dir| dir.join("cargo"))
        .find(|cargo| cargo.is_file())
}

/// Removes what a cargo that runs Flux (or its tests) passes down and a nested build must not
/// inherit: flags, wrappers (clippy), the target directory, the build script's variables. The
/// user's own cargo configuration (`CARGO_HOME`, registries, network) stays.
pub(crate) fn clean_env(command: &mut Command) {
    const REMOVE: [&str; 14] = [
        "RUSTFLAGS",
        "RUSTDOCFLAGS",
        "RUSTC",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO",
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_MAKEFLAGS",
        "OUT_DIR",
        "TARGET",
        "PROFILE",
    ];
    const PREFIXES: [&str; 7] = [
        "CARGO_PKG_",
        "CARGO_CFG_",
        "CARGO_FEATURE_",
        "CARGO_MANIFEST_",
        "CARGO_PRIMARY_",
        "CARGO_CRATE_",
        "__CARGO",
    ];
    for name in REMOVE {
        command.env_remove(name);
    }
    for (name, _) in std::env::vars_os() {
        let Some(name) = name.to_str() else {
            continue;
        };
        if PREFIXES.iter().any(|prefix| name.starts_with(prefix)) {
            command.env_remove(name);
        }
    }
}

/// Errors and warnings of cargo stand out in the log.
fn level_of(line: &str) -> Level {
    if line.starts_with("error") {
        Level::Error
    } else if line.starts_with("warning") {
        Level::Warn
    } else {
        Level::Info
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_plugin_and_reports_failures() {
        let log = PluginLog::memory();
        let dir = crate::tests::temp_dir("dev-broken");
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"broken\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[lib]\n\
             crate-type = [\"cdylib\"]\n\n[workspace]\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/lib.rs"),
            "pub fn f() -> u32 { \"not a number\" }\n",
        )
        .unwrap();
        let Err(error) = build(&dir, &log) else {
            panic!("a broken plugin built");
        };
        if error.contains("isn't installed") || error.contains("isn't found") {
            eprintln!("skipped: {error}");
            return;
        }
        assert!(error.contains("mismatched types"), "{error}");
        assert!(
            log.lines().iter().any(|line| line.level == Level::Error),
            "the errors are in the log"
        );
        std::fs::write(dir.join("src/lib.rs"), "pub fn f() -> u32 { 1 }\n").unwrap();
        build(&dir, &log).unwrap();
        assert!(
            dir.join("target/wasm32-wasip2/release/broken.wasm")
                .is_file()
        );
    }
}
