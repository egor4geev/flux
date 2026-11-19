//! Sound and video for Flux (API 0.5, FluxGram part 4): what a plugin's conversation plays.
//!
//! - [`voice`] — a voice message: OGG/Opus decoded here (libopus), out through the system's sound
//!   (cpal: CoreAudio, WASAPI), streamed — the file isn't decoded whole; faster without the pitch
//!   going up (1.5×, 2×). The sound device is open only while it plays.
//! - [`video`] — a video, a GIF, a round video message: the system's (hardware) decoder —
//!   AVFoundation on macOS, its frames as `CVPixelBuffer`s gpui draws without a copy (`surface`);
//!   Media Foundation on Windows, its frames as pixels. A dropped player lets its decoder go.
//! - [`fxa`] — animations of frames (an animated sticker rendered once by the plugin's helper):
//!   Flux's own file format, a frame read and decompressed at a time.
//! - [`sound`] — one sound at a time: whatever starts playing with sound pauses the one that
//!   played.
//!
//! Nothing here touches gpui or asks for frames: the app asks a playing player for its frame
//! (`VideoPlayer::frame`) as often as the video's rate needs, only while it is on screen, and a
//! player tells it of changes it didn't make (the end, another sound taking over) through
//! `on_change`.

pub mod fxa;
pub mod sound;
pub mod video;
pub mod voice;

use std::fmt;

/// Why a file can't be played.
#[derive(Debug)]
pub enum MediaError {
    /// The file can't be read.
    Io(std::io::Error),
    /// The file isn't what it should be: not OGG/Opus, a broken FXA, a video without a track.
    Format(String),
    /// The sound device: none, or it failed.
    Device(String),
    /// This system can't play it: a Windows without Media Foundation (an N edition without its
    /// media features, a server), a platform without a video backend. A format without a decoder
    /// here (HEVC on a Windows without the system's extension) is found once the file is read: a
    /// video's `error`.
    Unsupported(String),
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MediaError::Io(error) => write!(f, "{error}"),
            MediaError::Format(what) => write!(f, "not a playable file: {what}"),
            MediaError::Device(what) => write!(f, "sound device: {what}"),
            MediaError::Unsupported(what) => write!(f, "can't be played here: {what}"),
        }
    }
}

impl std::error::Error for MediaError {}

impl From<std::io::Error> for MediaError {
    fn from(error: std::io::Error) -> Self {
        MediaError::Io(error)
    }
}

/// Where a player is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Playback {
    Playing,
    /// Paused: by the app, or by another sound that took over.
    Paused,
    /// Played to the end (a looped one never is).
    Ended,
}

#[cfg(test)]
mod tests {
    /// The players go where the app keeps them: entities, tasks on other threads.
    #[test]
    fn players_are_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<crate::voice::VoicePlayer>();
        check::<crate::video::VideoPlayer>();
    }
}
