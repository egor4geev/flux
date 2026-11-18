//! An OGG/Opus file's sound: its packets read one at a time (`ogg`) and decoded by libopus to
//! mono at 48 kHz — whatever the stream's channels, Opus decodes to what it is asked.
//!
//! Time is counted in samples at 48 kHz, as Opus's granule positions are: the decoder's first
//! `pre_skip` samples are its warming up, not sound, and the last page's granule position says
//! where the sound ends (the last packet's rest is padding). A seek reads the packets from the
//! start without decoding them (a voice message's packets are a few kilobytes a second) and
//! decodes from 80 ms before the place, as the format asks, so the decoder has settled there.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use ogg::reading::PacketReader;
use opus::{Channels, Decoder};

use crate::MediaError;

pub(crate) const RATE: u64 = 48_000;
/// Decoded before a seek's place and dropped: 80 ms.
const PRE_ROLL: u64 = 3_840;
/// The longest Opus packet: 120 ms.
const MAX_PACKET: usize = 5_760;

/// What the headers say.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Info {
    serial: u32,
    pre_skip: u64,
    /// The output gain, Q7.8 dB.
    gain: i16,
    /// The granule position of the sound's end: `pre_skip` plus its samples.
    end: u64,
}

impl Info {
    /// The sound's samples.
    pub fn length(&self) -> u64 {
        self.end.saturating_sub(self.pre_skip)
    }
}

/// Reads the headers and the last page's granule position.
pub(crate) fn probe(path: &Path) -> Result<Info, MediaError> {
    let mut file = File::open(path)?;
    let mut reader = PacketReader::new(BufReader::new(&mut file));
    let head = reader
        .read_packet()
        .map_err(|error| MediaError::Format(format!("OGG: {error}")))?
        .ok_or_else(|| MediaError::Format("an empty file".into()))?;
    let data = &head.data;
    if data.len() < 19 || &data[..8] != b"OpusHead" || data[8] & 0xf0 != 0 {
        return Err(MediaError::Format("not OGG/Opus".into()));
    }
    let serial = head.stream_serial();
    let pre_skip = u64::from(u16::from_le_bytes([data[10], data[11]]));
    let gain = i16::from_le_bytes([data[16], data[17]]);
    drop(reader);
    let end = last_granule(&mut file, serial)?;
    Ok(Info {
        serial,
        pre_skip,
        gain,
        end,
    })
}

/// The granule position of the stream's last page: found from the file's end.
fn last_granule(file: &mut File, serial: u32) -> Result<u64, MediaError> {
    let length = file.metadata()?.len();
    let mut tail = 64 * 1024;
    loop {
        let start = length.saturating_sub(tail);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        // A page's header: "OggS", version 0, flags, the granule position, the serial.
        let found = (0..bytes.len().saturating_sub(27)).rev().find_map(|at| {
            let header = &bytes[at..at + 27];
            let granule = u64::from_le_bytes(header[6..14].try_into().unwrap());
            (&header[..4] == b"OggS"
                && header[4] == 0
                && u32::from_le_bytes(header[14..18].try_into().unwrap()) == serial
                && granule != u64::MAX)
                .then_some(granule)
        });
        match found {
            Some(granule) => return Ok(granule),
            None if start == 0 => return Err(MediaError::Format("OGG: no audio pages".into())),
            None => tail *= 4,
        }
    }
}

pub(crate) struct Source {
    reader: PacketReader<BufReader<File>>,
    decoder: Decoder,
    info: Info,
    /// The granule position (a sample at 48 kHz, the pre-skip included) of the next packet's start.
    granule: u64,
    /// Decoded samples before this are dropped: the pre-skip, a seek's pre-roll.
    keep_from: u64,
    /// The packet a seek stopped at, decoded next.
    pending: Option<Vec<u8>>,
    buffer: Vec<f32>,
}

impl Source {
    pub fn open(path: &Path, info: Info) -> Result<Self, MediaError> {
        let mut decoder = Decoder::new(RATE as u32, Channels::Mono)
            .map_err(|error| MediaError::Format(format!("Opus: {error}")))?;
        if info.gain != 0 {
            let _ = decoder.set_gain(i32::from(info.gain));
        }
        let mut source = Source {
            reader: PacketReader::new(BufReader::new(File::open(path)?)),
            decoder,
            info,
            granule: 0,
            keep_from: 0,
            pending: None,
            buffer: vec![0.0; MAX_PACKET],
        };
        source.seek(0)?;
        Ok(source)
    }

    /// The next sound comes from `sample` (of the sound, the pre-skip not counted).
    pub fn seek(&mut self, sample: u64) -> Result<(), MediaError> {
        self.reader
            .seek_bytes(SeekFrom::Start(0))
            .map_err(|error| MediaError::Format(format!("OGG: {error}")))?;
        // The two headers.
        for _ in 0..2 {
            self.next_packet()?;
        }
        let target = (sample + self.info.pre_skip).min(self.info.end);
        let start = target.saturating_sub(PRE_ROLL);
        self.granule = 0;
        self.keep_from = target;
        self.pending = None;
        let _ = self.decoder.reset_state();
        // Packets that end before the pre-roll aren't decoded.
        while let Some(packet) = self.next_packet()? {
            let samples = opus::packet::get_nb_samples(&packet, RATE as u32).unwrap_or(0) as u64;
            if self.granule + samples > start {
                self.pending = Some(packet);
                break;
            }
            self.granule += samples;
        }
        Ok(())
    }

    fn next_packet(&mut self) -> Result<Option<Vec<u8>>, MediaError> {
        loop {
            match self.reader.read_packet() {
                Ok(Some(packet)) if packet.stream_serial() == self.info.serial => {
                    return Ok(Some(packet.data));
                }
                Ok(Some(_)) => continue,
                Ok(None) => return Ok(None),
                Err(error) => return Err(MediaError::Format(format!("OGG: {error}"))),
            }
        }
    }

    /// Decodes the next packet's sound onto `output`; `false` at the end.
    pub fn decode(&mut self, output: &mut Vec<f32>) -> Result<bool, MediaError> {
        if self.granule >= self.info.end {
            return Ok(false);
        }
        let Some(packet) = self
            .pending
            .take()
            .map_or_else(|| self.next_packet(), |packet| Ok(Some(packet)))?
        else {
            return Ok(false);
        };
        // A broken packet is skipped as a lost one would be: the decoder fills its time.
        let decoded = match self.decoder.decode_float(&packet, &mut self.buffer, false) {
            Ok(samples) => samples,
            Err(_) => {
                let samples = opus::packet::get_nb_samples(&packet, RATE as u32).unwrap_or(960);
                self.decoder
                    .decode_float(&[], &mut self.buffer[..samples.min(MAX_PACKET)], false)
                    .unwrap_or(0)
            }
        };
        let start = self.granule;
        self.granule += decoded as u64;
        let from = self.keep_from.saturating_sub(start).min(decoded as u64) as usize;
        let to = (self.info.end.saturating_sub(start)).min(decoded as u64) as usize;
        if from < to {
            output.extend_from_slice(&self.buffer[from..to]);
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/voice.ogg")
    }

    fn decode_all(source: &mut Source) -> Vec<f32> {
        let mut samples = Vec::new();
        while source.decode(&mut samples).unwrap() {}
        samples
    }

    #[test]
    fn decodes_to_the_last_granule() {
        let info = probe(&fixture()).unwrap();
        // 1.77 s of speech (`make_voice`): the encoder's look-ahead is skipped.
        assert_eq!(info.pre_skip, 312);
        assert_eq!(info.length(), 85_141);
        let mut source = Source::open(&fixture(), info).unwrap();
        let samples = decode_all(&mut source);
        assert_eq!(samples.len() as u64, info.length());
        let peak = samples
            .iter()
            .map(|sample| sample.abs())
            .fold(0.0, f32::max);
        assert!(peak > 0.1, "speech, not silence: {peak}");
    }

    /// A seek lands on the very sample, and the pre-roll settles the decoder: what follows is
    /// what a decoding from the start gives there.
    #[test]
    fn seeks_to_the_sample() {
        let info = probe(&fixture()).unwrap();
        let mut source = Source::open(&fixture(), info).unwrap();
        let whole = decode_all(&mut source);
        for at in [0, 960, 30_000, 48_000, 85_000] {
            source.seek(at).unwrap();
            let rest = decode_all(&mut source);
            assert_eq!(rest.len(), whole.len() - at as usize, "from {at}");
            let difference = rest
                .iter()
                .zip(&whole[at as usize..])
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f32::max);
            assert!(difference < 0.02, "from {at}: off by {difference}");
        }
    }

    #[test]
    fn not_opus() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny.mp4");
        assert!(matches!(probe(&path), Err(MediaError::Format(_))));
    }
}
