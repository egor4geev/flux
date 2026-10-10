//! Installs every server of the fixtures (the ten Flux had built in before its language plugins)
//! that is not on the machine into Flux's real servers directory, so that Flux finds them ready:
//! `cargo test -p flux-lsp --test install -- --ignored --nocapture`. `FLUX_SERVERS_DIR` sets another
//! directory.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use flux_lsp::install::{self, Progress};

mod fixtures {
    use flux_lsp::config::{Install, ServerConfig};
    include!("fixtures/servers.rs");
}

use fixtures::default_servers;

fn servers_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FLUX_SERVERS_DIR") {
        return dir.into();
    }
    let home = std::env::var_os("HOME").expect("HOME");
    PathBuf::from(home).join("Library/Application Support/flux/servers")
}

#[test]
#[ignore]
fn install_every_server() {
    install::set_dir(servers_dir());
    let cancel = AtomicBool::new(false);
    let mut failed = Vec::new();
    for config in default_servers() {
        if let Err(reason) = install::check(&config) {
            eprintln!("{}: can't install here: {reason}", config.name);
            continue;
        }
        let started = Instant::now();
        let last = Mutex::new(String::new());
        let progress = |p: Progress| {
            let mut last = last.lock().unwrap();
            if *last != p.text {
                eprintln!("  {}: {}", config.name, p.text);
                *last = p.text;
            }
        };
        match install::install(&config, &progress, &cancel) {
            Ok(()) => eprintln!(
                "{}: {:?} ({:.1?})",
                config.name,
                config.resolve_command(),
                started.elapsed()
            ),
            Err(err) => {
                eprintln!("{}: FAILED: {err}", config.name);
                failed.push(config.name.clone());
            }
        }
        if config.resolve_command().is_none() {
            failed.push(config.name.clone());
        }
    }
    assert!(failed.is_empty(), "not installed: {failed:?}");
}
