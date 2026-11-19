//! The sound device's side: the system's default output (cpal — CoreAudio, WASAPI) at its own
//! rate and format, fed from a short queue the player's thread keeps filled. The device's thread
//! only copies samples out (the voice's one channel into every channel) at the player's volume,
//! fades in at a start and out at a stop (and glides to a new volume) so nothing clicks, and says
//! how far the voice has played.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use crate::MediaError;

/// A fade in or out: 5 ms at 48 kHz.
const FADE: f32 = 240.0;

/// Samples at the device's rate (one channel) and the stretch of the voice they stand for, in its
/// samples at 48 kHz.
struct Chunk {
    samples: Vec<f32>,
    at: usize,
    from: f64,
    step: f64,
}

#[derive(Default)]
struct Queue {
    chunks: VecDeque<Chunk>,
    queued: usize,
    gain: f32,
    /// The volume the samples go out at now: it glides to the player's in a fade's time.
    level: f32,
    /// Fading out to a stop.
    stopping: bool,
}

/// What the device's thread and the player's share.
pub(crate) struct Shared {
    queue: Mutex<Queue>,
    /// Where the voice is — the sample the device plays now (f64 bits).
    position: AtomicU64,
    /// The fade out of a stop is over: the device plays silence.
    stopped: AtomicBool,
    /// The queue ran dry.
    starved: AtomicBool,
    /// The device failed or went away.
    failed: AtomicBool,
    /// The player's volume, 0.0–1.0 (f32 bits).
    volume: AtomicU32,
}

impl Shared {
    pub fn position(&self) -> f64 {
        f64::from_bits(self.position.load(Ordering::Relaxed))
    }

    pub fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    pub fn starved(&self) -> bool {
        self.starved.load(Ordering::Acquire)
    }

    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    /// The device's samples queued, not yet played.
    pub fn queued(&self) -> usize {
        self.queue.lock().unwrap().queued
    }

    pub fn push(&self, samples: Vec<f32>, from: f64, to: f64) {
        if samples.is_empty() {
            return;
        }
        let step = (to - from) / samples.len() as f64;
        let mut queue = self.queue.lock().unwrap();
        queue.queued += samples.len();
        queue.chunks.push_back(Chunk {
            samples,
            at: 0,
            from,
            step,
        });
        self.starved.store(false, Ordering::Release);
    }

    /// Drops what is queued (a seek): the sound fades in again from what comes next.
    pub fn clear(&self, position: f64) {
        let mut queue = self.queue.lock().unwrap();
        queue.chunks.clear();
        queue.queued = 0;
        queue.gain = 0.0;
        self.position.store(position.to_bits(), Ordering::Relaxed);
    }

    /// The volume from now on, 0.0–1.0: the sound glides to it.
    pub fn set_volume(&self, volume: f32) {
        self.volume
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// Fades out; `stopped` says when it is silent.
    pub fn stop(&self) {
        self.queue.lock().unwrap().stopping = true;
    }

    /// The device's callback: the next `frames` of one channel.
    fn fill(&self, mut write: impl FnMut(f32) -> bool) {
        let Ok(mut queue) = self.queue.try_lock() else {
            // The player's thread has the queue for a moment: a few samples of silence.
            return;
        };
        let queue = &mut *queue;
        let volume = f32::from_bits(self.volume.load(Ordering::Relaxed));
        let mut position = None;
        loop {
            let target = if queue.stopping { 0.0 } else { 1.0 };
            let Some(chunk) = queue.chunks.front_mut() else {
                break;
            };
            if queue.stopping && queue.gain <= 0.0 {
                break;
            }
            let sample = chunk.samples[chunk.at];
            position = Some(chunk.from + chunk.at as f64 * chunk.step);
            chunk.at += 1;
            queue.queued -= 1;
            if chunk.at == chunk.samples.len() {
                position = Some(chunk.from + chunk.at as f64 * chunk.step);
                queue.chunks.pop_front();
            }
            queue.gain = if target > queue.gain {
                (queue.gain + 1.0 / FADE).min(1.0)
            } else {
                (queue.gain - 1.0 / FADE).max(0.0)
            };
            queue.level = if volume > queue.level {
                (queue.level + 1.0 / FADE).min(volume)
            } else {
                (queue.level - 1.0 / FADE).max(volume)
            };
            if !write(sample * queue.gain * queue.level) {
                break;
            }
        }
        if let Some(position) = position {
            self.position.store(position.to_bits(), Ordering::Relaxed);
        }
        if queue.stopping && (queue.gain <= 0.0 || queue.chunks.is_empty()) {
            self.stopped.store(true, Ordering::Release);
        } else if queue.chunks.is_empty() {
            self.starved.store(true, Ordering::Release);
        }
    }
}

/// The sound device, open while it lives.
pub(crate) struct Output {
    stream: cpal::Stream,
    pub shared: Arc<Shared>,
    /// The device's sample rate.
    pub rate: u32,
}

/// Whether the system has a sound device to play to.
pub(crate) fn available() -> Result<(), MediaError> {
    cpal::default_host()
        .default_output_device()
        .map(|_| ())
        .ok_or_else(|| MediaError::Device("no sound output".into()))
}

impl Output {
    /// Opens the default output at its own rate and `volume`; it plays what is pushed once
    /// started.
    pub fn open(position: f64, volume: f32) -> Result<Self, MediaError> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| MediaError::Device("no sound output".into()))?;
        let config = device
            .default_output_config()
            .map_err(|error| MediaError::Device(error.to_string()))?;
        let volume = volume.clamp(0.0, 1.0);
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                level: volume,
                ..Queue::default()
            }),
            position: AtomicU64::new(position.to_bits()),
            stopped: AtomicBool::new(false),
            starved: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            volume: AtomicU32::new(volume.to_bits()),
        });
        let rate = config.sample_rate();
        let stream = match config.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, config.config(), &shared),
            SampleFormat::I16 => build::<i16>(&device, config.config(), &shared),
            SampleFormat::U16 => build::<u16>(&device, config.config(), &shared),
            SampleFormat::I32 => build::<i32>(&device, config.config(), &shared),
            SampleFormat::F64 => build::<f64>(&device, config.config(), &shared),
            other => Err(MediaError::Device(format!("samples of {other}"))),
        }?;
        Ok(Output {
            stream,
            shared,
            rate,
        })
    }

    pub fn start(&self) -> Result<(), MediaError> {
        self.stream
            .play()
            .map_err(|error| MediaError::Device(error.to_string()))
    }
}

fn build<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream, MediaError> {
    let channels = usize::from(config.channels).max(1);
    let data = shared.clone();
    let errors = shared.clone();
    device
        .build_output_stream(
            config,
            move |output: &mut [T], _| {
                let mut frames = output.chunks_exact_mut(channels);
                data.fill(|sample| match frames.next() {
                    Some(frame) => {
                        frame.fill(T::from_sample(sample));
                        true
                    }
                    None => false,
                });
                for frame in frames {
                    frame.fill(T::EQUILIBRIUM);
                }
            },
            move |error| {
                // A rerouted stream keeps playing; anything else ends it.
                if error.kind() != cpal::ErrorKind::DeviceChanged {
                    errors.failed.store(true, Ordering::Release);
                }
            },
            None,
        )
        .map_err(|error| MediaError::Device(error.to_string()))
}

/// From 48 kHz to the device's rate: a cubic (Catmull-Rom) interpolation — speech has little
/// above the device's half rate, so no filter is needed.
pub(crate) struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// The last four input samples (the newest last).
    history: [f32; 4],
    /// Where the next output sample falls between `history[1]` and `history[2]`.
    phase: f64,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Option<Self> {
        (from != to).then(|| Resampler {
            step: f64::from(from) / f64::from(to),
            history: [0.0; 4],
            phase: 0.0,
        })
    }

    pub fn reset(&mut self) {
        self.history = [0.0; 4];
        self.phase = 0.0;
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        let mut output = Vec::with_capacity((input.len() as f64 / self.step) as usize + 2);
        for &sample in input {
            self.history = [self.history[1], self.history[2], self.history[3], sample];
            while self.phase < 1.0 {
                let [y0, y1, y2, y3] = self.history;
                let t = self.phase as f32;
                let value = y1
                    + 0.5
                        * t
                        * (y2 - y0
                            + t * (2.0 * y0 - 5.0 * y1 + 4.0 * y2 - y3
                                + t * (3.0 * (y1 - y2) + y3 - y0)));
                output.push(value);
                self.phase += self.step;
            }
            self.phase -= 1.0;
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 48 kHz to 44.1 kHz: as many samples as the rates say, and the same tone.
    #[test]
    fn resamples_a_tone() {
        let tone =
            |rate: f64, n: usize| (1000.0 * 2.0 * std::f64::consts::PI * n as f64 / rate).sin();
        let input: Vec<f32> = (0..48_000).map(|n| tone(48_000.0, n) as f32).collect();
        let mut resampler = Resampler::new(48_000, 44_100).unwrap();
        let mut output = Vec::new();
        for chunk in input.chunks(480) {
            output.extend(resampler.process(chunk));
        }
        assert!(
            (output.len() as i64 - 44_100).abs() <= 2,
            "{}",
            output.len()
        );
        // The output lags the input by two samples of 48 kHz: the interpolation's history.
        let lag = 2.0;
        let error = output[100..44_000]
            .iter()
            .enumerate()
            .map(|(index, sample)| {
                let n = (index + 100) as f64;
                let expected =
                    (1000.0 * 2.0 * std::f64::consts::PI * (n / 44_100.0 - lag / 48_000.0)).sin();
                (f64::from(*sample) - expected).abs()
            })
            .fold(0.0, f64::max);
        assert!(error < 0.01, "off by {error}");
        assert!(Resampler::new(48_000, 48_000).is_none());
    }
}
