//! Windows: Media Foundation's `IMFMediaEngine` plays the file, its frames transferred to BGRA
//! pixels (`Frame::Pixels`) — agent W builds it. Until then nothing plays: `open` says
//! `Unsupported`, and the app shows the poster.

pub(super) use super::windows_stub::Player;
