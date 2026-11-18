//! Makes a voice message — OGG/Opus, mono, 48 kHz, as Telegram's are — from a WAV of 16-bit mono
//! PCM at 48 kHz: the samples of FluxGram part 4 and the test fixture.
//!
//! ```sh
//! say -v Milena -o voice.wav --data-format=LEI16@48000 "Привет! Это голосовое сообщение."
//! cargo run -p flux-media --example make_voice -- voice.wav voice.ogg
//! ```

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;

use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use opus::{Application, Bitrate, Channels, Encoder};

/// 20 ms at 48 kHz.
const FRAME: usize = 960;
/// Packets on a page: a page a second, as recorders write them.
const PAGE: usize = 50;

fn main() {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let [input, output] = args.as_slice() else {
        eprintln!("usage: make_voice <in.wav: 16-bit mono 48 kHz> <out.ogg>");
        std::process::exit(2);
    };
    let samples = read_wav(input);
    let mut encoder = Encoder::new(48_000, Channels::Mono, Application::Voip).unwrap();
    encoder.set_bitrate(Bitrate::Bits(32_000)).unwrap();
    let pre_skip = encoder.get_lookahead().unwrap() as u16;

    let serial = 0x464c_5558; // "FLUX"
    let mut writer = PacketWriter::new(BufWriter::new(File::create(output).unwrap()));
    let mut head = b"OpusHead".to_vec();
    head.push(1); // version
    head.push(1); // channels
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&48_000u32.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes()); // output gain
    head.push(0); // mapping family: mono or stereo
    writer
        .write_packet(head, serial, PacketWriteEndInfo::EndPage, 0)
        .unwrap();
    let vendor = b"flux-media make_voice";
    let mut tags = b"OpusTags".to_vec();
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0u32.to_le_bytes()); // no comments
    writer
        .write_packet(tags, serial, PacketWriteEndInfo::EndPage, 0)
        .unwrap();

    // The encoder's output lags its input by the pre-skip: silence after the end lets the last
    // of the sound out.
    let frames = (samples.len() + usize::from(pre_skip)).div_ceil(FRAME);
    let mut packet = vec![0u8; 4000];
    for index in 0..frames {
        let mut frame = [0i16; FRAME];
        let start = (index * FRAME).min(samples.len());
        let end = (start + FRAME).min(samples.len());
        frame[..end - start].copy_from_slice(&samples[start..end]);
        let length = encoder.encode(&frame, &mut packet).unwrap();
        let last = index + 1 == frames;
        // The granule position of a packet's end: the samples decoded so far, the pre-skip
        // included; the last one's says where the sound ends (the rest is padding).
        let granule = if last {
            u64::from(pre_skip) + samples.len() as u64
        } else {
            ((index + 1) * FRAME) as u64
        };
        let end_info = if last {
            PacketWriteEndInfo::EndStream
        } else if (index + 1) % PAGE == 0 {
            PacketWriteEndInfo::EndPage
        } else {
            PacketWriteEndInfo::NormalPacket
        };
        writer
            .write_packet(packet[..length].to_vec(), serial, end_info, granule)
            .unwrap();
    }
    eprintln!(
        "{}: {:.2} s, {} packets",
        output.display(),
        samples.len() as f64 / 48_000.0,
        frames
    );
}

/// The samples of a canonical WAV file: 16-bit mono PCM at 48 kHz.
fn read_wav(path: &PathBuf) -> Vec<i16> {
    let bytes = std::fs::read(path).unwrap();
    assert!(
        &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "not a WAV file"
    );
    let mut at = 12;
    let mut format = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = &bytes[at + 8..(at + 8 + size).min(bytes.len())];
        if id == b"fmt " {
            let channels = u16::from_le_bytes([body[2], body[3]]);
            let rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            let bits = u16::from_le_bytes([body[14], body[15]]);
            format = Some((channels, rate, bits));
        } else if id == b"data" {
            assert_eq!(format, Some((1, 48_000, 16)), "16-bit mono 48 kHz only");
            return body
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| i16::from_le_bytes(*pair))
                .collect();
        }
        at += 8 + size + (size & 1);
    }
    panic!("no data in the WAV file");
}
