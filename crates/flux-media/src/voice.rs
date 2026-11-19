//! A voice message's player: an OGG/Opus file demuxed (`ogg`) and decoded (libopus) a little
//! ahead of the sound device, out through the system's sound (cpal: CoreAudio, WASAPI) at its
//! volume; a speed above 1× stretches the time, not the pitch (WSOLA tuned for speech,
//! [`stretch`]). The device is
//! open only while it plays; playing takes the sound ([`crate::sound`]), and another sound taking
//! over pauses it.
//!
//! While it plays, a thread of its own decodes ~150 ms ahead of the device (a packet of 20 ms at a
//! time — never the whole file), stretches the sound to the speed, resamples it to the device's
//! rate if that isn't 48 kHz, and wakes ~25 times a second to top the queue up; paused, there is
//! no thread and no device.

mod output;
mod source;
mod stretch;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::sound::{self, Sound};
use crate::{MediaError, Playback};
use output::{Output, Resampler};
use source::{Info, RATE, Source};
use stretch::Stretch;

/// The speeds the speed button goes round.
pub const SPEEDS: [f32; 3] = [1.0, 1.5, 2.0];

/// How far ahead of the device the sound is decoded.
const AHEAD: Duration = Duration::from_millis(150);
/// How often the playing thread tops the queue up.
const TOP_UP: Duration = Duration::from_millis(40);

/// A voice message open for playing. Dropping it stops the sound and closes the device.
pub struct VoicePlayer {
    shared: Arc<Shared>,
}

/// What the player, its playing thread and [`crate::sound`] share.
struct Shared {
    path: PathBuf,
    info: Info,
    control: Mutex<Control>,
    /// Wakes the playing thread: a pause, a seek, a new speed.
    wake: Condvar,
    on_change: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

struct Control {
    state: Playback,
    speed: f32,
    /// 0.0–1.0, the samples' scale.
    volume: f32,
    /// Where it is, in the voice's samples at 48 kHz, while nothing plays it.
    position: f64,
    /// The playing thread's number: a newer one (or none playing) tells it to stop.
    run: u64,
    /// The device's side of the playing thread: where it plays now.
    device: Option<Arc<output::Shared>>,
    /// A seek the playing thread hasn't taken yet.
    seek: Option<f64>,
}

impl VoicePlayer {
    /// Opens an OGG/Opus file: reads its headers and its length; nothing plays yet.
    pub fn open(path: &Path) -> Result<Self, MediaError> {
        let info = source::probe(path)?;
        Ok(VoicePlayer {
            shared: Arc::new(Shared {
                path: path.to_path_buf(),
                info,
                control: Mutex::new(Control {
                    state: Playback::Paused,
                    speed: 1.0,
                    volume: 1.0,
                    position: 0.0,
                    run: 0,
                    device: None,
                    seek: None,
                }),
                wake: Condvar::new(),
                on_change: Mutex::new(None),
            }),
        })
    }

    /// Plays from where it is (from the start once it ended): opens the sound device and takes
    /// the sound — whatever else played with sound pauses.
    pub fn play(&self) -> Result<(), MediaError> {
        output::available()?;
        {
            let mut control = self.shared.control.lock().unwrap();
            if control.state == Playback::Playing {
                return Ok(());
            }
            if control.state == Playback::Ended {
                control.position = 0.0;
            }
            control.state = Playback::Playing;
            control.run += 1;
            control.seek = None;
            let (run, start) = (control.run, control.position);
            let shared = self.shared.clone();
            std::thread::Builder::new()
                .name("voice".into())
                .spawn(move || shared.play(run, start))?;
        }
        sound::take(&self.sound());
        Ok(())
    }

    /// Pauses where it is; the sound device closes.
    pub fn pause(&self) {
        if self.shared.pause() {
            sound::release(&self.sound());
        }
    }

    /// Goes to `to` (clamped to the length), playing or not.
    pub fn seek(&self, to: Duration) {
        let to = (to.as_secs_f64() * RATE as f64).min(self.shared.info.length() as f64);
        let mut control = self.shared.control.lock().unwrap();
        match control.state {
            Playback::Playing => {
                control.seek = Some(to);
                drop(control);
                self.shared.wake.notify_all();
            }
            Playback::Paused | Playback::Ended => {
                control.state = Playback::Paused;
                control.position = to;
            }
        }
    }

    /// 1.0 — as recorded; 1.5, 2.0 — faster at the same pitch (0.5–3.0 work).
    pub fn set_speed(&self, speed: f32) {
        self.shared.control.lock().unwrap().speed = speed.clamp(0.5, 3.0);
        self.shared.wake.notify_all();
    }

    pub fn speed(&self) -> f32 {
        self.shared.control.lock().unwrap().speed
    }

    /// 0.0 (silent) – 1.0 (as recorded), the samples' scale: playing, the sound glides to it.
    pub fn set_volume(&self, volume: f32) {
        let mut control = self.shared.control.lock().unwrap();
        control.volume = volume.clamp(0.0, 1.0);
        if let Some(device) = &control.device {
            device.set_volume(control.volume);
        }
    }

    pub fn volume(&self) -> f32 {
        self.shared.control.lock().unwrap().volume
    }

    /// Where it is, at the speed's scale: 0:12 of 0:42 whatever the speed.
    pub fn position(&self) -> Duration {
        let samples = self.shared.control.lock().unwrap().position();
        Duration::from_secs_f64(samples.max(0.0) / RATE as f64).min(self.duration())
    }

    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.shared.info.length() as f64 / RATE as f64)
    }

    pub fn state(&self) -> Playback {
        self.shared.control.lock().unwrap().state
    }

    /// Called on another thread when the state changes by itself: the end, another sound took
    /// over (on the thread that started the other sound), the device went away. The app repaints
    /// and plays the next voice message.
    pub fn on_change(&self, callback: impl Fn() + Send + Sync + 'static) {
        *self.shared.on_change.lock().unwrap() = Some(Arc::new(callback));
    }

    fn sound(&self) -> Arc<dyn Sound> {
        self.shared.clone()
    }
}

impl Drop for VoicePlayer {
    fn drop(&mut self) {
        self.pause();
    }
}

impl Control {
    fn position(&self) -> f64 {
        match (self.seek, &self.device) {
            (Some(seek), _) => seek,
            (None, Some(device)) if self.state == Playback::Playing => device.position(),
            _ => self.position,
        }
    }
}

/// Why a playing thread stopped.
enum Stop {
    /// Paused, dropped, or a newer thread plays.
    Told,
    /// Played to the end.
    Ended,
}

impl Shared {
    /// Pauses a playing one; `false` if it didn't play.
    fn pause(&self) -> bool {
        let mut control = self.control.lock().unwrap();
        if control.state != Playback::Playing {
            return false;
        }
        control.position = control.position();
        control.state = Playback::Paused;
        control.device = None;
        control.seek = None;
        control.run += 1;
        drop(control);
        self.wake.notify_all();
        true
    }

    fn changed(&self) {
        let callback = self.on_change.lock().unwrap().clone();
        if let Some(callback) = callback {
            callback();
        }
    }

    /// The playing thread: from `start` until told to stop, the end or a failure.
    fn play(self: Arc<Self>, run: u64, start: f64) {
        let stop = self.run(run, start);
        let mut control = self.control.lock().unwrap();
        if control.run != run {
            return;
        }
        // It stopped by itself.
        let position = control.position();
        control.state = match stop {
            Ok(Stop::Ended) => Playback::Ended,
            _ => Playback::Paused,
        };
        control.position = match stop {
            Ok(Stop::Ended) => self.info.length() as f64,
            _ => position,
        };
        control.device = None;
        control.seek = None;
        control.run += 1;
        drop(control);
        if let Err(error) = stop {
            eprintln!("flux-media: a voice message stopped: {error}");
        }
        let sound: Arc<dyn Sound> = self.clone();
        sound::release(&sound);
        self.changed();
    }

    fn run(&self, run: u64, start: f64) -> Result<Stop, MediaError> {
        let mut source = Source::open(&self.path, self.info)?;
        source.seek(start as u64)?;
        let volume = self.control.lock().unwrap().volume;
        let output = Output::open(start, volume)?;
        let mut resampler = Resampler::new(RATE as u32, output.rate);
        let ahead = (AHEAD.as_secs_f64() * f64::from(output.rate)) as usize;
        let mut stretch = Stretch::new(self.control.lock().unwrap().speed);
        // The voice's sample the stretch's input starts at.
        let mut offset = start;
        let mut decoded = Vec::new();
        let mut finished = false;
        let mut started = false;
        {
            let mut control = self.control.lock().unwrap();
            if control.run != run {
                return Ok(Stop::Told);
            }
            // The volume may have changed while the device opened.
            output.shared.set_volume(control.volume);
            control.device = Some(output.shared.clone());
        }
        let stop = loop {
            let (speed, seek) = {
                let mut control = self.control.lock().unwrap();
                if control.run != run {
                    break Stop::Told;
                }
                (control.speed, control.seek.take())
            };
            if let Some(to) = seek {
                source.seek(to as u64)?;
                stretch = Stretch::new(speed);
                offset = to;
                finished = false;
                if let Some(resampler) = &mut resampler {
                    resampler.reset();
                }
                output.shared.clear(to);
            }
            stretch.set_speed(speed);
            if output.shared.failed() {
                return Err(MediaError::Device("the sound output went away".into()));
            }
            let mut all_out = false;
            while output.shared.queued() < ahead {
                if let Some(piece) = stretch.pull() {
                    let samples = match &mut resampler {
                        Some(resampler) => resampler.process(&piece.samples),
                        None => piece.samples,
                    };
                    output
                        .shared
                        .push(samples, offset + piece.from, offset + piece.to);
                } else if finished {
                    all_out = true;
                    break;
                } else {
                    decoded.clear();
                    if source.decode(&mut decoded)? {
                        stretch.push(&decoded);
                    } else {
                        finished = true;
                        stretch.finish();
                    }
                }
            }
            if !started {
                output.start()?;
                started = true;
            }
            if all_out && output.shared.starved() {
                break Stop::Ended;
            }
            let control = self.control.lock().unwrap();
            if control.run != run {
                break Stop::Told;
            }
            let _ = self.wake.wait_timeout(control, TOP_UP).unwrap();
        };
        // Fade out (at most 50 ms), then the device closes.
        output.shared.stop();
        let fading = Instant::now();
        while !output.shared.stopped() && fading.elapsed() < Duration::from_millis(50) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(stop)
    }
}

impl Sound for Shared {
    fn pause_for_another(&self) {
        if self.pause() {
            self.changed();
        }
    }
}
