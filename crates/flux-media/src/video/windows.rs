//! Windows: Media Foundation's media engine plays the file — `IMFMediaEngine`, the system's whole
//! player (the one behind its HTML video): the file's source, the decoders (the GPU's through
//! the Direct3D 11 device it is given, where the system has them; HEVC through the system's
//! extension), the sound with the picture kept in time, looping, the speed with the pitch kept.
//! A source reader would give the decoded frames only — the clock, the sound and its speed
//! would be ours to build — so the engine it is.
//!
//! The engine runs in frame-server mode: it draws nothing itself. Each player has a thread of
//! its own that, while the video plays (or a paused one's frame is wanted, after it opened and
//! after a seek), asks the engine for a new frame at the video's rate (`OnVideoStreamTick`), has
//! it converted to BGRA into a texture of the video's size (`TransferVideoFrame`, on the GPU),
//! and reads it back into memory: `Frame::Pixels`, one allocation a frame, which the app moves
//! into the image it draws. So the app's thread never waits for the GPU: `frame` only takes what
//! the thread left.
//!
//! The thread is in COM's multithreaded apartment and makes the engine there; the engine is
//! free-threaded, so the app drives it from its own thread, and the engine's threads tell of the
//! metadata, the end, a failure through `IMFMediaEngineNotify`. Media Foundation is loaded and
//! started once per process (`mfplat.dll` looked up at run time: an N edition of Windows without
//! its media features still runs Flux — a video says `Unsupported` and shows its poster). One
//! Direct3D 11 device serves every player (made again if the GPU was reset). A dropped player
//! stops its thread, which shuts the engine down: the decoder, the sound and the textures go.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HMODULE, RECT, S_OK};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
use windows::Win32::Media::MediaFoundation::{
    CLSID_MFMediaEngineClassFactory, IMFAttributes, IMFByteStream, IMFDXGIDeviceManager,
    IMFMediaEngine, IMFMediaEngineClassFactory, IMFMediaEngineEx, IMFMediaEngineNotify,
    IMFMediaEngineNotify_Impl, MF_ACCESSMODE_READ, MF_FILE_ACCESSMODE, MF_FILE_FLAGS,
    MF_FILE_OPENMODE, MF_FILEFLAGS_NONE, MF_MEDIA_ENGINE_CALLBACK, MF_MEDIA_ENGINE_DXGI_MANAGER,
    MF_MEDIA_ENGINE_ERR, MF_MEDIA_ENGINE_ERR_ABORTED, MF_MEDIA_ENGINE_ERR_DECODE,
    MF_MEDIA_ENGINE_ERR_ENCRYPTED, MF_MEDIA_ENGINE_ERR_NETWORK,
    MF_MEDIA_ENGINE_ERR_SRC_NOT_SUPPORTED, MF_MEDIA_ENGINE_EVENT,
    MF_MEDIA_ENGINE_EVENT_DURATIONCHANGE, MF_MEDIA_ENGINE_EVENT_ENDED, MF_MEDIA_ENGINE_EVENT_ERROR,
    MF_MEDIA_ENGINE_EVENT_FIRSTFRAMEREADY, MF_MEDIA_ENGINE_EVENT_FORMATCHANGE,
    MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA, MF_MEDIA_ENGINE_EVENT_SEEKED,
    MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, MF_MT_FRAME_RATE, MF_OPENMODE_FAIL_IF_NOT_EXIST,
    MF_VERSION, MFARGB, MFSTARTUP_LITE,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::Win32::System::Variant::VT_UI8;
use windows::core::{BSTR, HRESULT, Interface, PCSTR, PCWSTR, s, w};
use windows_core::implement;

use super::{Frame, VideoOptions};
use crate::sound::{self, Sound};
use crate::{MediaError, Playback};

/// How long a paused video's frame is waited for after it opened or after a seek.
const STILL_WAIT: Duration = Duration::from_secs(3);
/// Frames that can't be read in a row before the video fails.
const MAX_MISSES: u32 = 30;

struct State {
    playback: Playback,
    muted: bool,
    looped: bool,
    size: Option<(u32, u32)>,
    duration: Duration,
    frame_interval: Duration,
    error: Option<String>,
    /// A paused video's frame is wanted (it opened, it was moved), until then.
    still: Option<Instant>,
    /// A paused video was moved: its frame is taken as it is, without waiting for the engine to
    /// call it new (it may not, until it plays).
    seeked: bool,
    /// The player was dropped: its thread shuts the engine down and ends.
    closing: bool,
}

/// What the player, its thread, the engine's callbacks and [`crate::sound`] share.
struct Shared {
    engine: Mutex<Option<Engine>>,
    state: Mutex<State>,
    /// Wakes the thread: it plays, a paused frame is wanted, the player is dropped.
    wake: Condvar,
    /// The newest frame the app hasn't taken.
    frame: Mutex<Option<Frame>>,
    on_change: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

/// The media engine, called from any thread.
#[derive(Clone)]
struct Engine(IMFMediaEngine);

// SAFETY: the media engine is free-threaded, as Media Foundation's objects are: the app's thread
// drives it while the player's thread pulls its frames and its own threads call back.
unsafe impl Send for Engine {}
unsafe impl Sync for Engine {}

pub(super) struct Player {
    shared: Arc<Shared>,
}

impl Player {
    pub fn open(path: &Path, options: VideoOptions) -> Result<Self, MediaError> {
        if !path.is_file() {
            return Err(MediaError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{} not found", path.display()),
            )));
        }
        let shared = Arc::new(Shared {
            engine: Mutex::new(None),
            state: Mutex::new(State {
                playback: Playback::Paused,
                muted: options.muted,
                looped: options.looped,
                size: None,
                duration: Duration::ZERO,
                frame_interval: Duration::from_millis(33),
                error: None,
                still: None,
                seeked: false,
                closing: false,
            }),
            wake: Condvar::new(),
            frame: Mutex::new(None),
            on_change: Mutex::new(None),
        });
        // The thread makes the engine in its apartment and says how it went.
        let (ready, made) = mpsc::sync_channel(1);
        let thread_shared = shared.clone();
        let path = path.to_path_buf();
        std::thread::Builder::new()
            .name("flux-video".into())
            .spawn(move || run(thread_shared, path, options, ready))
            .map_err(|error| MediaError::Unsupported(format!("a video's thread: {error}")))?;
        match made.recv() {
            Ok(Ok(())) => Ok(Player { shared }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(MediaError::Unsupported("a video's thread ended".into())),
        }
    }

    pub fn play(&self) -> Result<(), MediaError> {
        let (from_start, muted) = {
            let mut state = self.shared.state.lock().unwrap();
            if let Some(error) = &state.error {
                return Err(MediaError::Format(error.clone()));
            }
            if state.playback == Playback::Playing {
                return Ok(());
            }
            let from_start = state.playback == Playback::Ended;
            state.playback = Playback::Playing;
            state.still = None;
            (from_start, state.muted)
        };
        if let Some(engine) = self.shared.engine() {
            // SAFETY: plain calls on a live engine.
            let played = unsafe {
                if from_start {
                    let _ = engine.0.SetCurrentTime(0.0);
                }
                engine.0.Play()
            };
            if let Err(error) = played {
                self.shared.state.lock().unwrap().playback = Playback::Paused;
                return Err(MediaError::Unsupported(format!(
                    "the video can't play: {error}"
                )));
            }
        }
        self.shared.wake.notify_all();
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
        let to = {
            let mut state = self.shared.state.lock().unwrap();
            if state.playback == Playback::Ended {
                state.playback = Playback::Paused;
            }
            if state.playback == Playback::Paused {
                state.still = Some(Instant::now() + STILL_WAIT);
            }
            if state.duration.is_zero() {
                to
            } else {
                to.min(state.duration)
            }
        };
        if let Some(engine) = self.shared.engine() {
            // SAFETY: a plain call on a live engine.
            let _ = unsafe { engine.0.SetCurrentTime(to.as_secs_f64()) };
        }
        self.shared.wake.notify_all();
    }

    pub fn set_muted(&self, muted: bool) {
        let playing = {
            let mut state = self.shared.state.lock().unwrap();
            state.muted = muted;
            state.playback == Playback::Playing
        };
        if let Some(engine) = self.shared.engine() {
            // SAFETY: a plain call on a live engine.
            let _ = unsafe { engine.0.SetMuted(muted) };
        }
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
        if let Some(engine) = self.shared.engine() {
            // SAFETY: a plain call on a live engine.
            let _ = unsafe { engine.0.SetVolume(f64::from(volume.clamp(0.0, 1.0))) };
        }
    }

    pub fn set_speed(&self, speed: f32) {
        let speed = f64::from(speed.clamp(0.5, 2.0));
        if let Some(engine) = self.shared.engine() {
            // SAFETY: plain calls on a live engine. The default rate is the one a new load
            // starts at; the rate is kept while paused.
            unsafe {
                let _ = engine.0.SetDefaultPlaybackRate(speed);
                let _ = engine.0.SetPlaybackRate(speed);
            }
        }
    }

    pub fn set_looped(&self, looped: bool) {
        self.shared.state.lock().unwrap().looped = looped;
        if let Some(engine) = self.shared.engine() {
            // SAFETY: a plain call on a live engine.
            let _ = unsafe { engine.0.SetLoop(looped) };
        }
    }

    pub fn position(&self) -> Duration {
        let (ended, duration) = {
            let state = self.shared.state.lock().unwrap();
            (state.playback == Playback::Ended, state.duration)
        };
        if ended {
            return duration;
        }
        let seconds = self
            .shared
            .engine()
            // SAFETY: a plain call on a live engine.
            .map_or(0.0, |engine| unsafe { engine.0.GetCurrentTime() });
        let position = if seconds.is_finite() && seconds > 0.0 {
            Duration::from_secs_f64(seconds)
        } else {
            Duration::ZERO
        };
        if duration.is_zero() {
            position
        } else {
            position.min(duration)
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
        self.shared.frame.lock().unwrap().take()
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
        // The thread shuts the engine down on its way out — not waited for here: the app's
        // thread drops the players of videos scrolled away.
        self.shared.state.lock().unwrap().closing = true;
        self.shared.wake.notify_all();
    }
}

impl Shared {
    fn engine(&self) -> Option<Engine> {
        self.engine.lock().unwrap().clone()
    }

    /// Pauses a playing one; `false` if it didn't play.
    fn pause(&self) -> bool {
        {
            let mut state = self.state.lock().unwrap();
            if state.playback != Playback::Playing {
                return false;
            }
            state.playback = Playback::Paused;
        }
        if let Some(engine) = self.engine() {
            // SAFETY: a plain call on a live engine.
            let _ = unsafe { engine.0.Pause() };
        }
        true
    }

    fn changed(&self) {
        let callback = self.on_change.lock().unwrap().clone();
        if let Some(callback) = callback {
            callback();
        }
    }

    /// The engine's news, on its threads.
    fn event(self: &Arc<Self>, event: MF_MEDIA_ENGINE_EVENT, code: usize, hresult: u32) {
        match event {
            MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA => self.loaded(true),
            MF_MEDIA_ENGINE_EVENT_FORMATCHANGE | MF_MEDIA_ENGINE_EVENT_DURATIONCHANGE => {
                self.loaded(false)
            }
            // A paused video's frame there is to show: the first one, the one a seek went to.
            MF_MEDIA_ENGINE_EVENT_FIRSTFRAMEREADY | MF_MEDIA_ENGINE_EVENT_SEEKED => {
                let mut state = self.state.lock().unwrap();
                if state.playback != Playback::Playing {
                    state.still = Some(Instant::now() + STILL_WAIT);
                    state.seeked |= event == MF_MEDIA_ENGINE_EVENT_SEEKED;
                    drop(state);
                    self.wake.notify_all();
                }
            }
            MF_MEDIA_ENGINE_EVENT_ENDED => self.ended(),
            MF_MEDIA_ENGINE_EVENT_ERROR => self.failed(describe(code, hresult)),
            _ => {}
        }
    }

    /// The file's header is read (`metadata`), or the picture's size or the length changed:
    /// the size, the length, the frame rate — or no video in the file.
    fn loaded(self: &Arc<Self>, metadata: bool) {
        let Some(engine) = self.engine() else {
            return;
        };
        let (mut width, mut height) = (0u32, 0u32);
        // SAFETY: plain calls on a live engine.
        let (sized, video, duration) = unsafe {
            let sized = engine
                .0
                .GetNativeVideoSize(Some(&mut width), Some(&mut height))
                .is_ok();
            (sized, engine.0.HasVideo().as_bool(), engine.0.GetDuration())
        };
        if metadata && !video {
            self.failed("no video in the file".into());
            return;
        }
        let rate = metadata.then(|| frame_rate(&engine.0)).flatten();
        {
            let mut state = self.state.lock().unwrap();
            if sized && width > 0 && height > 0 {
                state.size = Some((width, height));
            }
            if duration.is_finite() && duration > 0.0 {
                state.duration = Duration::from_secs_f64(duration);
            }
            if let Some(rate) = rate {
                state.frame_interval = Duration::from_secs_f64(1.0 / rate.min(120.0));
            }
            // A paused one shows its first frame.
            if metadata && state.playback != Playback::Playing {
                state.still = Some(Instant::now() + STILL_WAIT);
            }
        }
        self.wake.notify_all();
        self.changed();
    }

    /// Played to the end — a looped one doesn't end: the engine starts it again.
    fn ended(self: &Arc<Self>) {
        let muted = {
            let mut state = self.state.lock().unwrap();
            if state.playback != Playback::Playing || state.looped {
                return;
            }
            state.playback = Playback::Ended;
            state.muted
        };
        if !muted {
            let sound: Arc<dyn Sound> = self.clone();
            sound::release(&sound);
        }
        self.changed();
    }

    fn failed(self: &Arc<Self>, error: String) {
        let was_playing = {
            let mut state = self.state.lock().unwrap();
            state.error = Some(error);
            state.still = None;
            let was_playing = state.playback == Playback::Playing;
            state.playback = Playback::Paused;
            was_playing
        };
        if let Some(engine) = self.engine() {
            // SAFETY: a plain call on a live engine.
            let _ = unsafe { engine.0.Pause() };
        }
        if was_playing {
            let sound: Arc<dyn Sound> = self.clone();
            sound::release(&sound);
        }
        self.changed();
    }
}

impl Sound for Shared {
    fn pause_for_another(&self) {
        if self.pause() {
            self.changed();
        }
    }
}

/// The engine's callback: holds the player's shared part weakly (the engine holds the callback).
#[implement(IMFMediaEngineNotify)]
struct Notify {
    shared: Weak<Shared>,
}

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, param1: usize, param2: u32) -> windows::core::Result<()> {
        if let Some(shared) = self.shared.upgrade() {
            shared.event(MF_MEDIA_ENGINE_EVENT(event as i32), param1, param2);
        }
        Ok(())
    }
}

/// A player's thread: makes the engine, then pulls frames while there are frames to pull, until
/// the player is dropped.
fn run(
    shared: Arc<Shared>,
    path: PathBuf,
    options: VideoOptions,
    ready: SyncSender<Result<(), MediaError>>,
) {
    // SAFETY: this thread's own COM, undone below.
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    match open_engine(&shared, &path, options) {
        Ok(gpu) => {
            let _ = ready.send(Ok(()));
            drop(ready);
            pull_frames(&shared, &gpu);
        }
        Err(error) => {
            let _ = ready.send(Err(error));
        }
    }
    let engine = shared.engine.lock().unwrap().take();
    if let Some(engine) = engine {
        // SAFETY: the engine stops calling back and lets the decoder and the sound go.
        let _ = unsafe { engine.0.Shutdown() };
    }
    if com {
        // SAFETY: balances this thread's `CoInitializeEx`; its COM objects are released.
        unsafe { CoUninitialize() };
    }
}

/// Makes the engine (in `shared`) and gives it the file; the GPU it draws frames on.
fn open_engine(
    shared: &Arc<Shared>,
    path: &Path,
    options: VideoOptions,
) -> Result<Arc<Gpu>, MediaError> {
    let mf = media_foundation()?;
    let gpu = gpu()?;
    let unsupported = |what: &str, error: windows::core::Error| {
        MediaError::Unsupported(format!("Media Foundation: {what}: {error}"))
    };
    let notify: IMFMediaEngineNotify = Notify {
        shared: Arc::downgrade(shared),
    }
    .into();
    // SAFETY: COM calls with valid objects; the attributes hold what they're given.
    let engine = unsafe {
        let factory: IMFMediaEngineClassFactory =
            CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)
                .map_err(|error| unsupported("the media engine", error))?;
        let attributes = mf
            .attributes(3)
            .map_err(|error| unsupported("attributes", error))?;
        attributes
            .SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)
            .and_then(|()| attributes.SetUnknown(&MF_MEDIA_ENGINE_DXGI_MANAGER, &gpu.manager))
            .and_then(|()| {
                attributes.SetUINT32(
                    &MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT,
                    DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
                )
            })
            .map_err(|error| unsupported("attributes", error))?;
        let engine = factory
            .CreateInstance(0, &attributes)
            .map_err(|error| unsupported("the media engine", error))?;
        let _ = engine.SetMuted(options.muted);
        let _ = engine.SetLoop(options.looped);
        engine
    };
    // The callbacks find the engine from now on: the file is given after.
    *shared.engine.lock().unwrap() = Some(Engine(engine.clone()));
    // A byte stream of the file, not a URL: a path isn't parsed as one ('#', '%').
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: COM calls with valid objects; `wide` is a null-terminated path.
    unsafe {
        let stream = mf
            .file(&wide)
            .map_err(|error| MediaError::Io(std::io::Error::other(error.message())))?;
        let engine: IMFMediaEngineEx = engine
            .cast()
            .map_err(|error| unsupported("the media engine", error))?;
        let url = BSTR::from_wide(&wide[..wide.len() - 1]);
        engine
            .SetSourceFromByteStream(&stream, &url)
            .map_err(|error| MediaError::Format(format!("{error}")))?;
    }
    Ok(gpu)
}

/// Pulls the engine's new frames while it plays (or a paused one's frame is wanted), at twice
/// the video's rate; sleeps otherwise. Ends when the player is dropped.
fn pull_frames(shared: &Arc<Shared>, gpu: &Gpu) {
    let mut textures: Option<Textures> = None;
    // Frames that couldn't be read in a row: a passing failure is skipped, a lasting one fails.
    let mut misses = 0;
    loop {
        let (interval, size, seeked) = {
            let mut state = shared.state.lock().unwrap();
            loop {
                if state.closing {
                    return;
                }
                if state.still.is_some_and(|until| Instant::now() >= until) {
                    state.still = None;
                }
                if state.playback == Playback::Playing || state.still.is_some() {
                    break;
                }
                state = shared.wake.wait(state).unwrap();
            }
            let seeked = state.seeked && state.playback != Playback::Playing;
            state.seeked = false;
            (
                (state.frame_interval / 2).max(Duration::from_millis(4)),
                state.size,
                seeked,
            )
        };
        if let (Some(engine), Some(size)) = (shared.engine(), size) {
            let mut time = 0i64;
            // `OnVideoStreamTick` says "a new frame" with S_OK and "none" with S_FALSE, which
            // the binding's `Result` can't tell apart: called through the vtable.
            // SAFETY: a live engine, a valid out pointer.
            let tick = unsafe {
                (Interface::vtable(&engine.0).OnVideoStreamTick)(
                    Interface::as_raw(&engine.0),
                    &mut time,
                )
            };
            if tick == S_OK || seeked {
                match read_frame(gpu, &engine.0, &mut textures, size) {
                    Ok(frame) => {
                        misses = 0;
                        *shared.frame.lock().unwrap() = Some(frame);
                        let still = shared.state.lock().unwrap().still.take().is_some();
                        if still {
                            // The app asks a paused video for its frame when told.
                            shared.changed();
                        }
                    }
                    Err(error) => {
                        misses += 1;
                        if misses == MAX_MISSES {
                            shared.failed(format!("its frames can't be read: {error}"));
                        }
                    }
                }
            }
        }
        let state = shared.state.lock().unwrap();
        if !state.closing {
            let _ = shared.wake.wait_timeout(state, interval).unwrap();
        }
    }
}

/// The textures a frame goes through: the engine draws it into `target` (BGRA, on the GPU), the
/// GPU copies it into `staging`, which is read.
struct Textures {
    size: (u32, u32),
    target: ID3D11Texture2D,
    staging: ID3D11Texture2D,
}

impl Textures {
    fn new(gpu: &Gpu, (width, height): (u32, u32)) -> windows::core::Result<Self> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let staging = D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..desc
        };
        let (mut target, mut copy) = (None, None);
        // SAFETY: valid descriptions and out pointers.
        unsafe {
            gpu.device.CreateTexture2D(&desc, None, Some(&mut target))?;
            gpu.device
                .CreateTexture2D(&staging, None, Some(&mut copy))?;
        }
        let missing = || windows::core::Error::from(windows::Win32::Foundation::E_POINTER);
        Ok(Textures {
            size: (width, height),
            target: target.ok_or_else(missing)?,
            staging: copy.ok_or_else(missing)?,
        })
    }
}

/// The engine's current frame as BGRA pixels, rows top to bottom.
fn read_frame(
    gpu: &Gpu,
    engine: &IMFMediaEngine,
    textures: &mut Option<Textures>,
    size: (u32, u32),
) -> windows::core::Result<Frame> {
    if textures
        .as_ref()
        .is_none_or(|textures| textures.size != size)
    {
        *textures = None;
        *textures = Some(Textures::new(gpu, size)?);
    }
    let Some(textures) = textures.as_ref() else {
        unreachable!("made above");
    };
    let (width, height) = size;
    let rect = RECT {
        left: 0,
        top: 0,
        right: width as i32,
        bottom: height as i32,
    };
    let black = MFARGB {
        rgbBlue: 0,
        rgbGreen: 0,
        rgbRed: 0,
        rgbAlpha: u8::MAX,
    };
    let row = width as usize * 4;
    let mut bgra = Vec::with_capacity(row * height as usize);
    // SAFETY: live objects; the mapped memory is `RowPitch` × `height` bytes, read before
    // `Unmap`.
    unsafe {
        engine.TransferVideoFrame(&textures.target, None, &rect, Some(&black))?;
        gpu.context
            .CopyResource(&textures.staging, &textures.target);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        gpu.context
            .Map(&textures.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let pitch = mapped.RowPitch as usize;
        let data = mapped.pData.cast::<u8>().cast_const();
        for y in 0..height as usize {
            bgra.extend_from_slice(std::slice::from_raw_parts(data.add(y * pitch), row));
        }
        gpu.context.Unmap(&textures.staging, 0);
    }
    // A video's alpha byte isn't defined: opaque.
    for pixel in bgra.as_chunks_mut::<4>().0 {
        pixel[3] = u8::MAX;
    }
    Ok(Frame::Pixels {
        width,
        height,
        bgra,
    })
}

/// The video's frames a second, from its stream's media type.
fn frame_rate(engine: &IMFMediaEngine) -> Option<f64> {
    let engine: IMFMediaEngineEx = engine.cast().ok()?;
    // SAFETY: plain calls on a live engine; a `VT_UI8` value owns nothing.
    unsafe {
        let streams = engine.GetNumberOfStreams().ok()?;
        (0..streams).find_map(|stream| {
            let value = engine.GetStreamAttribute(stream, &MF_MT_FRAME_RATE).ok()?;
            let value = &value.Anonymous.Anonymous;
            if value.vt != VT_UI8 {
                return None;
            }
            // The numerator in the high 32 bits, the denominator in the low.
            let packed = value.Anonymous.uhVal;
            let (numerator, denominator) = ((packed >> 32) as f64, (packed as u32) as f64);
            (numerator >= 1.0 && denominator >= 1.0).then(|| numerator / denominator)
        })
    }
}

/// What a failure the engine reported means.
fn describe(code: usize, hresult: u32) -> String {
    let what = match MF_MEDIA_ENGINE_ERR(code as i32) {
        MF_MEDIA_ENGINE_ERR_SRC_NOT_SUPPORTED => "the system can't play this file",
        MF_MEDIA_ENGINE_ERR_DECODE => "the video can't be decoded",
        MF_MEDIA_ENGINE_ERR_NETWORK => "the file can't be read",
        MF_MEDIA_ENGINE_ERR_ENCRYPTED => "the video is encrypted",
        MF_MEDIA_ENGINE_ERR_ABORTED => "the video's loading stopped",
        _ => "the video can't be played",
    };
    // MF_E_TOPO_CODEC_NOT_FOUND, MF_E_INVALIDMEDIATYPE: no decoder for the video's format.
    if matches!(hresult, 0xC00D_5212 | 0xC00D_36B4) {
        return format!(
            "{what}: no decoder for its format here (HEVC needs the system's HEVC Video \
             Extensions)"
        );
    }
    format!("{what} ({})", HRESULT(hresult as i32).message())
}

/// A function as `GetProcAddress` finds it, and the ones it is turned into (`mfapi.h`).
type Proc = unsafe extern "system" fn() -> isize;
type Startup = unsafe extern "system" fn(u32, u32) -> HRESULT;
type CreateAttributes = unsafe extern "system" fn(*mut *mut c_void, u32) -> HRESULT;
type CreateDxgiManager = unsafe extern "system" fn(*mut u32, *mut *mut c_void) -> HRESULT;
type CreateFile = unsafe extern "system" fn(
    MF_FILE_ACCESSMODE,
    MF_FILE_OPENMODE,
    MF_FILE_FLAGS,
    PCWSTR,
    *mut *mut c_void,
) -> HRESULT;

/// Media Foundation's few functions Flux calls, looked up in `mfplat.dll` at run time.
struct MediaFoundation {
    create_attributes: CreateAttributes,
    create_dxgi_manager: CreateDxgiManager,
    create_file: CreateFile,
}

impl MediaFoundation {
    fn attributes(&self, size: u32) -> windows::core::Result<IMFAttributes> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: the out pointer gets a new reference, owned by the result.
        unsafe {
            (self.create_attributes)(&mut raw, size).ok()?;
            Ok(IMFAttributes::from_raw(raw))
        }
    }

    fn dxgi_manager(&self) -> windows::core::Result<(IMFDXGIDeviceManager, u32)> {
        let (mut raw, mut token) = (std::ptr::null_mut(), 0u32);
        // SAFETY: as above.
        unsafe {
            (self.create_dxgi_manager)(&mut token, &mut raw).ok()?;
            Ok((IMFDXGIDeviceManager::from_raw(raw), token))
        }
    }

    /// A byte stream reading the file at `path` (UTF-16, null-terminated).
    fn file(&self, path: &[u16]) -> windows::core::Result<IMFByteStream> {
        assert_eq!(path.last(), Some(&0), "a null-terminated path");
        let mut raw = std::ptr::null_mut();
        // SAFETY: as above; `path` is null-terminated.
        unsafe {
            (self.create_file)(
                MF_ACCESSMODE_READ,
                MF_OPENMODE_FAIL_IF_NOT_EXIST,
                MF_FILEFLAGS_NONE,
                PCWSTR(path.as_ptr()),
                &mut raw,
            )
            .ok()?;
            Ok(IMFByteStream::from_raw(raw))
        }
    }
}

/// Media Foundation, loaded and started once per process (never stopped: the process ends).
fn media_foundation() -> Result<&'static MediaFoundation, MediaError> {
    static LOADED: OnceLock<Result<MediaFoundation, String>> = OnceLock::new();
    LOADED
        .get_or_init(|| {
            // SAFETY: the system's own DLL; the functions' types are its documented ones.
            unsafe {
                let module: HMODULE =
                    LoadLibraryExW(w!("mfplat.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
                        .map_err(|_| "Media Foundation isn't on this system".to_string())?;
                let find = |name: PCSTR| {
                    GetProcAddress(module, name)
                        .ok_or_else(|| "Media Foundation is incomplete here".to_string())
                };
                let startup = std::mem::transmute::<Proc, Startup>(find(s!("MFStartup"))?);
                let loaded = MediaFoundation {
                    create_attributes: std::mem::transmute::<Proc, CreateAttributes>(find(s!(
                        "MFCreateAttributes"
                    ))?),
                    create_dxgi_manager: std::mem::transmute::<Proc, CreateDxgiManager>(find(s!(
                        "MFCreateDXGIDeviceManager"
                    ))?),
                    create_file: std::mem::transmute::<Proc, CreateFile>(find(s!("MFCreateFile"))?),
                };
                startup(MF_VERSION, MFSTARTUP_LITE)
                    .ok()
                    .map_err(|error| format!("Media Foundation doesn't start: {error}"))?;
                Ok(loaded)
            }
        })
        .as_ref()
        .map_err(|error| MediaError::Unsupported(error.clone()))
}

/// The Direct3D 11 device the engines decode and convert frames on, with Media Foundation's
/// manager of it.
struct Gpu {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    manager: IMFDXGIDeviceManager,
}

// SAFETY: a Direct3D 11 device is free-threaded; its immediate context is used by several threads
// (the engines', the players') with the device's multithread protection on.
unsafe impl Send for Gpu {}
unsafe impl Sync for Gpu {}

/// The device every player shares: made once, and again if the GPU was reset or removed.
fn gpu() -> Result<Arc<Gpu>, MediaError> {
    static GPU: Mutex<Option<Arc<Gpu>>> = Mutex::new(None);
    let mut current = GPU.lock().unwrap();
    if let Some(gpu) = current.as_ref()
        // SAFETY: a plain call on a live device.
        && unsafe { gpu.device.GetDeviceRemovedReason() }.is_ok()
    {
        return Ok(gpu.clone());
    }
    let gpu = Arc::new(Gpu::new()?);
    *current = Some(gpu.clone());
    Ok(gpu)
}

impl Gpu {
    fn new() -> Result<Gpu, MediaError> {
        let mf = media_foundation()?;
        let flags = D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT;
        let mut failure = None;
        // The GPU's decoders; else the system's software device (a machine without a GPU's
        // driver, a server).
        for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
            let (mut device, mut context) = (None, None);
            // SAFETY: valid out pointers.
            let made = unsafe {
                D3D11CreateDevice(
                    None::<&IDXGIAdapter>,
                    driver,
                    HMODULE::default(),
                    flags,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            };
            match (made, device, context) {
                (Ok(()), Some(device), Some(context)) => {
                    // SAFETY: COM calls with valid objects.
                    let manager = unsafe {
                        // The engine decodes on its threads while the players read frames.
                        let multithread: ID3D11Multithread = device.cast().map_err(|error| {
                            MediaError::Unsupported(format!("Direct3D 11: {error}"))
                        })?;
                        let _ = multithread.SetMultithreadProtected(true);
                        let (manager, token) = mf.dxgi_manager().map_err(|error| {
                            MediaError::Unsupported(format!("Media Foundation: {error}"))
                        })?;
                        manager.ResetDevice(&device, token).map_err(|error| {
                            MediaError::Unsupported(format!("Media Foundation: {error}"))
                        })?;
                        manager
                    };
                    return Ok(Gpu {
                        device,
                        context,
                        manager,
                    });
                }
                (made, _, _) => failure = made.err(),
            }
        }
        Err(MediaError::Unsupported(format!(
            "no Direct3D 11 device for video: {}",
            failure.map_or_else(|| "none made".to_string(), |error| error.message())
        )))
    }
}
