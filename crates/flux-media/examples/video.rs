//! Plays a video for 3 s as the app does — asking for a frame at the video's rate, serving the
//! main run loop between — and prints the frames, the size, the frames' pixel format and the CPU
//! the process used. With `--cycles N` it opens and drops the video N times and prints the memory
//! before and after (a dropped player must let everything go).
//!
//! ```sh
//! cargo run -p flux-media --example video -- video.mp4 [--sound] [--loop] [--cycles 300]
//! ```

#[cfg(target_os = "macos")]
fn main() {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use core_video::pixel_buffer::CVPixelBuffer;
    use flux_media::video::{Frame, VideoOptions, VideoPlayer};
    use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args
        .iter()
        .find(|arg| !arg.starts_with("--"))
        .map(PathBuf::from)
    else {
        eprintln!("usage: video <file.mp4> [--sound] [--loop] [--cycles N]");
        std::process::exit(2);
    };
    let options = VideoOptions {
        muted: !args.iter().any(|arg| arg == "--sound"),
        looped: args.iter().any(|arg| arg == "--loop"),
    };
    let cycles: Option<usize> = args
        .iter()
        .position(|arg| arg == "--cycles")
        .and_then(|at| args.get(at + 1)?.parse().ok());
    // The main run loop, as the app's: AVFoundation's notifications come through it.
    let serve = |time: Duration| {
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, time.as_secs_f64(), false);
    };

    if let Some(cycles) = cycles {
        let before = memory();
        for cycle in 0..cycles {
            // The app's run loop drains an autorelease pool every turn; a bare one doesn't.
            objc2::rc::autoreleasepool(|_| {
                let player = VideoPlayer::open(&path, options).unwrap();
                player.play().unwrap();
                let started = Instant::now();
                while player.frame().is_none() && started.elapsed() < Duration::from_secs(1) {
                    serve(Duration::from_millis(5));
                }
                drop(player);
                serve(Duration::from_millis(2));
            });
            if cycle == 9 {
                println!("after 10: {} KB", memory());
            }
        }
        serve(Duration::from_millis(200));
        println!("{cycles} open/play/drop: {before} KB → {} KB", memory());
        return;
    }

    let opened = Instant::now();
    let player = VideoPlayer::open(&path, options).unwrap();
    println!("opened in {:?}", opened.elapsed());
    let changes = Arc::new(AtomicUsize::new(0));
    let counted = changes.clone();
    player.on_change(move || {
        counted.fetch_add(1, Ordering::Relaxed);
    });
    let ready = Instant::now();
    while player.size().is_none() && ready.elapsed() < Duration::from_secs(2) {
        serve(Duration::from_millis(2));
    }
    println!(
        "{}: {:?}, {:.2} s, a frame every {:?}; known after {:?}",
        path.display(),
        player.size(),
        player.duration().as_secs_f64(),
        player.frame_interval(),
        ready.elapsed()
    );

    let cpu = cpu_time();
    let started = Instant::now();
    player.play().unwrap();
    let mut frames = 0;
    let mut last: Option<CVPixelBuffer> = None;
    while started.elapsed() < Duration::from_secs(3) {
        if let Some(Frame::Surface(buffer)) = player.frame() {
            frames += 1;
            last = Some(buffer);
        }
        serve(player.frame_interval() / 2);
    }
    let wall = started.elapsed().as_secs_f64();
    let used = cpu_time() - cpu;
    if let Some(buffer) = last {
        let format = buffer.get_pixel_format().to_be_bytes();
        println!(
            "{frames} frames in {wall:.2} s; {}×{} {:?} ({} planes); position {:.2} s, {:?}",
            buffer.get_width(),
            buffer.get_height(),
            String::from_utf8_lossy(&format),
            buffer.get_plane_count(),
            player.position().as_secs_f64(),
            player.state()
        );
    } else {
        println!("no frames in {wall:.2} s ({:?})", player.error());
    }
    println!(
        "CPU: {:.1} % of a core ({used:.3} s in {wall:.2} s); {} changes",
        100.0 * used / wall,
        changes.load(Ordering::Relaxed)
    );
}

/// The process's resident memory, KB.
#[cfg(target_os = "macos")]
fn memory() -> u64 {
    ps("rss=").trim().parse().unwrap_or(0)
}

/// The CPU time the process used so far, seconds.
#[cfg(target_os = "macos")]
fn cpu_time() -> f64 {
    // `[[hh:]mm:]ss.cc`
    ps("cputime=").trim().split(':').fold(0.0, |total, part| {
        total * 60.0 + part.parse::<f64>().unwrap_or(0.0)
    })
}

#[cfg(target_os = "macos")]
fn ps(field: &str) -> String {
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", field, "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("the video example plays on macOS");
}
