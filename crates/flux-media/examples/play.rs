//! Plays a voice message to the end, printing where it is twice a second.
//!
//! ```sh
//! cargo run -p flux-media --example play -- voice.ogg [speed] [seek seconds]
//! ```

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use flux_media::Playback;
use flux_media::voice::VoicePlayer;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(path) = args.next().map(PathBuf::from) else {
        eprintln!("usage: play <file.ogg> [speed] [seek seconds]");
        std::process::exit(2);
    };
    let speed: f32 = args
        .next()
        .and_then(|speed| speed.to_str()?.parse().ok())
        .unwrap_or(1.0);
    let seek: Option<f64> = args.next().and_then(|seek| seek.to_str()?.parse().ok());

    let opened = Instant::now();
    let player = VoicePlayer::open(&path).unwrap();
    println!(
        "{}: {:.2} s, opened in {:?}; pid {}",
        path.display(),
        player.duration().as_secs_f64(),
        opened.elapsed(),
        std::process::id()
    );
    let (changed, change) = mpsc::channel();
    player.on_change(move || {
        let _ = changed.send(());
    });
    player.set_speed(speed);
    if let Some(seek) = seek {
        player.seek(Duration::from_secs_f64(seek));
    }
    let started = Instant::now();
    player.play().unwrap();
    loop {
        match change.recv_timeout(Duration::from_millis(500)) {
            Ok(()) => println!("changed: {:?}", player.state()),
            Err(_) => println!(
                "{:6.2} s at {speed}× ({:?})",
                player.position().as_secs_f64(),
                player.state()
            ),
        }
        if player.state() != Playback::Playing {
            break;
        }
    }
    println!(
        "{:?} after {:.2} s of playing",
        player.state(),
        started.elapsed().as_secs_f64()
    );
}
