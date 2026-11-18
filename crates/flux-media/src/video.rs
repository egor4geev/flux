//! A video's player: the system's (hardware) decoder plays the file — on macOS AVFoundation
//! (`AVPlayer` with an `AVPlayerItemVideoOutput`, [`macos`]), its frames `CVPixelBuffer`s that
//! gpui's `surface` draws without a copy; on Windows Media Foundation (`IMFMediaEngine`,
//! `windows.rs` — not built yet: `open` says `Unsupported`), its frames as pixels. The sound goes
//! out through the system's player; unmuted, it takes the sound ([`crate::sound`]). A dropped
//! player lets its decoder go — the app drops the players of videos off screen.
//!
//! On macOS a player is driven on the main thread (AVFoundation's player is main-thread-only):
//! `open` there fails elsewhere, and a call from another thread goes to the main queue. Its
//! notifications come through the main queue, which the app's run loop serves.
//!
//! Drawing a frame (macOS): `Frame::Surface(buffer)` → `gpui::surface(buffer)` sized as the
//! picture is shown (`size`'s aspect ratio), with corner radii from its style (Flux's gpui patch:
//! `.rounded(px(8.))`, `.rounded_full()` for a round message) — they round the picture as drawn,
//! as `img`'s do, so a box of another aspect ratio rounds the letterboxed picture inside it. The
//! buffer is `420f` (biplanar Y′CbCr 4:2:0, full range) — the one format gpui's Metal surface
//! draws. Keep the last frame and draw it every render: `frame` gives a new one only when there
//! is one.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[cfg(target_os = "macos")]
use macos as backend;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use unsupported as backend;
#[cfg(target_os = "windows")]
use windows as backend;

use crate::{MediaError, Playback};

/// How a video starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoOptions {
    /// Without sound: a GIF, a round message in the feed.
    pub muted: bool,
    /// From the start again at the end.
    pub looped: bool,
}

/// A frame to draw.
#[derive(Clone)]
pub enum Frame {
    /// macOS: the decoder's buffer, drawn by gpui's `surface` without a copy.
    #[cfg(target_os = "macos")]
    Surface(core_video::pixel_buffer::CVPixelBuffer),
    /// Elsewhere: the frame's pixels — BGRA, rows top to bottom.
    Pixels {
        width: u32,
        height: u32,
        bgra: Arc<[u8]>,
    },
}

/// A video open for playing. Dropping it stops it and lets the decoder go.
pub struct VideoPlayer {
    inner: backend::Player,
}

impl VideoPlayer {
    /// Opens an MP4 (H.264, HEVC): nothing plays yet; the first frame comes soon after. The size
    /// and the length come a moment later (`on_change`).
    pub fn open(path: &Path, options: VideoOptions) -> Result<Self, MediaError> {
        backend::Player::open(path, options).map(|inner| VideoPlayer { inner })
    }

    /// Plays from where it is (from the start once it ended); unmuted, it takes the sound.
    pub fn play(&self) -> Result<(), MediaError> {
        self.inner.play()
    }

    pub fn pause(&self) {
        self.inner.pause()
    }

    /// Goes to `to` (clamped to the length), playing or not; the frame there comes next.
    pub fn seek(&self, to: Duration) {
        self.inner.seek(to)
    }

    /// Unmuting a playing video takes the sound.
    pub fn set_muted(&self, muted: bool) {
        self.inner.set_muted(muted)
    }

    pub fn muted(&self) -> bool {
        self.inner.muted()
    }

    /// 0.0–1.0.
    pub fn set_volume(&self, volume: f32) {
        self.inner.set_volume(volume)
    }

    /// 0.5–2.0; the system keeps the pitch.
    pub fn set_speed(&self, speed: f32) {
        self.inner.set_speed(speed)
    }

    pub fn set_looped(&self, looped: bool) {
        self.inner.set_looped(looped)
    }

    pub fn position(&self) -> Duration {
        self.inner.position()
    }

    /// The length; zero until the file's header is read.
    pub fn duration(&self) -> Duration {
        self.inner.duration()
    }

    /// The picture's size in pixels as shown (its rotation applied), once known.
    pub fn size(&self) -> Option<(u32, u32)> {
        self.inner.size()
    }

    pub fn state(&self) -> Playback {
        self.inner.state()
    }

    /// Why it can't be played, once that is known (a broken file, no video in it, a failure
    /// while playing): `on_change` tells of it, the state is `Paused`, `play` fails.
    pub fn error(&self) -> Option<String> {
        self.inner.error()
    }

    /// The frame to show now: a new one since the last call, else `None` — the last one stays on
    /// screen. Cheap: the app calls it at the video's rate ([`Self::frame_interval`]) while the
    /// video is on screen. A paused video gives the frame where it stands once, after `open`
    /// and after a `seek`.
    pub fn frame(&self) -> Option<Frame> {
        self.inner.frame()
    }

    /// How often a new frame comes (the video's frame rate, once known; 33 ms before): the app
    /// asks for frames at this rate while it plays.
    pub fn frame_interval(&self) -> Duration {
        self.inner.frame_interval()
    }

    /// Called on another thread (macOS: the main queue or AVFoundation's thread) when the state
    /// changes by itself: the size and the length are known, the end, another sound took over, a
    /// failure.
    pub fn on_change(&self, callback: impl Fn() + Send + Sync + 'static) {
        self.inner.on_change(callback)
    }
}

/// No video backend on this system.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod unsupported {
    pub(super) use super::windows_stub::Player;
}

/// A backend that plays nothing: Windows until its backend is built, other systems.
#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
mod windows_stub {
    use std::path::Path;
    use std::time::Duration;

    use super::{Frame, VideoOptions};
    use crate::{MediaError, Playback};

    pub(crate) enum Player {}

    impl Player {
        pub fn open(path: &Path, options: VideoOptions) -> Result<Self, MediaError> {
            let _ = (path, options);
            Err(MediaError::Unsupported(
                "videos: not on this system yet".into(),
            ))
        }
        pub fn play(&self) -> Result<(), MediaError> {
            match *self {}
        }
        pub fn pause(&self) {
            match *self {}
        }
        pub fn seek(&self, _: Duration) {
            match *self {}
        }
        pub fn set_muted(&self, _: bool) {
            match *self {}
        }
        pub fn muted(&self) -> bool {
            match *self {}
        }
        pub fn set_volume(&self, _: f32) {
            match *self {}
        }
        pub fn set_speed(&self, _: f32) {
            match *self {}
        }
        pub fn set_looped(&self, _: bool) {
            match *self {}
        }
        pub fn position(&self) -> Duration {
            match *self {}
        }
        pub fn duration(&self) -> Duration {
            match *self {}
        }
        pub fn size(&self) -> Option<(u32, u32)> {
            match *self {}
        }
        pub fn state(&self) -> Playback {
            match *self {}
        }
        pub fn error(&self) -> Option<String> {
            match *self {}
        }
        pub fn frame(&self) -> Option<Frame> {
            match *self {}
        }
        pub fn frame_interval(&self) -> Duration {
            match *self {}
        }
        pub fn on_change(&self, _: impl Fn() + Send + Sync + 'static) {
            match *self {}
        }
    }
}
