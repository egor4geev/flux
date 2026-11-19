//! The video backends on a tiny MP4 (128×96, 1 s, H.264 + AAC) and, on Windows, a GIF-like one
//! (96×96, 1 s, H.264 without sound) and an HEVC one (128×128, 1 s): the size and the length
//! come through `on_change`, playing gives frames the app draws (macOS: `420f` buffers for gpui's
//! surface; Windows: BGRA pixels), the end stops it, a looped one plays on, a seek and a replay
//! work, and a dropped player is gone. On the main thread, as in the app (AVFoundation's player
//! is main-thread-only) — so without the test harness, which runs tests on threads of its own.

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

/// Windows: Media Foundation's media engine. Skipped, with the reason printed, where Media
/// Foundation or its H.264 decoder isn't there (a Windows Server without its media feature, an N
/// edition without the Media Feature Pack).
#[cfg(target_os = "windows")]
fn main() {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use flux_media::video::{Frame, VideoOptions, VideoPlayer};
    use flux_media::{MediaError, Playback};

    let wait = |until: &mut dyn FnMut() -> bool, limit: Duration| {
        let started = Instant::now();
        while !until() && started.elapsed() < limit {
            std::thread::sleep(Duration::from_millis(5));
        }
        until()
    };
    // A frame's pixels: its size, BGRA rows, opaque, not all black.
    let check = |frame: Frame, size: (u32, u32)| {
        let Frame::Pixels {
            width,
            height,
            bgra,
        } = frame;
        assert_eq!((width, height), size);
        assert_eq!(bgra.len(), (width * height * 4) as usize);
        let pixels = bgra.as_chunks::<4>().0;
        assert!(pixels.iter().all(|pixel| pixel[3] == u8::MAX));
        assert!(pixels.iter().any(|pixel| pixel[..3] != [0, 0, 0]));
    };
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let opened = Instant::now();
    let player = match VideoPlayer::open(
        &fixtures.join("tiny.mp4"),
        VideoOptions {
            muted: true,
            looped: false,
        },
    ) {
        Ok(player) => player,
        Err(MediaError::Unsupported(why)) => {
            println!("video: skipped — {why}");
            return;
        }
        Err(error) => panic!("tiny.mp4: {error}"),
    };
    println!("video: opened in {:?}", opened.elapsed());
    let changes = Arc::new(AtomicUsize::new(0));
    let counted = changes.clone();
    player.on_change(move || {
        counted.fetch_add(1, Ordering::Relaxed);
    });
    assert!(wait(
        &mut || player.size().is_some() || player.error().is_some(),
        Duration::from_secs(10)
    ));
    if let Some(error) = player.error() {
        println!("video: skipped — {error}");
        return;
    }
    println!("video: metadata in {:?}", opened.elapsed());
    assert_eq!(player.size(), Some((128, 96)));
    assert!((player.duration().as_secs_f64() - 1.0).abs() < 0.05);
    assert!((player.frame_interval().as_secs_f64() - 1.0 / 30.0).abs() < 1e-3);
    assert!(changes.load(Ordering::Relaxed) >= 1);

    // Paused, it gives its first frame.
    let mut first = None;
    assert!(
        wait(
            &mut || {
                first = first.take().or_else(|| player.frame());
                first.is_some()
            },
            Duration::from_secs(5)
        ),
        "a paused video's first frame"
    );
    check(first.unwrap(), (128, 96));

    player.play().unwrap();
    let mut frames = 0;
    let mut last = None;
    wait(
        &mut || {
            if let Some(frame) = player.frame() {
                frames += 1;
                let Frame::Pixels { bgra, .. } = &frame;
                last = Some(bgra.clone());
                check(frame, (128, 96));
            }
            player.state() == Playback::Ended
        },
        Duration::from_secs(5),
    );
    assert_eq!(player.state(), Playback::Ended, "{:?}", player.error());
    assert!(frames >= 10, "{frames} frames");
    assert_eq!(player.position(), player.duration());
    println!("video: played, {frames} frames");

    // Back to the middle, then played again from the start. The frame a seek went to while
    // paused: said, not required (the engine may give it only once it plays).
    player.seek(Duration::from_millis(500));
    assert_eq!(player.state(), Playback::Paused);
    assert!(wait(
        &mut || (player.position().as_secs_f64() - 0.5).abs() < 0.05,
        Duration::from_secs(2)
    ));
    let mut still = None;
    wait(
        &mut || {
            still = still.take().or_else(|| player.frame());
            still.is_some()
        },
        Duration::from_secs(3),
    );
    match still {
        Some(Frame::Pixels { bgra, .. }) if Some(&bgra) != last.as_ref() => {
            println!("video: a paused seek shows the frame there")
        }
        Some(_) => println!("video: a paused seek gives the frame it stood at"),
        None => println!("video: a paused seek gives no frame"),
    }
    player.play().unwrap();
    assert!(wait(
        &mut || player.state() == Playback::Ended,
        Duration::from_secs(3)
    ));
    player.play().unwrap();
    assert!(wait(
        &mut || player.position() > Duration::ZERO
            && player.position() < Duration::from_millis(500),
        Duration::from_secs(2)
    ));
    player.set_speed(2.0);
    player.set_volume(0.5);
    player.pause();
    assert_eq!(player.state(), Playback::Paused);
    assert!(player.error().is_none());
    drop(player);

    // A GIF: looped without sound, it plays on past its end.
    let opened = Instant::now();
    let gif = VideoPlayer::open(
        &fixtures.join("gif.mp4"),
        VideoOptions {
            muted: true,
            looped: true,
        },
    )
    .unwrap();
    println!("video: a GIF opened in {:?}", opened.elapsed());
    assert!(wait(&mut || gif.size().is_some(), Duration::from_secs(5)));
    println!("video: its metadata in {:?}", opened.elapsed());
    assert_eq!(gif.size(), Some((96, 96)));
    gif.play().unwrap();
    let started = Instant::now();
    let mut frames = 0;
    wait(
        &mut || {
            if let Some(frame) = gif.frame() {
                frames += 1;
                check(frame, (96, 96));
            }
            started.elapsed() > Duration::from_millis(2500)
        },
        Duration::from_secs(3),
    );
    assert_eq!(gif.state(), Playback::Playing, "{:?}", gif.error());
    assert!(frames >= 50, "{frames} frames in 2.5 s of a looped 1 s GIF");
    assert!(gif.position() < gif.duration());
    println!("video: looped, {frames} frames in 2.5 s");
    drop(gif);

    // HEVC: plays where the system has its decoder (the HEVC Video Extensions), else fails with
    // a word saying so — never silently.
    let hevc = VideoPlayer::open(
        &fixtures.join("hevc.mp4"),
        VideoOptions {
            muted: true,
            looped: false,
        },
    )
    .unwrap();
    let _ = hevc.play();
    let mut frames = 0;
    wait(
        &mut || {
            if let Some(frame) = hevc.frame() {
                frames += 1;
                check(frame, (128, 128));
            }
            frames >= 3 || hevc.error().is_some()
        },
        Duration::from_secs(5),
    );
    match hevc.error() {
        Some(error) => println!("video: HEVC can't be played — {error}"),
        None => {
            assert!(frames >= 3, "HEVC: neither frames nor a failure");
            println!("video: HEVC plays");
        }
    }
    drop(hevc);

    // Not a video: the failure comes through `on_change` (or at once).
    let voice = fixtures.join("voice.ogg");
    let options = VideoOptions {
        muted: true,
        looped: true,
    };
    match VideoPlayer::open(&voice, options) {
        Ok(player) => {
            assert!(wait(
                &mut || player.error().is_some(),
                Duration::from_secs(5)
            ));
            println!("video: not a video — {}", player.error().unwrap());
            assert!(player.play().is_err());
        }
        Err(error) => println!("video: not a video — {error}"),
    }
    println!("video: ok");
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn main() {}
