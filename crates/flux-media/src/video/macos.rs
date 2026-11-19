//! macOS: AVFoundation plays the file — an `AVPlayer` with an `AVPlayerItem`, its frames pulled
//! from an `AVPlayerItemVideoOutput` as `CVPixelBuffer`s in the format gpui's Metal `surface`
//! draws (`420f`: biplanar Y′CbCr 4:2:0, full range, IOSurface-backed), so a frame goes from the
//! hardware decoder to the screen without a copy. The sound goes out through AVFoundation.
//!
//! AVFoundation's player is main-thread-only: the player is driven on the main thread — a call
//! from elsewhere goes there through the main queue — and its notifications (the end, a
//! failure) come on the main queue. The file's tracks are read off it (`loadValuesAsynchronously`),
//! which tells the app the size and the length. `frame` only asks the output, from any thread.

use std::cell::Cell;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use block2::RcBlock;
use core_foundation::base::TCFType;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_av_foundation::{
    AVAsynchronousKeyValueLoading, AVMediaTypeVideo, AVPlayer, AVPlayerActionAtItemEnd,
    AVPlayerItem, AVPlayerItemDidPlayToEndTimeNotification,
    AVPlayerItemFailedToPlayToEndTimeNotification, AVPlayerItemVideoOutput, AVURLAsset,
};
use objc2_core_foundation::CFString;
use objc2_core_media::CMTime;
use objc2_core_video::{
    kCVPixelBufferMetalCompatibilityKey, kCVPixelBufferPixelFormatTypeKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
};
use objc2_foundation::{
    NSArray, NSDictionary, NSNotification, NSNotificationCenter, NSNumber, NSObjectProtocol,
    NSOperationQueue, NSString, NSURL, ns_string,
};

use super::{Frame, VideoOptions};
use crate::sound::{self, Sound};
use crate::{MediaError, Playback};

/// AVFoundation's objects: touched on the main thread only (`Player::main`), except the output.
struct Av {
    player: Retained<AVPlayer>,
    item: Retained<AVPlayerItem>,
    output: Retained<AVPlayerItemVideoOutput>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
}

// SAFETY: the player and the item are only used on the main thread (`Shared::main` checks for it
// and sends the work there otherwise); the video output is safe on any thread, as AVFoundation
// pulls frames from a display link's thread.
unsafe impl Send for Av {}
unsafe impl Sync for Av {}

struct State {
    playback: Playback,
    muted: bool,
    looped: bool,
    speed: f32,
    size: Option<(u32, u32)>,
    duration: Duration,
    frame_interval: Duration,
    error: Option<String>,
}

/// What the player, AVFoundation's callbacks and [`crate::sound`] share.
struct Shared {
    av: Mutex<Option<Arc<Av>>>,
    state: Mutex<State>,
    on_change: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

pub(super) struct Player {
    shared: Arc<Shared>,
    av: Arc<Av>,
}

impl Player {
    pub fn open(path: &Path, options: VideoOptions) -> Result<Self, MediaError> {
        let mtm = MainThreadMarker::new().ok_or_else(|| {
            MediaError::Unsupported("a video is opened on the main thread".into())
        })?;
        if !path.is_file() {
            return Err(MediaError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{} not found", path.display()),
            )));
        }
        let path = NSString::from_str(&path.to_string_lossy());
        // SAFETY: plain AVFoundation calls with valid objects, on the main thread.
        let av = unsafe {
            let url = NSURL::fileURLWithPath(&path);
            let asset = AVURLAsset::URLAssetWithURL_options(&url, None);
            let item = AVPlayerItem::playerItemWithAsset(&asset, mtm);
            // gpui's Metal renderer draws `420f` buffers it can take as Metal textures.
            let format = NSNumber::new_u32(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange);
            let metal = NSNumber::new_bool(true);
            let keys: [&NSString; 2] = [
                cf_key(kCVPixelBufferPixelFormatTypeKey),
                cf_key(kCVPixelBufferMetalCompatibilityKey),
            ];
            let values: [&AnyObject; 2] = [&format, &metal];
            let attributes = NSDictionary::from_slices(&keys, &values);
            let output = AVPlayerItemVideoOutput::initWithPixelBufferAttributes(
                AVPlayerItemVideoOutput::alloc(),
                Some(&attributes),
            );
            item.addOutput(&output);
            let player = AVPlayer::playerWithPlayerItem(Some(&item), mtm);
            player.setMuted(options.muted);
            // A local file: no waiting to buffer before playing.
            player.setAutomaticallyWaitsToMinimizeStalling(false);
            // A GIF or a round message in the feed doesn't keep the display awake.
            player.setPreventsDisplaySleepDuringVideoPlayback(!options.muted);
            player.setActionAtItemEnd(if options.looped {
                AVPlayerActionAtItemEnd::None
            } else {
                AVPlayerActionAtItemEnd::Pause
            });
            (asset, item, output, player)
        };
        let (asset, item, output, player) = av;
        let shared = Arc::new(Shared {
            av: Mutex::new(None),
            state: Mutex::new(State {
                playback: Playback::Paused,
                muted: options.muted,
                looped: options.looped,
                speed: 1.0,
                size: None,
                duration: Duration::ZERO,
                frame_interval: Duration::from_millis(33),
                error: None,
            }),
            on_change: Mutex::new(None),
        });

        // The end and a failure, on the main queue.
        let center = NSNotificationCenter::defaultCenter();
        let main_queue = NSOperationQueue::mainQueue();
        let mut observers = Vec::new();
        let weak = Arc::downgrade(&shared);
        let ended = RcBlock::new(move |_: NonNull<NSNotification>| {
            if let Some(shared) = weak.upgrade() {
                shared.ended();
            }
        });
        let weak = Arc::downgrade(&shared);
        let failed = RcBlock::new(move |_: NonNull<NSNotification>| {
            if let Some(shared) = weak.upgrade() {
                shared.failed("the video can't be played to its end".into());
            }
        });
        // SAFETY: the item is a valid object; the observers are removed when the player goes.
        unsafe {
            observers.push(center.addObserverForName_object_queue_usingBlock(
                Some(AVPlayerItemDidPlayToEndTimeNotification),
                Some(&item),
                Some(&main_queue),
                &ended,
            ));
            observers.push(center.addObserverForName_object_queue_usingBlock(
                Some(AVPlayerItemFailedToPlayToEndTimeNotification),
                Some(&item),
                Some(&main_queue),
                &failed,
            ));
        }

        // The tracks, read off the main thread: the size, the length, the frame rate.
        let weak = Arc::downgrade(&shared);
        let loaded_asset = asset.clone();
        let loaded = RcBlock::new(move || {
            if let Some(shared) = weak.upgrade() {
                shared.loaded(&loaded_asset);
            }
        });
        let keys = NSArray::from_slice(&[ns_string!("tracks"), ns_string!("duration")]);
        // SAFETY: the block holds what it uses; AVFoundation calls it once, on its own thread.
        unsafe { asset.loadValuesAsynchronouslyForKeys_completionHandler(&keys, Some(&loaded)) };

        let av = Arc::new(Av {
            player,
            item,
            output,
            observers,
        });
        *shared.av.lock().unwrap() = Some(av.clone());
        Ok(Player { shared, av })
    }

    pub fn play(&self) -> Result<(), MediaError> {
        let (speed, muted) = {
            let mut state = self.shared.state.lock().unwrap();
            if let Some(error) = &state.error {
                return Err(MediaError::Format(error.clone()));
            }
            if state.playback == Playback::Playing {
                return Ok(());
            }
            let from_start = state.playback == Playback::Ended;
            state.playback = Playback::Playing;
            if from_start {
                self.shared
                    .main(|av| unsafe { av.player.seekToTime(zero()) });
            }
            (state.speed, state.muted)
        };
        self.shared
            .main(move |av| unsafe { av.player.setRate(speed) });
        if !muted {
            sound::take(&self.sound());
        }
        Ok(())
    }

    pub fn pause(&self) {
        if self.shared.pause() {
            sound::release(&self.sound());
        }
    }

    pub fn seek(&self, to: Duration) {
        let mut state = self.shared.state.lock().unwrap();
        let to = if state.duration.is_zero() {
            to
        } else {
            to.min(state.duration)
        };
        if state.playback == Playback::Ended {
            state.playback = Playback::Paused;
        }
        drop(state);
        self.shared.main(move |av| unsafe {
            av.player.seekToTime_toleranceBefore_toleranceAfter(
                CMTime::with_seconds(to.as_secs_f64(), 600),
                zero(),
                zero(),
            );
        });
    }

    pub fn set_muted(&self, muted: bool) {
        let playing = {
            let mut state = self.shared.state.lock().unwrap();
            state.muted = muted;
            state.playback == Playback::Playing
        };
        self.shared.main(move |av| unsafe {
            av.player.setMuted(muted);
            av.player.setPreventsDisplaySleepDuringVideoPlayback(!muted);
        });
        if playing {
            if muted {
                sound::release(&self.sound());
            } else {
                sound::take(&self.sound());
            }
        }
    }

    pub fn muted(&self) -> bool {
        self.shared.state.lock().unwrap().muted
    }

    pub fn set_volume(&self, volume: f32) {
        let volume = volume.clamp(0.0, 1.0);
        self.shared
            .main(move |av| unsafe { av.player.setVolume(volume) });
    }

    pub fn set_speed(&self, speed: f32) {
        let speed = speed.clamp(0.5, 2.0);
        let playing = {
            let mut state = self.shared.state.lock().unwrap();
            state.speed = speed;
            state.playback == Playback::Playing
        };
        if playing {
            self.shared
                .main(move |av| unsafe { av.player.setRate(speed) });
        }
    }

    pub fn set_looped(&self, looped: bool) {
        self.shared.state.lock().unwrap().looped = looped;
        self.shared.main(move |av| unsafe {
            av.player.setActionAtItemEnd(if looped {
                AVPlayerActionAtItemEnd::None
            } else {
                AVPlayerActionAtItemEnd::Pause
            });
        });
    }

    pub fn position(&self) -> Duration {
        let state = self.shared.state.lock().unwrap();
        if state.playback == Playback::Ended {
            return state.duration;
        }
        // SAFETY: reading the time is safe from any thread.
        let seconds = unsafe { self.av.player.currentTime().seconds() };
        let position = if seconds.is_finite() && seconds > 0.0 {
            Duration::from_secs_f64(seconds)
        } else {
            Duration::ZERO
        };
        if state.duration.is_zero() {
            position
        } else {
            position.min(state.duration)
        }
    }

    pub fn duration(&self) -> Duration {
        self.shared.state.lock().unwrap().duration
    }

    pub fn size(&self) -> Option<(u32, u32)> {
        self.shared.state.lock().unwrap().size
    }

    pub fn state(&self) -> Playback {
        self.shared.state.lock().unwrap().playback
    }

    pub fn error(&self) -> Option<String> {
        self.shared.state.lock().unwrap().error.clone()
    }

    pub fn frame(&self) -> Option<Frame> {
        // SAFETY: the output is made for pulling frames from any thread; a copied buffer is ours
        // (+1), handed to `core_video`'s type under the create rule.
        unsafe {
            let output = &self.av.output;
            let time = output.itemTimeForHostTime(objc2_quartz_core::CACurrentMediaTime());
            if !output.hasNewPixelBufferForItemTime(time) {
                return None;
            }
            let buffer =
                output.copyPixelBufferForItemTime_itemTimeForDisplay(time, std::ptr::null_mut())?;
            let raw = Retained::into_raw(buffer);
            Some(Frame::Surface(
                core_video::pixel_buffer::CVPixelBuffer::wrap_under_create_rule(raw.cast()),
            ))
        }
    }

    pub fn frame_interval(&self) -> Duration {
        self.shared.state.lock().unwrap().frame_interval
    }

    pub fn on_change(&self, callback: impl Fn() + Send + Sync + 'static) {
        *self.shared.on_change.lock().unwrap() = Some(Arc::new(callback));
    }

    fn sound(&self) -> Arc<dyn Sound> {
        self.shared.clone()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.pause();
        self.shared.av.lock().unwrap().take();
        // The decoder goes with the item: the player lets it go on the main thread.
        let av = self.av.clone();
        run_on_main(move || unsafe {
            let center = NSNotificationCenter::defaultCenter();
            for observer in &av.observers {
                let observer: &AnyObject = observer.as_ref();
                center.removeObserver(observer);
            }
            av.player.pause();
            av.item.removeOutput(&av.output);
            av.player.replaceCurrentItemWithPlayerItem(None);
        });
    }
}

impl Shared {
    /// Runs `work` with AVFoundation's objects on the main thread: now if this is it, else soon.
    fn main(&self, work: impl FnOnce(&Av) + Send + 'static) {
        let Some(av) = self.av.lock().unwrap().clone() else {
            return;
        };
        run_on_main(move || work(&av));
    }

    /// Pauses a playing one; `false` if it didn't play.
    fn pause(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.playback != Playback::Playing {
            return false;
        }
        state.playback = Playback::Paused;
        drop(state);
        self.main(|av| unsafe { av.player.pause() });
        true
    }

    fn changed(&self) {
        let callback = self.on_change.lock().unwrap().clone();
        if let Some(callback) = callback {
            callback();
        }
    }

    /// The item played to its end (on the main thread): from the start again if looped.
    fn ended(self: &Arc<Self>) {
        let mut state = self.state.lock().unwrap();
        if state.playback != Playback::Playing {
            return;
        }
        if state.looped {
            drop(state);
            self.main(|av| unsafe { av.player.seekToTime(zero()) });
            return;
        }
        state.playback = Playback::Ended;
        let muted = state.muted;
        drop(state);
        if !muted {
            let sound: Arc<dyn Sound> = self.clone();
            sound::release(&sound);
        }
        self.changed();
    }

    fn failed(self: &Arc<Self>, error: String) {
        let mut state = self.state.lock().unwrap();
        state.error = Some(error);
        let was_playing = state.playback == Playback::Playing;
        state.playback = Playback::Paused;
        drop(state);
        if was_playing {
            let sound: Arc<dyn Sound> = self.clone();
            sound::release(&sound);
        }
        self.changed();
    }

    /// The tracks are read (on AVFoundation's thread): the picture's size as shown, the length,
    /// the frame rate — or no video in the file.
    fn loaded(self: &Arc<Self>, asset: &AVURLAsset) {
        // SAFETY: the asset's values are loaded; reading them doesn't block.
        let found = unsafe {
            let duration = asset.duration().seconds();
            #[allow(deprecated)]
            let tracks = AVMediaTypeVideo.map(|video| asset.tracksWithMediaType(video));
            tracks.and_then(|tracks| tracks.firstObject()).map(|track| {
                let natural = track.naturalSize();
                let transform = track.preferredTransform();
                // The rotation of a phone's video: the size the transform turns it to.
                let width = (natural.width * transform.a + natural.height * transform.c).abs();
                let height = (natural.width * transform.b + natural.height * transform.d).abs();
                let rate = track.nominalFrameRate();
                (width, height, rate, duration)
            })
        };
        match found {
            Some((width, height, rate, duration)) if width >= 1.0 && height >= 1.0 => {
                let mut state = self.state.lock().unwrap();
                state.size = Some((width.round() as u32, height.round() as u32));
                if duration.is_finite() && duration > 0.0 {
                    state.duration = Duration::from_secs_f64(duration);
                }
                if rate.is_finite() && rate >= 1.0 {
                    state.frame_interval =
                        Duration::from_secs_f64(1.0 / f64::from(rate.min(120.0)));
                }
                drop(state);
                self.changed();
            }
            _ => self.failed("no video in the file".into()),
        }
    }
}

impl Sound for Shared {
    fn pause_for_another(&self) {
        if self.pause() {
            self.changed();
        }
    }
}

/// Runs `work` on the main thread: now if this is it, else through the main queue.
fn run_on_main(work: impl FnOnce() + Send + 'static) {
    if MainThreadMarker::new().is_some() {
        work();
        return;
    }
    let work = Cell::new(Some(work));
    let block = RcBlock::new(move || {
        if let Some(work) = work.take() {
            work();
        }
    });
    // SAFETY: the block owns what it runs.
    unsafe { NSOperationQueue::mainQueue().addOperationWithBlock(&block) };
}

fn zero() -> CMTime {
    // SAFETY: a constant.
    unsafe { CMTime::new(0, 1) }
}

/// A CoreVideo key as the `NSString` it is toll-free bridged to.
fn cf_key(key: &CFString) -> &NSString {
    // SAFETY: `CFString` and `NSString` are toll-free bridged.
    unsafe { &*(key as *const CFString).cast::<NSString>() }
}
