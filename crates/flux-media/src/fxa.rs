//! FXA — Flux's file of an animation's frames: an animated sticker (Lottie, a VP9 video) rendered
//! once, at the size it is shown, by the plugin's helper; Flux plays it a frame at a time, reading
//! and decompressing the next one off the window's thread, so only the frames in sight are in
//! memory and playing costs no rendering.
//!
//! The layout, little-endian:
//!
//! | offset | size | what |
//! |---|---|---|
//! | 0 | 4 | `FXA1` |
//! | 4 | 2 | width, pixels |
//! | 6 | 2 | height, pixels |
//! | 8 | 4 | the frames' count, *n* ≥ 1 |
//! | 12 | 4 | a frame's length, microseconds (33 333 — 30 frames a second) |
//! | 16 | 4 | flags: 0 (reserved) |
//! | 20 | 4 × (*n* + 1) | each frame's offset from the file's start, then the end of the last |
//! | … | | the frames: each an LZ4 block (the block format, no size before it) of width × height × 4 bytes — BGRA, straight (not premultiplied) alpha, rows top to bottom |
//!
//! A writer writes the whole file to `<name>.tmp` and renames it, so a reader never sees half of
//! one. The plugin's helper has its own writer of the same layout (`fluxgram-helper`).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use crate::MediaError;

pub const MAGIC: &[u8; 4] = b"FXA1";
/// The header before the offsets.
const HEADER: usize = 20;
/// No more frames than this: a 60-second animation at 60 frames a second.
const MAX_FRAMES: u32 = 3600;

/// An animation file open for reading.
pub struct Animation {
    file: Mutex<File>,
    width: u32,
    height: u32,
    frame_duration: Duration,
    /// The frames' offsets, then the end of the last.
    offsets: Vec<u32>,
}

impl Animation {
    /// Reads the header and the offsets, checks them against the file's length.
    pub fn open(path: &Path) -> Result<Self, MediaError> {
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        let mut header = [0u8; HEADER];
        file.read_exact(&mut header)
            .map_err(|_| MediaError::Format("FXA: too short".into()))?;
        if &header[0..4] != MAGIC {
            return Err(MediaError::Format("not an FXA file".into()));
        }
        let u16_at = |at: usize| u16::from_le_bytes([header[at], header[at + 1]]) as u32;
        let u32_at = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
        let (width, height, count) = (u16_at(4), u16_at(6), u32_at(8));
        let micros = u32_at(12);
        if width == 0 || height == 0 || count == 0 || count > MAX_FRAMES || micros == 0 {
            return Err(MediaError::Format(format!(
                "FXA: {width}×{height}, {count} frames of {micros} µs"
            )));
        }
        let mut table = vec![0u8; 4 * (count as usize + 1)];
        file.read_exact(&mut table)
            .map_err(|_| MediaError::Format("FXA: the offsets are cut".into()))?;
        let offsets: Vec<u32> = table
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| u32::from_le_bytes(*bytes))
            .collect();
        let start = (HEADER + table.len()) as u32;
        let ordered = offsets.windows(2).all(|pair| pair[0] < pair[1]);
        if offsets[0] != start || !ordered || u64::from(*offsets.last().unwrap()) > length {
            return Err(MediaError::Format("FXA: the offsets are broken".into()));
        }
        Ok(Animation {
            file: Mutex::new(file),
            width,
            height,
            frame_duration: Duration::from_micros(micros.into()),
            offsets,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn frame_count(&self) -> u32 {
        self.offsets.len() as u32 - 1
    }

    /// How long a frame stays on screen.
    pub fn frame_duration(&self) -> Duration {
        self.frame_duration
    }

    /// The whole animation's length.
    pub fn duration(&self) -> Duration {
        self.frame_duration * self.frame_count()
    }

    /// The bytes of a frame's pixels: width × height × 4.
    pub fn frame_size(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    /// Frame `index` (wrapping past the last): its pixels — BGRA, straight alpha, rows top to
    /// bottom — into `pixels` (resized to [`Self::frame_size`]). Reads the file: call it off the
    /// window's thread.
    pub fn read_frame(&self, index: u32, pixels: &mut Vec<u8>) -> Result<(), MediaError> {
        let index = (index % self.frame_count()) as usize;
        let (start, end) = (self.offsets[index], self.offsets[index + 1]);
        let mut block = vec![0u8; (end - start) as usize];
        {
            let mut file = self.file.lock().unwrap();
            file.seek(SeekFrom::Start(start.into()))?;
            file.read_exact(&mut block)?;
        }
        pixels.resize(self.frame_size(), 0);
        let written = lz4_flex::block::decompress_into(&block, pixels)
            .map_err(|error| MediaError::Format(format!("FXA frame {index}: {error}")))?;
        if written != pixels.len() {
            return Err(MediaError::Format(format!("FXA frame {index}: cut")));
        }
        Ok(())
    }
}

/// Writes an animation: frames one by one, the file at [`Writer::finish`].
pub struct Writer {
    path: PathBuf,
    width: u32,
    height: u32,
    frame_duration: Duration,
    blocks: Vec<Vec<u8>>,
}

impl Writer {
    /// An animation of `width` × `height` pixels (each up to 65 535), a frame every
    /// `frame_duration`.
    pub fn new(path: &Path, width: u32, height: u32, frame_duration: Duration) -> Self {
        Writer {
            path: path.to_path_buf(),
            width,
            height,
            frame_duration,
            blocks: Vec::new(),
        }
    }

    /// The next frame: width × height × 4 bytes, BGRA, straight alpha.
    pub fn push(&mut self, bgra: &[u8]) -> Result<(), MediaError> {
        let size = self.width as usize * self.height as usize * 4;
        if bgra.len() != size {
            return Err(MediaError::Format(format!(
                "FXA: a frame of {} bytes, {size} expected",
                bgra.len()
            )));
        }
        self.blocks.push(lz4_flex::block::compress(bgra));
        Ok(())
    }

    /// Writes the file (to `<name>.tmp`, then renamed over the path).
    pub fn finish(self) -> Result<(), MediaError> {
        let count = self.blocks.len();
        if count == 0 || count > MAX_FRAMES as usize {
            return Err(MediaError::Format(format!("FXA: {count} frames")));
        }
        let (Ok(width), Ok(height)) = (u16::try_from(self.width), u16::try_from(self.height))
        else {
            return Err(MediaError::Format("FXA: too large".into()));
        };
        let micros = u32::try_from(self.frame_duration.as_micros())
            .ok()
            .filter(|micros| *micros > 0)
            .ok_or_else(|| MediaError::Format("FXA: a frame's length".into()))?;
        let mut out = Vec::with_capacity(
            HEADER + 4 * (count + 1) + self.blocks.iter().map(Vec::len).sum::<usize>(),
        );
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.extend_from_slice(&(count as u32).to_le_bytes());
        out.extend_from_slice(&micros.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        let mut offset = (HEADER + 4 * (count + 1)) as u64;
        for block in self.blocks.iter() {
            out.extend_from_slice(&(offset as u32).to_le_bytes());
            offset += block.len() as u64;
        }
        if offset > u64::from(u32::MAX) {
            return Err(MediaError::Format("FXA: over 4 GB".into()));
        }
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        for block in &self.blocks {
            out.extend_from_slice(block);
        }
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(".tmp");
        let temp = self.path.with_file_name(name);
        let mut file = File::create(&temp)?;
        file.write_all(&out)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_frames_back() {
        let dir = std::env::temp_dir().join(format!("flux-fxa-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.fxa");
        let mut writer = Writer::new(&path, 3, 2, Duration::from_micros(33_333));
        let frames: Vec<Vec<u8>> = (0..4u8)
            .map(|frame| (0..24).map(|byte| byte * frame).collect())
            .collect();
        for frame in &frames {
            writer.push(frame).unwrap();
        }
        assert!(writer.push(&[0; 5]).is_err());
        writer.finish().unwrap();

        let animation = Animation::open(&path).unwrap();
        assert_eq!((animation.width(), animation.height()), (3, 2));
        assert_eq!(animation.frame_count(), 4);
        assert_eq!(animation.frame_duration(), Duration::from_micros(33_333));
        let mut pixels = Vec::new();
        for (index, frame) in frames.iter().enumerate() {
            animation.read_frame(index as u32, &mut pixels).unwrap();
            assert_eq!(&pixels, frame);
        }
        animation.read_frame(5, &mut pixels).unwrap();
        assert_eq!(pixels, frames[1], "past the last frame it wraps");

        std::fs::write(&path, b"FXA1 broken").unwrap();
        assert!(Animation::open(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
