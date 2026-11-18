//! The macOS video backend on a tiny MP4 (128×96, 1 s, H.264 + AAC): the size and the length
//! come through `on_change`, playing gives `420f` frames gpui's surface draws, the end stops it,
//! a seek and a replay work, and a dropped player is gone. On the main thread, as in the app —
//! so without the test harness, which runs tests on threads of its own.

#[cfg(target_os = "macos")]
fn main() {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use flux_media::Playback;
    use flux_media::video::{Frame, VideoOptions, VideoPlayer};
    use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};

    let serve = |until: &mut dyn FnMut() -> bool, limit: Duration| {
        let started = Instant::now();
        while !until() && started.elapsed() < limit {
            CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.005, false);
        }
        until()
    };
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny.mp4");
    objc2::rc::autoreleasepool(|_| {
        let options = VideoOptions {
            muted: true,
            looped: false,
        };
        let player = VideoPlayer::open(&fixture, options).unwrap();
        let changes = Arc::new(AtomicUsize::new(0));
        let counted = changes.clone();
        player.on_change(move || {
            counted.fetch_add(1, Ordering::Relaxed);
        });
        assert!(serve(
            &mut || player.size().is_some(),
            Duration::from_secs(3)
        ));
        assert_eq!(player.size(), Some((128, 96)));
        assert!((player.duration().as_secs_f64() - 1.0).abs() < 0.05);
        assert!((player.frame_interval().as_secs_f64() - 1.0 / 30.0).abs() < 1e-4);
        assert!(changes.load(Ordering::Relaxed) >= 1);

        player.play().unwrap();
        let mut frames = 0;
        let mut format = 0;
        serve(
            &mut || {
                if let Some(Frame::Surface(buffer)) = player.frame() {
                    frames += 1;
                    format = buffer.get_pixel_format();
                    assert_eq!((buffer.get_width(), buffer.get_height()), (128, 96));
                }
                player.state() == Playback::Ended
            },
            Duration::from_secs(3),
        );
        assert_eq!(player.state(), Playback::Ended);
        assert_eq!(&format.to_be_bytes(), b"420f");
        assert!(frames >= 10, "{frames} frames");
        assert_eq!(player.position(), player.duration());

        // Back to the middle, then played again from the start.
        player.seek(Duration::from_millis(500));
        assert_eq!(player.state(), Playback::Paused);
        assert!(serve(
            &mut || (player.position().as_secs_f64() - 0.5).abs() < 0.05,
            Duration::from_secs(1)
        ));
        player.play().unwrap();
        assert!(serve(
            &mut || player.state() == Playback::Ended,
            Duration::from_secs(2)
        ));
        player.play().unwrap();
        assert!(serve(
            &mut || player.position() > Duration::ZERO
                && player.position() < Duration::from_millis(500),
            Duration::from_secs(1)
        ));
        player.pause();
        assert_eq!(player.state(), Playback::Paused);
        assert!(player.error().is_none());
    });

    // Not a video: the failure comes through `on_change`.
    let voice = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/voice.ogg");
    let options = VideoOptions {
        muted: true,
        looped: true,
    };
    let player = VideoPlayer::open(&voice, options).unwrap();
    assert!(serve(
        &mut || player.error().is_some(),
        Duration::from_secs(3)
    ));
    assert!(player.play().is_err());
    println!("video: ok");
}

#[cfg(not(target_os = "macos"))]
fn main() {}
