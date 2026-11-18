//! The media of a conversation (API 0.5, FluxGram part 4): voice messages, videos (a GIF, a round
//! message, a video), animated stickers, played by `flux-media`.
//!
//! - A voice message: ▶ / ⏸ in an accent circle (a ring of the download around it, × while it
//!   downloads), the waveform from the message's data (the played part in the accent; a click or
//!   a drag seeks, playing or not), where it is and its length, the speed (1× → 1.5× → 2×, kept
//!   for the whole app); an accent dot while `unheard`. ▶ on one not downloaded asks the plugin
//!   (`part-clicked`) and plays it when the message is put again with its file. While one plays:
//!   a bar over the feed (who, ⏸, the speed, ×; a click reveals the message) and a line in the
//!   status bar ([`crate::now_playing`]); at its end the next voice message below plays. Its first
//!   play since it was put goes to the plugin as `media-played`.
//! - A GIF and a round message with `autoplay`: play by themselves, looped, without sound, while
//!   on screen in the active window — [`MAX_VIDEOS`] at a time, the others show their poster. A
//!   round one clicked plays with sound from the start, larger, with a ring of progress; a click
//!   again pauses it; at its end, or when another sound takes over, it is back to the small loop.
//!   A video: its poster with ▶ and the length; a click opens the media viewer
//!   ([`crate::image_popup`]) among the feed's pictures and videos.
//! - An animated sticker (`image-spec.animation`, an FXA file): played from its frames, the next
//!   one read and decompressed off the window's thread; only on screen in the active window,
//!   [`MAX_ANIMATIONS`] at a time, the others on their first frame (the picture); once or looped
//!   as its spec says. The panel's items are drawn the same way by their keys
//!   ([`Media::render_animation`], [`Media::render_gif`]).
//!
//! The players live in [`Media`], by key ("<message>/<part>", a panel item's key): each media
//! element records, as it is laid out, whether it is on screen (within the area it is clipped
//! to); the next frame starts the players of those in sight and drops the others (a video lets
//! its decoder go). Frames are asked for by one timer at the rate of what plays — a video's,
//! an animation's, a few times a second for a voice message — only while something plays: an
//! idle feed draws nothing.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flux_media::fxa::Animation;
use flux_media::video::{Frame, VideoOptions, VideoPlayer};
use flux_media::voice::{SPEEDS, VoicePlayer};
use flux_media::{MediaError, Playback};
use flux_plugin::api::events::UiEvent;
use flux_plugin::api::ui::{ImageShape, ImageSpec, MessagePart, VideoKind, VideoSpec, VoiceSpec};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, ClickEvent, Context, Div, Hsla, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, PathBuilder, Pixels, RenderImage,
    SharedString, Task, WeakEntity, Window, canvas, div, fill, img, point, prelude::*, px,
    relative, size,
};

use super::{Conversation, render_picture, window_active};
use crate::i18n::tr;
use crate::icons::{IconName, icon};
use crate::image_popup::{ImagePopup, MediaItem, VideoItem};
use crate::image_view::ImageSource;
use crate::theme::{self, Theme, UiColors};

/// A round video message's side in the feed…
pub(super) const ROUND_SIDE: f32 = 240.;
/// …and while it plays with sound.
const ROUND_LOUD_SIDE: f32 = 320.;
/// Videos without sound playing at once in a conversation (GIFs, round messages): the others
/// show their poster.
const MAX_VIDEOS: usize = 4;
/// Animations playing at once in a conversation: the others stay on their first frame.
const MAX_ANIMATIONS: usize = 6;
/// A voice message's ▶ / ⏸.
const VOICE_BUTTON: f32 = 36.;
const WAVE_HEIGHT: f32 = 22.;
const WAVE_BAR: f32 = 2.;
const WAVE_GAP: f32 = 2.;
/// A waveform without data: an even line this loud (of 31).
const WAVE_FLAT: u8 = 4;
/// How often a voice message's place is drawn again while it plays: in sight, and out of it
/// (only the status bar's line, by the second).
const VOICE_TICK: Duration = Duration::from_millis(250);
const VOICE_TICK_UNSEEN: Duration = Duration::from_secs(1);
/// From this many animations playing at once, each shows every other frame.
const CROWD: usize = 3;
/// The shortest step of the frames' timer: a broken file's rate can't spin it.
const MIN_TICK: Duration = Duration::from_millis(16);
/// The playing bar over the feed.
const BAR_HEIGHT: f32 = 34.;

/// What a media element asked for as it was drawn: what plays while it is on screen.
#[derive(Clone, Debug, PartialEq)]
enum Want {
    /// A video: without sound and looped, or with sound (a round message clicked).
    Video { path: PathBuf, loud: bool },
    /// An animation of frames.
    Animation { path: PathBuf, looped: bool },
}

/// What the media elements of a frame recorded: shared with their prepaint.
#[derive(Default)]
struct Sightings {
    /// The frame being drawn.
    frame: Cell<u64>,
    /// The order the elements were drawn in: the first ones play when not all may.
    next: Cell<u64>,
    /// The elements drawn and what they asked for, by key (this frame's).
    wants: RefCell<HashMap<String, (u64, Want)>>,
    /// Each key's last record: the frame, on screen.
    seen: RefCell<HashMap<String, (u64, bool)>>,
}

impl Sightings {
    fn want(&self, key: &str, want: Want) {
        let order = self.next.get();
        self.next.set(order + 1);
        self.wants
            .borrow_mut()
            .insert(key.to_string(), (order, want));
    }

    /// Records where a key is in this frame; true — it came on screen or left it.
    fn record(&self, key: &str, on_screen: bool) -> bool {
        let frame = self.frame.get();
        let before = self
            .seen
            .borrow_mut()
            .insert(key.to_string(), (frame, on_screen));
        let was = before.is_some_and(|(at, on)| on && at + 1 >= frame);
        was != on_screen
    }

    /// On screen in the frame before this one.
    fn on_screen(&self, key: &str, frame: u64) -> bool {
        self.seen
            .borrow()
            .get(key)
            .is_some_and(|(at, on)| *on && *at == frame)
    }
}

/// The voice message played or paused.
struct Voice {
    message: String,
    part: usize,
    /// The author, for the bar and the status line.
    who: SharedString,
    player: VoicePlayer,
}

/// A frame of a video on screen.
#[derive(Clone)]
enum Shown {
    /// The decoder's buffer (macOS), drawn without a copy.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Surface(Frame),
    /// The frame's pixels (elsewhere).
    Image(Arc<RenderImage>),
}

/// A video playing in the feed or the panel: a GIF, a round message.
struct Video {
    player: VideoPlayer,
    /// The last frame, drawn until the next comes.
    frame: Option<Shown>,
    loud: bool,
}

impl Video {
    fn set_frame(&mut self, frame: Frame, cx: &mut App) {
        let shown = match frame {
            Frame::Pixels {
                width,
                height,
                bgra,
            } => match image::RgbaImage::from_raw(width, height, bgra.to_vec()) {
                Some(buffer) => {
                    Shown::Image(Arc::new(RenderImage::new([image::Frame::new(buffer)])))
                }
                None => return,
            },
            #[cfg(target_os = "macos")]
            surface @ Frame::Surface(_) => Shown::Surface(surface),
        };
        if let Some(Shown::Image(old)) = self.frame.replace(shown) {
            cx.drop_image(old, None);
        }
    }

    /// Its picture leaves the atlases.
    fn release(self, cx: &mut App) {
        if let Some(Shown::Image(image)) = self.frame {
            cx.drop_image(image, None);
        }
    }
}

/// A frame read ahead: its index and pixels (`Err` — it couldn't be read).
type NextFrame = Result<(u32, Vec<u8>), ()>;

/// An animation of frames playing (an FXA file).
struct Playing {
    animation: Arc<Animation>,
    looped: bool,
    /// The frame shown.
    image: Option<Arc<RenderImage>>,
    /// When the next frame is due.
    due: Instant,
    /// The next frame, read off the window's thread.
    next: Arc<Mutex<Option<NextFrame>>>,
    /// Played to its end (not looped): the last frame stays.
    done: bool,
    /// Frames advanced at a time: 2 while many play at once ([`CROWD`]) — half the frames at the
    /// same speed, half the window's redraws.
    stride: u32,
}

impl Playing {
    fn open(path: &Path, looped: bool, cx: &App) -> Result<Self, MediaError> {
        let mut playing = Playing {
            animation: Arc::new(Animation::open(path)?),
            looped,
            image: None,
            due: Instant::now(),
            next: Arc::default(),
            done: false,
            stride: 1,
        };
        playing.read(0, cx);
        Ok(playing)
    }

    /// Reads and decompresses frame `index` on the background executor.
    fn read(&mut self, index: u32, cx: &App) {
        let (animation, next) = (self.animation.clone(), self.next.clone());
        cx.background_executor()
            .spawn(async move {
                let mut pixels = Vec::new();
                let read = animation
                    .read_frame(index, &mut pixels)
                    .map(|()| (index, pixels))
                    .map_err(|error| log::warn!("animation frame {index}: {error}"));
                *next.lock().unwrap() = Some(read);
            })
            .detach();
    }

    /// The next frame takes its place when its time has come and it is read (the one after it
    /// is read then); true — the picture changed.
    fn advance(&mut self, now: Instant, cx: &mut App) -> bool {
        if self.done || now < self.due {
            return false;
        }
        let Some(read) = self.next.lock().unwrap().take() else {
            return false;
        };
        let (width, height) = (self.animation.width(), self.animation.height());
        let Some((index, buffer)) = read.ok().and_then(|(index, pixels)| {
            Some((index, image::RgbaImage::from_raw(width, height, pixels)?))
        }) else {
            self.done = true;
            return false;
        };
        // FXA is BGRA with straight alpha: what gpui's sprites take.
        let image = Arc::new(RenderImage::new([image::Frame::new(buffer)]));
        if let Some(old) = self.image.replace(image) {
            cx.drop_image(old, None);
        }
        let step = self.step();
        // Behind by more than a frame (a busy moment): the clock starts again from now.
        self.due = match now.duration_since(self.due) > step {
            true => now + step,
            false => self.due + step,
        };
        let last = self.animation.frame_count() - 1;
        if index >= last && !self.looped {
            self.done = true;
        } else {
            let next = match self.looped {
                true => (index + self.stride) % (last + 1),
                false => (index + self.stride).min(last),
            };
            self.read(next, cx);
        }
        true
    }

    /// How long a shown frame stays: the file's frame, times the stride.
    fn step(&self) -> Duration {
        self.animation.frame_duration().max(MIN_TICK) * self.stride
    }

    fn release(self, cx: &mut App) {
        if let Some(image) = self.image {
            cx.drop_image(image, None);
        }
    }
}

/// The viewer opened on a video not downloaded yet: the message put again with its file plays
/// it there.
struct WaitingViewer {
    popup: WeakEntity<ImagePopup>,
    message: String,
    part: usize,
    index: usize,
}

/// What plays now, for the status bar's line ([`crate::now_playing`]).
pub(crate) struct NowPlaying {
    pub who: SharedString,
    pub position: Duration,
    pub playing: bool,
}

/// The players of one conversation's media.
pub(super) struct Media {
    plugin: SharedString,
    sightings: Rc<Sightings>,
    /// The conversation was in sight in the active window in the last frame.
    in_sight: bool,
    /// The window it was drawn in: the frames' timer checks it is still the active one.
    window: Option<AnyWindowHandle>,
    voice: Option<Voice>,
    /// ▶ pressed on a part not downloaded yet (a voice, a round message): it plays when its
    /// message is put again with the file.
    wanted: Option<(String, usize)>,
    /// Parts that can't be played: their ▶ says so (until the message is put again).
    failed: HashMap<(String, usize), SharedString>,
    /// Keys whose player failed to open: not tried again each frame.
    broken: HashSet<String>,
    /// Parts played since they were put: `media-played` went out.
    played: HashSet<(String, usize)>,
    /// The voice part whose waveform is being dragged.
    seeking: Option<(String, usize)>,
    /// The round message playing with sound; paused by a click (not by another sound).
    loud: Option<(String, usize)>,
    loud_paused: bool,
    /// GIFs without `autoplay` pressed to play.
    started: HashSet<String>,
    videos: HashMap<String, Video>,
    animations: HashMap<String, Playing>,
    viewer: Option<WaitingViewer>,
    /// The players tell of changes on their threads: the conversation hears them through this.
    changes: Option<mpsc::UnboundedSender<()>>,
    _listen: Option<Task<()>>,
    /// The frames' timer and its step.
    ticker: Option<(Duration, Task<()>)>,
    /// The voice message's second shown last (the status bar's line).
    second: Option<u64>,
}

impl Media {
    pub(super) fn new(plugin: SharedString) -> Self {
        Self {
            plugin,
            sightings: Rc::default(),
            in_sight: true,
            window: None,
            voice: None,
            wanted: None,
            failed: HashMap::new(),
            broken: HashSet::new(),
            played: HashSet::new(),
            seeking: None,
            loud: None,
            loud_paused: false,
            started: HashSet::new(),
            videos: HashMap::new(),
            animations: HashMap::new(),
            viewer: None,
            changes: None,
            _listen: None,
            ticker: None,
            second: None,
        }
    }

    /// A media file the plugin may play: in its folders, as its pictures.
    fn allowed(&self, path: Option<&str>, cx: &mut App) -> Option<PathBuf> {
        let path = PathBuf::from(path?);
        crate::plugin_images::allowed(&self.plugin, &path, cx).then_some(path)
    }

    fn is_loud(&self, message: &str, part: usize) -> bool {
        self.loud
            .as_ref()
            .is_some_and(|(loud, at)| loud == message && *at == part)
    }

    fn current_voice(&self, message: &str, part: usize) -> Option<&Voice> {
        self.voice
            .as_ref()
            .filter(|voice| voice.message == message && voice.part == part)
    }

    /// A frame begins (the conversation is drawn): whether it is in sight; the players of what
    /// was on screen in the last frame start, the others go; the frames' timer follows.
    pub(super) fn begin_frame(
        &mut self,
        shown: bool,
        window: &mut Window,
        cx: &mut Context<Conversation>,
    ) {
        self.in_sight = shown && window_active(window);
        self.window = Some(window.window_handle());
        let last = self.sightings.frame.get();
        self.sightings.frame.set(last + 1);
        self.sightings.next.set(0);
        let wants = std::mem::take(&mut *self.sightings.wants.borrow_mut());
        // The round message with sound is the one `loud` names now (a click may have changed
        // it since the element was drawn).
        let loud_key = self
            .loud
            .as_ref()
            .map(|(message, part)| format!("{message}/{part}"));
        let mut wanted: Vec<(u64, String, Want)> = wants
            .into_iter()
            .map(|(key, (order, want))| {
                let want = match want {
                    Want::Video { path, .. } => Want::Video {
                        loud: Some(&key) == loud_key.as_ref(),
                        path,
                    },
                    other => other,
                };
                (key, (order, want))
            })
            .filter(|(key, (_, want))| {
                // A round message with sound plays wherever it is drawn; the rest only on screen.
                let loud = matches!(want, Want::Video { loud: true, .. });
                !self.broken.contains(key)
                    && (loud || (self.in_sight && self.sightings.on_screen(key, last)))
            })
            .map(|(key, (order, want))| (order, key, want))
            .collect();
        // The field's panel is over the feed: its items play first.
        wanted.sort_by_key(|(order, key, _)| (!key.starts_with("panel/"), *order));
        let (mut videos, mut animations) = (0, 0);
        let mut keep: HashMap<String, Want> = HashMap::new();
        for (_, key, want) in wanted {
            let room = match &want {
                Want::Video { loud: true, .. } => true,
                Want::Video { .. } => {
                    videos += 1;
                    videos <= MAX_VIDEOS
                }
                Want::Animation { .. } => {
                    animations += 1;
                    animations <= MAX_ANIMATIONS
                }
            };
            if room {
                keep.insert(key, want);
            }
        }
        // What isn't wanted any more goes (a video's decoder with it).
        let gone: Vec<String> = self
            .videos
            .iter()
            .filter(|(key, video)| match keep.get(*key) {
                Some(Want::Video { loud, .. }) => *loud != video.loud,
                _ => true,
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in gone {
            if let Some(video) = self.videos.remove(&key) {
                video.release(cx);
            }
        }
        let gone: Vec<String> = self
            .animations
            .keys()
            .filter(|key| !keep.contains_key(*key))
            .cloned()
            .collect();
        for key in gone {
            if let Some(playing) = self.animations.remove(&key) {
                playing.release(cx);
            }
        }
        for (key, want) in keep {
            match want {
                Want::Video { path, loud } if !self.videos.contains_key(&key) => {
                    self.open_video(&key, &path, loud, cx);
                }
                Want::Animation { path, looped } if !self.animations.contains_key(&key) => {
                    match Playing::open(&path, looped, cx) {
                        Ok(playing) => {
                            self.animations.insert(key, playing);
                        }
                        Err(error) => {
                            log::warn!("animation {}: {error}", path.display());
                            self.broken.insert(key);
                        }
                    }
                }
                _ => {}
            }
        }
        self.schedule(shown, cx);
    }

    /// Opens a video of the feed: without sound and looped, or with sound from the start.
    fn open_video(&mut self, key: &str, path: &Path, loud: bool, cx: &mut Context<Conversation>) {
        let options = VideoOptions {
            muted: !loud,
            looped: !loud,
        };
        let opened = VideoPlayer::open(path, options).and_then(|player| {
            player.play()?;
            Ok(player)
        });
        match opened {
            Ok(player) => {
                let changes = self.listen(cx);
                player.on_change(move || {
                    changes.unbounded_send(()).ok();
                });
                self.videos.insert(
                    key.to_string(),
                    Video {
                        player,
                        frame: None,
                        loud,
                    },
                );
            }
            Err(error) => {
                log::warn!("video {}: {error}", path.display());
                self.broken.insert(key.to_string());
            }
        }
    }

    /// The channel the players' changes come through (made once).
    fn listen(&mut self, cx: &mut Context<Conversation>) -> mpsc::UnboundedSender<()> {
        if let Some(changes) = &self.changes {
            return changes.clone();
        }
        let (sender, mut receiver) = mpsc::unbounded::<()>();
        self._listen = Some(cx.spawn(async move |this, cx| {
            while receiver.next().await.is_some() {
                // A burst of changes is one.
                while receiver.try_recv().is_ok() {}
                if this.update(cx, |this, cx| this.media_changed(cx)).is_err() {
                    break;
                }
            }
        }));
        self.changes = Some(sender.clone());
        sender
    }

    /// The frames' timer at the rate of what plays; none — nothing plays.
    fn schedule(&mut self, shown: bool, cx: &mut Context<Conversation>) {
        let mut step: Option<Duration> = None;
        let mut take = |each: Duration| {
            let each = each.max(MIN_TICK);
            step = Some(step.map_or(each, |step| step.min(each)));
        };
        for video in self.videos.values() {
            if video.player.state() == Playback::Playing {
                // At half the interval: polled at the interval itself, about one frame in nine
                // is missed.
                take(video.player.frame_interval() / 2);
            }
        }
        // Many at once: every other frame each (each frame of an animation is a new texture,
        // and every tick redraws the window).
        let playing = self
            .animations
            .values()
            .filter(|playing| !playing.done)
            .count();
        let stride = if playing >= CROWD { 2 } else { 1 };
        for playing in self.animations.values_mut() {
            playing.stride = stride;
            if !playing.done {
                take(playing.step());
            }
        }
        if self
            .voice
            .as_ref()
            .is_some_and(|voice| voice.player.state() == Playback::Playing)
        {
            take(if shown { VOICE_TICK } else { VOICE_TICK_UNSEEN });
        }
        let Some(step) = step else {
            self.ticker = None;
            return;
        };
        if self.ticker.as_ref().is_some_and(|(each, _)| *each == step) {
            return;
        }
        let window = self.window;
        self.ticker = Some((
            step,
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(step).await;
                    // The window's own activity: a minimized one draws no frames to tell it.
                    let active = window
                        .and_then(|window| {
                            cx.update_window(window, |_, window, _| window_active(window))
                                .ok()
                        })
                        .unwrap_or(true);
                    if this
                        .update(cx, |this, cx| this.media_tick(active, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            }),
        ));
    }

    // --- Drawing ---

    /// A voice message's part, `width` wide at most.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_voice(
        &self,
        message: &str,
        part: usize,
        spec: &VoiceSpec,
        outgoing: bool,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Conversation>,
    ) -> AnyElement {
        let _ = (outgoing, window);
        let ui = Theme::ui(cx);
        let current = self.current_voice(message, part);
        let playing = current.is_some_and(|voice| voice.player.state() == Playback::Playing);
        let length = Duration::from_secs(spec.duration.into());
        let duration = current
            .map(|voice| voice.player.duration())
            .filter(|duration| !duration.is_zero())
            .unwrap_or(length);
        let position = current.map(|voice| voice.player.position());
        let fraction = position.map_or(0., |position| {
            (position.as_secs_f32() / duration.as_secs_f32().max(0.001)).clamp(0., 1.)
        });
        let downloaded = self.allowed(spec.path.as_deref(), cx).is_some();
        let downloading = !downloaded && spec.progress.is_some();
        let failed = self.failed.get(&(message.to_string(), part)).cloned();
        let speed = crate::now_playing::speed(cx);

        let glyph = if downloading {
            icon(IconName::Close, ui.on_accent)
                .size(px(14.))
                .into_any_element()
        } else if playing {
            pause_glyph(14., ui.on_accent).into_any_element()
        } else {
            play_glyph(VOICE_BUTTON, ui.on_accent)
        };
        let button_message = message.to_string();
        let button = div()
            .id(SharedString::from(format!(
                "fg-voice-play-{message}-{part}"
            )))
            .relative()
            .flex_none()
            .size(px(VOICE_BUTTON))
            .rounded_full()
            .bg(ui.accent)
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|style| style.opacity(0.9))
            .child(glyph)
            .children(spec.progress.filter(|_| downloading).map(|done| {
                div().absolute().top(px(-3.)).left(px(-3.)).child(ring(
                    VOICE_BUTTON + 6.,
                    2.,
                    done,
                    ui.accent,
                    ui.divider,
                ))
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.click_voice(&button_message, part, cx)
            }));

        let wave_width = (width - VOICE_BUTTON - 10.).clamp(60., 240.);
        let bars = (((wave_width + WAVE_GAP) / (WAVE_BAR + WAVE_GAP)).floor() as usize).max(8);
        let levels = resample(&spec.waveform, bars);
        let played = (fraction * bars as f32).round() as usize;
        let dim = UiColors::tint(ui.foreground, 0.28);
        let accent = ui.accent;
        let wave_bounds: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::default();
        let (down_bounds, move_bounds) = (wave_bounds.clone(), wave_bounds.clone());
        let (down_message, move_message) = (message.to_string(), message.to_string());
        let wave = div()
            .id(SharedString::from(format!(
                "fg-voice-wave-{message}-{part}"
            )))
            .relative()
            .w(px(wave_width))
            .h(px(WAVE_HEIGHT))
            // The bars in one paint: as boxes they were ~60 more elements to lay out per voice
            // message, every frame something plays in the feed.
            .child(
                canvas(
                    move |bounds, _, _| wave_bounds.set(Some(bounds)),
                    move |bounds, _, window, _| {
                        for (index, level) in levels.iter().enumerate() {
                            let height = px((WAVE_HEIGHT * *level as f32 / 31.).max(2.));
                            let origin = point(
                                bounds.origin.x + px(index as f32 * (WAVE_BAR + WAVE_GAP)),
                                bounds.origin.y + (bounds.size.height - height) / 2.,
                            );
                            let color = if index < played { accent } else { dim };
                            let bar = Bounds::new(origin, size(px(WAVE_BAR), height));
                            window.paint_quad(fill(bar, color).corner_radii(px(1.)));
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            .when(downloaded, |wave| {
                wave.cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            let Some(at) = fraction_at(down_bounds.get(), event.position.x) else {
                                return;
                            };
                            this.media.seeking = Some((down_message.clone(), part));
                            this.seek_voice(&down_message, part, at, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                        let dragging = event.pressed_button == Some(MouseButton::Left)
                            && this.media.seeking.as_ref() == Some(&(move_message.clone(), part));
                        if let (true, Some(at)) =
                            (dragging, fraction_at(move_bounds.get(), event.position.x))
                        {
                            this.seek_voice(&move_message, part, at, cx);
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _, _| this.media.seeking = None),
                    )
            });

        let time = match (position, &failed) {
            (_, Some(error)) => error.to_string(),
            (Some(position), None) => format!("{} / {}", clock(position), clock(duration)),
            (None, None) => clock(length),
        };
        let speed_message = message.to_string();
        let details = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(16.))
            .text_size(px(theme::TEXT_XS))
            .text_color(if failed.is_some() {
                ui.error
            } else {
                ui.text_muted
            })
            .child(div().whitespace_nowrap().child(time))
            .when(current.is_some(), |details| {
                details.child(
                    speed_pill(
                        SharedString::from(format!("fg-voice-speed-{message}-{part}")),
                        speed,
                        ui,
                    )
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            let _ = &speed_message;
                            this.cycle_speed(cx)
                        },
                    )),
                )
            })
            .when(spec.unheard, |details| {
                details.child(div().size(px(6.)).rounded_full().bg(ui.accent))
            });

        div()
            .id(SharedString::from(format!("fg-voice-{message}-{part}")))
            .flex()
            .items_center()
            .gap(px(10.))
            .py(px(2.))
            .child(button)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(wave)
                    .child(details),
            )
            .into_any_element()
    }

    /// A video's part: a GIF, a round message, a video; `width` — the bubble's inner width.
    pub(super) fn render_video(
        &self,
        message: &str,
        part: usize,
        spec: &VideoSpec,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Conversation>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let key = format!("{message}/{part}");
        let round = spec.kind == VideoKind::Round;
        let loud = self.is_loud(message, part);
        let size = match spec.kind {
            VideoKind::Round if loud => (ROUND_LOUD_SIDE, ROUND_LOUD_SIDE),
            VideoKind::Round => (ROUND_SIDE, ROUND_SIDE),
            VideoKind::Video | VideoKind::Gif => super::fit(
                spec.poster.width,
                spec.poster.height,
                width.min(super::PHOTO_WIDTH),
                super::PHOTO_HEIGHT,
            ),
        };
        let path = self.allowed(spec.path.as_deref(), cx);
        let downloading = path.is_none() && spec.poster.progress.is_some();
        let plays_by_itself =
            spec.kind != VideoKind::Video && (spec.autoplay || self.started.contains(&key));
        if let Some(path) = &path
            && (plays_by_itself || loud)
        {
            self.sightings.want(
                &key,
                Want::Video {
                    path: path.clone(),
                    loud,
                },
            );
        }
        let video = self.videos.get(&key);
        let playing = video.is_some_and(|video| video.player.state() == Playback::Playing);
        let radius = if round {
            px(size.0 / 2.)
        } else {
            px(super::PICTURE_RADIUS)
        };
        let picture = match video.and_then(|video| video.frame.as_ref()) {
            Some(frame) => draw_frame(frame, size, radius, round),
            None => {
                let mut poster = spec.poster.clone();
                // The pill, ▶ and the download are drawn here.
                poster.label = None;
                poster.play = false;
                poster.progress = None;
                if round {
                    poster.shape = ImageShape::Circle;
                }
                render_picture(&poster, size, &self.plugin, cx.entity_id(), window, cx)
            }
        };
        let shade = |alpha: f32| gpui::black().opacity(alpha);
        let failed = self.failed.get(&(message.to_string(), part)).cloned();
        // ▶ where a click plays: a video, a GIF or a round message that doesn't play by itself.
        let play = !downloading && !playing && (spec.kind == VideoKind::Video || !plays_by_itself)
            || (loud && !playing);
        let center = if downloading {
            Some(
                div()
                    .relative()
                    .size(px(44.))
                    .rounded_full()
                    .bg(shade(0.45))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(IconName::Close, gpui::white()).size(px(16.)))
                    .child(div().absolute().inset_0().child(ring(
                        44.,
                        2.5,
                        spec.poster.progress.unwrap_or(0.),
                        gpui::white(),
                        shade(0.2),
                    )))
                    .into_any_element(),
            )
        } else if play {
            let side = (size.0.min(size.1) * 0.28).clamp(32., 48.);
            Some(
                div()
                    .size(px(side))
                    .rounded_full()
                    .bg(shade(0.45))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(play_glyph(side, gpui::white()))
                    .into_any_element(),
            )
        } else {
            None
        };
        let length = Duration::from_secs(spec.duration.into());
        let label = match spec.kind {
            VideoKind::Gif => tr("GIF").to_string(),
            VideoKind::Video => clock(length),
            VideoKind::Round => match video.filter(|_| loud) {
                Some(video) => format!(
                    "{} / {}",
                    clock(video.player.position()),
                    clock(
                        Some(video.player.duration())
                            .filter(|duration| !duration.is_zero())
                            .unwrap_or(length)
                    )
                ),
                None => clock(length),
            },
        };
        let label = match &failed {
            Some(error) => error.to_string(),
            None => label,
        };
        let pill = div()
            .h(px(18.))
            .px(px(6.))
            .flex()
            .items_center()
            .gap(px(4.))
            .rounded_full()
            .bg(shade(0.55))
            .text_size(px(theme::TEXT_XS))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(gpui::white())
            .whitespace_nowrap()
            .child(label)
            .when(round && spec.unheard, |pill| {
                pill.child(div().size(px(6.)).rounded_full().bg(ui.accent))
            });
        let pill = match round {
            true => div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px((size.1 * 0.08).max(6.)))
                .flex()
                .justify_center()
                .child(pill),
            false => div().absolute().top(px(6.)).left(px(6.)).child(pill),
        };
        // A round message with sound: its progress around it.
        let progress = video.filter(|_| loud).map(|video| {
            let duration = video.player.duration().as_secs_f32().max(0.001);
            let done = (video.player.position().as_secs_f32() / duration).clamp(0., 1.);
            div().absolute().top(px(-4.)).left(px(-4.)).child(ring(
                size.0 + 8.,
                3.,
                done,
                ui.accent,
                ui.divider,
            ))
        });
        let (click_message, menu_message) = (message.to_string(), message.to_string());
        div()
            .id(SharedString::from(format!("fg-video-{message}-{part}")))
            .relative()
            .flex_none()
            .w(px(size.0))
            .h(px(size.1))
            .cursor_pointer()
            .child(
                div()
                    .size_full()
                    .rounded(radius)
                    .overflow_hidden()
                    .child(picture),
            )
            .children(progress)
            .children(center.map(|center| {
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(center)
            }))
            .child(pill)
            .child(self.sighting(&key, cx))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.click_video(&click_message, part, window, cx)
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_menu(menu_message.clone(), Some(part), event.position, window, cx)
                }),
            )
            .into_any_element()
    }

    /// An animated sticker of a message (`spec.animation` is there): a click and the menu as a
    /// picture's.
    pub(super) fn render_sticker(
        &self,
        message: &str,
        part: usize,
        spec: &ImageSpec,
        size: (f32, f32),
        window: &mut Window,
        cx: &mut Context<Conversation>,
    ) -> AnyElement {
        let animation = self.render_animation(&format!("{message}/{part}"), spec, size, window, cx);
        let (click_message, menu_message) = (message.to_string(), message.to_string());
        div()
            .id(SharedString::from(format!("fg-pic-{message}-{part}")))
            .flex_none()
            .cursor_pointer()
            .child(animation)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.click_picture(&click_message, part, window, cx)
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_menu(menu_message.clone(), Some(part), event.position, window, cx)
                }),
            )
            .into_any_element()
    }

    /// A picture with its animation (an FXA file), named by `key` among the conversation's media:
    /// a message's sticker, an item of the field's panel. Without an animation — the picture.
    pub(super) fn render_animation(
        &self,
        key: &str,
        spec: &ImageSpec,
        size: (f32, f32),
        window: &mut Window,
        cx: &mut Context<Conversation>,
    ) -> AnyElement {
        if let Some(animation) = &spec.animation
            && let Some(path) = self.allowed(Some(&animation.path), cx)
        {
            self.sightings.want(
                key,
                Want::Animation {
                    path,
                    looped: animation.looped,
                },
            );
        }
        let picture = match self
            .animations
            .get(key)
            .and_then(|playing| playing.image.clone())
        {
            Some(image) => img(image)
                .w(px(size.0))
                .h(px(size.1))
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            // Until its first frame is read: the picture.
            None => render_picture(spec, size, &self.plugin, cx.entity_id(), window, cx),
        };
        div()
            .relative()
            .flex_none()
            .w(px(size.0))
            .h(px(size.1))
            .child(picture)
            .child(self.sighting(key, cx))
            .into_any_element()
    }

    /// A GIF of the field's panel, named by `key`: its MP4 (`path`) looped without sound while on
    /// screen, the poster until then.
    #[allow(dead_code)] // The field's panel draws its GIFs with it (`panel.rs`).
    pub(super) fn render_gif(
        &self,
        key: &str,
        path: Option<&str>,
        poster: &ImageSpec,
        size: (f32, f32),
        window: &mut Window,
        cx: &mut Context<Conversation>,
    ) -> AnyElement {
        if let Some(path) = self.allowed(path, cx) {
            self.sightings.want(key, Want::Video { path, loud: false });
        }
        let radius = px(super::PICTURE_RADIUS);
        let picture = match self.videos.get(key).and_then(|video| video.frame.as_ref()) {
            Some(frame) => draw_frame(frame, size, radius, false),
            None => render_picture(poster, size, &self.plugin, cx.entity_id(), window, cx),
        };
        div()
            .relative()
            .flex_none()
            .w(px(size.0))
            .h(px(size.1))
            .rounded(radius)
            .overflow_hidden()
            .child(picture)
            .child(self.sighting(key, cx))
            .into_any_element()
    }

    /// An empty element over a media element that records, as it is laid out, whether it is on
    /// screen — within the area it is clipped to (the feed's, the panel's); a change brings the
    /// next frame, which starts or stops its player.
    fn sighting(&self, key: &str, cx: &mut Context<Conversation>) -> impl IntoElement + use<> {
        let sightings = self.sightings.clone();
        let key = key.to_string();
        let view = cx.entity_id();
        canvas(
            move |bounds, window, cx| {
                let clip = window.content_mask().bounds;
                let on_screen = bounds.intersects(&clip);
                if sightings.record(&key, on_screen) {
                    cx.defer(move |cx| cx.notify(view));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0()
    }

    /// The bar over the feed while a voice message (or a round one with sound) of this
    /// conversation plays; none — nothing plays.
    pub(super) fn render_playing_bar(
        &self,
        window: &mut Window,
        cx: &mut Context<Conversation>,
    ) -> Option<AnyElement> {
        let _ = window;
        let voice = self.voice.as_ref()?;
        let ui = Theme::ui(cx);
        let playing = voice.player.state() == Playback::Playing;
        let (position, duration) = (voice.player.position(), voice.player.duration());
        let fraction = (position.as_secs_f32() / duration.as_secs_f32().max(0.001)).clamp(0., 1.);
        let speed = crate::now_playing::speed(cx);
        let message = voice.message.clone();
        let toggle = div()
            .id("fg-playing-toggle")
            .flex_none()
            .size(px(24.))
            .rounded(px(crate::ui::RADIUS_SM))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|style| style.bg(ui.hover))
            .child(match playing {
                true => pause_glyph(12., ui.accent_text).into_any_element(),
                false => play_glyph(24., ui.accent_text),
            })
            .tooltip(crate::ui::tooltip(
                if playing { tr("Pause") } else { tr("Play") },
                None,
            ))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.toggle_now_playing(cx)
            }));
        let line = div()
            .flex_1()
            .min_w(px(40.))
            .h(px(3.))
            .rounded(px(1.5))
            .bg(ui.divider)
            .child(
                div()
                    .h_full()
                    .rounded(px(1.5))
                    .bg(ui.accent)
                    .w(relative(fraction)),
            );
        Some(
            div()
                .id("fg-playing")
                .flex_none()
                .h(px(BAR_HEIGHT))
                .px(px(12.))
                .flex()
                .items_center()
                .gap(px(10.))
                .border_b_1()
                .border_color(ui.divider)
                .text_size(px(theme::TEXT_SM))
                .cursor_pointer()
                .child(toggle)
                .child(
                    div()
                        .flex_none()
                        .max_w(px(220.))
                        .truncate()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(ui.foreground)
                        .child(voice.who.clone()),
                )
                // In a one-to-one chat the message has no author: its name is the kind already.
                .when(voice.who.as_ref() != tr("Voice message"), |bar| {
                    bar.child(
                        div()
                            .flex_none()
                            .text_color(ui.text_muted)
                            .child(tr("Voice message")),
                    )
                })
                .child(line)
                .child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .text_color(ui.text_muted)
                        .child(format!("{} / {}", clock(position), clock(duration))),
                )
                .child(
                    speed_pill("fg-playing-speed", speed, ui).on_click(cx.listener(
                        |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.cycle_speed(cx)
                        },
                    )),
                )
                .child(
                    crate::ui::icon_button("fg-playing-stop", IconName::Close, ui)
                        .tooltip(crate::ui::tooltip(tr("Stop"), None))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.stop_voice(cx)
                        })),
                )
                // A click on the bar shows the message.
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.reveal(&message);
                    cx.notify();
                }))
                .into_any_element(),
        )
    }
}

impl Conversation {
    // --- Voice messages ---

    /// ▶ / ⏸ of a voice part: the one played pauses or goes on; another one plays from its start
    /// (one not downloaded asks the plugin, and a click while it downloads cancels).
    fn click_voice(&mut self, message: &str, part: usize, cx: &mut Context<Self>) {
        let key = (message.to_string(), part);
        self.media.failed.remove(&key);
        if let Some(voice) = self.media.current_voice(message, part) {
            match voice.player.state() {
                Playback::Playing => voice.player.pause(),
                Playback::Paused | Playback::Ended => self.play_voice(cx),
            }
            return self.media_changed(cx);
        }
        let Some(MessagePart::Voice(spec)) = self
            .messages
            .get(message)
            .and_then(|found| found.parts.get(part))
            .cloned()
        else {
            return;
        };
        match self.media.allowed(spec.path.as_deref(), cx) {
            Some(path) => self.start_voice(message, part, &path, None, true, cx),
            None => {
                // The plugin downloads it; pressed again while it does — it cancels.
                self.media.wanted =
                    match spec.progress.is_some() && self.media.wanted == Some(key.clone()) {
                        true => None,
                        false => Some(key),
                    };
                self.emit(UiEvent::PartClicked((message.to_string(), part as u32)), cx);
                cx.notify();
            }
        }
    }

    /// Opens a voice part's file and plays it (or only goes to `at`, a share of its length).
    fn start_voice(
        &mut self,
        message: &str,
        part: usize,
        path: &Path,
        at: Option<f32>,
        play: bool,
        cx: &mut Context<Self>,
    ) {
        // The one that played stops (its sound device closes).
        self.media.voice = None;
        self.media.wanted = None;
        let player = match VoicePlayer::open(path) {
            Ok(player) => player,
            Err(error) => {
                log::warn!("voice message {}: {error}", path.display());
                self.media
                    .failed
                    .insert((message.to_string(), part), short_error(&error));
                crate::now_playing::clear(cx.entity_id(), cx);
                return cx.notify();
            }
        };
        player.set_speed(crate::now_playing::speed(cx));
        let changes = self.media.listen(cx);
        player.on_change(move || {
            changes.unbounded_send(()).ok();
        });
        if let Some(at) = at {
            player.seek(player.duration().mul_f32(at.clamp(0., 1.)));
        }
        let who = self.who(message);
        self.media.voice = Some(Voice {
            message: message.to_string(),
            part,
            who,
            player,
        });
        crate::now_playing::set(cx.entity().downgrade(), cx);
        if play {
            self.play_voice(cx);
        }
        self.media_changed(cx);
    }

    /// The voice message's author, as the bar and the status line name it.
    fn who(&self, message: &str) -> SharedString {
        match self.messages.get(message) {
            Some(found) if found.outgoing => tr("You").into(),
            Some(found) => found
                .author
                .as_ref()
                .map(|author| SharedString::from(author.name.clone()))
                .unwrap_or_else(|| tr("Voice message").into()),
            None => tr("Voice message").into(),
        }
    }

    /// The current voice message plays; its first play since it was put goes to the plugin.
    fn play_voice(&mut self, cx: &mut Context<Self>) {
        let Some(voice) = &self.media.voice else {
            return;
        };
        let key = (voice.message.clone(), voice.part);
        if let Err(error) = voice.player.play() {
            log::warn!("voice message: {error}");
            self.media.failed.insert(key, short_error(&error));
            return;
        }
        // Whatever played with sound pauses: a round one goes back to its small loop.
        self.quiet_round(cx);
        if self.media.played.insert(key.clone()) {
            self.emit(UiEvent::MediaPlayed((key.0, key.1 as u32)), cx);
        }
    }

    /// A click or a drag on a waveform: the voice there goes to `at` (a share of its length),
    /// playing or not; another downloaded one becomes the current one, paused there.
    fn seek_voice(&mut self, message: &str, part: usize, at: f32, cx: &mut Context<Self>) {
        if let Some(voice) = self.media.current_voice(message, part) {
            voice
                .player
                .seek(voice.player.duration().mul_f32(at.clamp(0., 1.)));
            return self.media_changed(cx);
        }
        let path = match self
            .messages
            .get(message)
            .and_then(|found| found.parts.get(part))
        {
            Some(MessagePart::Voice(spec)) => self.media.allowed(spec.path.as_deref(), cx),
            _ => None,
        };
        if let Some(path) = path {
            self.start_voice(message, part, &path, Some(at), false, cx);
        }
    }

    /// The speed button: 1× → 1.5× → 2× → 1×, for every voice message from now on.
    fn cycle_speed(&mut self, cx: &mut Context<Self>) {
        let speed = crate::now_playing::speed(cx);
        let next = SPEEDS
            .iter()
            .position(|known| (known - speed).abs() < 0.01)
            .map_or(SPEEDS[0], |at| SPEEDS[(at + 1) % SPEEDS.len()]);
        crate::now_playing::set_speed(next, cx);
        if let Some(voice) = &self.media.voice {
            voice.player.set_speed(next);
        }
        cx.notify();
    }

    /// × of the playing bar: the voice message stops, the bar and the status line go.
    fn stop_voice(&mut self, cx: &mut Context<Self>) {
        self.media.voice = None;
        crate::now_playing::clear(cx.entity_id(), cx);
        self.media_changed(cx);
    }

    /// The voice message after `message`'s `part` in the feed plays — downloaded, or asked for.
    fn play_next_voice(&mut self, message: &str, part: usize, cx: &mut Context<Self>) {
        let Some(start) = self.position(message) else {
            return;
        };
        let next = self.order[start..]
            .iter()
            .enumerate()
            .find_map(|(offset, id)| {
                self.messages[id]
                    .parts
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| offset > 0 || *index > part)
                    .find_map(|(index, found)| match found {
                        MessagePart::Voice(spec) => Some((id.clone(), index, spec.path.clone())),
                        _ => None,
                    })
            });
        let Some((next, index, path)) = next else {
            return;
        };
        match self.media.allowed(path.as_deref(), cx) {
            Some(path) => self.start_voice(&next, index, &path, None, true, cx),
            None => {
                self.media.wanted = Some((next.clone(), index));
                self.emit(UiEvent::PartClicked((next, index as u32)), cx);
            }
        }
    }

    // --- Videos ---

    /// A click on a video part: a video (and a GIF that plays by itself) opens the media viewer;
    /// a round message plays with sound, pauses, goes on; a GIF that doesn't play by itself
    /// starts (or stops); one not downloaded asks the plugin.
    fn click_video(
        &mut self,
        message: &str,
        part: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(MessagePart::Video(spec)) = self
            .messages
            .get(message)
            .and_then(|found| found.parts.get(part))
            .cloned()
        else {
            return;
        };
        let key = format!("{message}/{part}");
        self.media.failed.remove(&(message.to_string(), part));
        self.media.broken.remove(&key);
        let path = self.media.allowed(spec.path.as_deref(), cx);
        match spec.kind {
            VideoKind::Video => return self.open_media_viewer(message, part, window, cx),
            VideoKind::Gif if spec.autoplay && path.is_some() => {
                return self.open_media_viewer(message, part, window, cx);
            }
            VideoKind::Gif => {
                match path {
                    Some(_) if !self.media.started.remove(&key) => {
                        self.media.started.insert(key);
                    }
                    Some(_) => {}
                    None => self.emit(UiEvent::PartClicked((message.to_string(), part as u32)), cx),
                }
                return cx.notify();
            }
            VideoKind::Round => {}
        }
        // A round message.
        if path.is_none() {
            let wanted = (message.to_string(), part);
            self.media.wanted =
                match spec.poster.progress.is_some() && self.media.wanted == Some(wanted.clone()) {
                    true => None,
                    false => Some(wanted),
                };
            self.emit(UiEvent::PartClicked((message.to_string(), part as u32)), cx);
            return cx.notify();
        }
        if self.media.is_loud(message, part) {
            if let Some(video) = self.media.videos.get(&key) {
                match video.player.state() {
                    Playback::Playing => {
                        video.player.pause();
                        self.media.loud_paused = true;
                    }
                    _ => {
                        self.media.loud_paused = false;
                        video.player.play().ok();
                    }
                }
            }
            return self.media_changed(cx);
        }
        self.play_round(message, part, cx);
    }

    /// A round message plays with sound from its start, larger.
    fn play_round(&mut self, message: &str, part: usize, cx: &mut Context<Self>) {
        // The voice message pauses (one sound at a time — the player's sound would too).
        if let Some(voice) = &self.media.voice {
            voice.player.pause();
        }
        self.quiet_round(cx);
        self.media.wanted = None;
        let key = format!("{message}/{part}");
        // The muted loop's player goes: the one with sound starts from the beginning.
        if let Some(video) = self.media.videos.remove(&key) {
            video.release(cx);
        }
        self.media.loud = Some((message.to_string(), part));
        self.media.loud_paused = false;
        // Opened here, on the click: the frame after it shows it larger, playing.
        let path = match self
            .messages
            .get(message)
            .and_then(|found| found.parts.get(part))
        {
            Some(MessagePart::Video(spec)) => self.media.allowed(spec.path.as_deref(), cx),
            _ => None,
        };
        if let Some(path) = path {
            self.media.open_video(&key, &path, true, cx);
        }
        let pair = (message.to_string(), part);
        if self.media.played.insert(pair.clone()) {
            self.emit(UiEvent::MediaPlayed((pair.0, pair.1 as u32)), cx);
        }
        cx.notify();
    }

    /// The round message with sound goes back to its small muted loop.
    fn quiet_round(&mut self, cx: &mut Context<Self>) {
        if let Some((message, part)) = self.media.loud.take() {
            if let Some(video) = self.media.videos.remove(&format!("{message}/{part}")) {
                video.release(cx);
            }
            self.media.loud_paused = false;
            cx.notify();
        }
    }

    /// Opens the media viewer on a video or a GIF among the feed's pictures and videos; one not
    /// downloaded opens at once on its poster with a spinner, the plugin downloads it, and the
    /// viewer plays it when it comes ([`Self::media_put`]).
    fn open_media_viewer(
        &mut self,
        message: &str,
        part: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut items = Vec::new();
        let mut clicked = None;
        for id in &self.order {
            for (index, found) in self.messages[id].parts.iter().enumerate() {
                let this = id == message && index == part;
                let item = match found {
                    MessagePart::Image(spec) if !spec.play => {
                        let full = self.allowed_path(spec.full.as_ref(), cx);
                        let path = self.allowed_path(spec.path.as_ref(), cx);
                        match full.or(path) {
                            Some(path) => MediaItem::Picture(ImageSource::File(path)),
                            None => continue,
                        }
                    }
                    MessagePart::Video(spec) if spec.kind != VideoKind::Round => {
                        let path = self.media.allowed(spec.path.as_deref(), cx);
                        if path.is_none() && !this {
                            continue;
                        }
                        MediaItem::Video(VideoItem {
                            path,
                            poster: self
                                .allowed_path(spec.poster.path.as_ref(), cx)
                                .map(ImageSource::File),
                            duration: spec.duration,
                            gif: spec.kind == VideoKind::Gif,
                        })
                    }
                    _ => continue,
                };
                if this {
                    clicked = Some(items.len());
                }
                items.push(item);
            }
        }
        let Some(index) = clicked else {
            return;
        };
        let waiting = matches!(&items[index], MediaItem::Video(video) if video.path.is_none());
        if waiting {
            self.emit(UiEvent::PartClicked((message.to_string(), part as u32)), cx);
        }
        // The viewer's video takes the sound.
        if let Some(voice) = &self.media.voice {
            voice.player.pause();
        }
        self.quiet_round(cx);
        let popup = crate::image_popup::show_media(items, index, window, cx);
        self.media.viewer = match (waiting, popup) {
            (true, Some(popup)) => {
                popup.update(cx, |popup, cx| popup.set_loading(index, true, cx));
                Some(WaitingViewer {
                    popup: popup.downgrade(),
                    message: message.to_string(),
                    part,
                    index,
                })
            }
            _ => None,
        };
        cx.notify();
    }

    // --- Changes ---

    /// Messages were put: their parts may be played again as new (`media-played`), may play
    /// now — ▶ pressed before the download — and the viewer waiting for a video gets its file.
    pub(super) fn media_put(&mut self, ids: &[String], cx: &mut Context<Self>) {
        for id in ids {
            self.media.played.retain(|(message, _)| message != id);
            self.media.failed.retain(|(message, _), _| message != id);
            let prefix = format!("{id}/");
            self.media.broken.retain(|key| !key.starts_with(&prefix));
        }
        if let Some((message, part)) = self.media.wanted.clone()
            && ids.contains(&message)
        {
            match self
                .messages
                .get(&message)
                .and_then(|found| found.parts.get(part))
                .cloned()
            {
                Some(MessagePart::Voice(spec)) => {
                    if let Some(path) = self.media.allowed(spec.path.as_deref(), cx) {
                        self.start_voice(&message, part, &path, None, true, cx);
                    }
                }
                Some(MessagePart::Video(spec)) => {
                    if self.media.allowed(spec.path.as_deref(), cx).is_some() {
                        self.play_round(&message, part, cx);
                    }
                }
                _ => self.media.wanted = None,
            }
        }
        let Some(viewer) = &self.media.viewer else {
            return;
        };
        if !ids.contains(&viewer.message) {
            return;
        }
        let Some(popup) = viewer.popup.upgrade() else {
            self.media.viewer = None;
            return;
        };
        let (message, part, index) = (viewer.message.clone(), viewer.part, viewer.index);
        if let Some(MessagePart::Video(spec)) = self
            .messages
            .get(&message)
            .and_then(|found| found.parts.get(part))
            .cloned()
            && let Some(path) = self.media.allowed(spec.path.as_deref(), cx)
        {
            popup.update(cx, |popup, cx| {
                popup.set_video_path(index, path, cx);
                popup.set_loading(index, false, cx);
            });
            self.media.viewer = None;
        }
    }

    /// A player changed by itself (the end, another sound, the first frame), or the user did
    /// something: the next voice message at the end, the round one back to its loop, the timer
    /// and the status bar's line follow.
    fn media_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(voice) = &self.media.voice
            && voice.player.state() == Playback::Ended
        {
            let (message, part) = (voice.message.clone(), voice.part);
            self.media.voice = None;
            crate::now_playing::clear(cx.entity_id(), cx);
            self.play_next_voice(&message, part, cx);
        }
        if let Some((message, part)) = self.media.loud.clone()
            && let Some(video) = self.media.videos.get(&format!("{message}/{part}"))
        {
            let state = video.player.state();
            // At its end, or paused by another sound (not before it started): back to the small
            // loop.
            let taken = state == Playback::Paused
                && !self.media.loud_paused
                && !video.player.position().is_zero();
            if state == Playback::Ended || taken {
                self.quiet_round(cx);
            }
        }
        // A video that failed while playing: its poster and a word on it instead.
        let failed: Vec<(String, String)> = self
            .media
            .videos
            .iter()
            .filter_map(|(key, video)| Some((key.clone(), video.player.error()?)))
            .collect();
        for (key, error) in failed {
            log::warn!("video {key}: {error}");
            if let Some(video) = self.media.videos.remove(&key) {
                video.release(cx);
            }
            if let Some((message, part)) = key.rsplit_once('/')
                && let Ok(part) = part.parse::<usize>()
            {
                self.media
                    .failed
                    .insert((message.to_string(), part), tr("Can't be played").into());
            }
            self.media.broken.insert(key);
        }
        let shown = self.shown;
        self.media.schedule(shown, cx);
        self.media.second = None;
        crate::now_playing::refresh(cx);
        cx.notify();
    }

    /// The frames' timer: new frames of the videos and the animations on screen, the voice
    /// message's place; out of sight (another tab, an inactive or minimized window) what plays
    /// on screen stops.
    fn media_tick(&mut self, active: bool, cx: &mut Context<Self>) {
        let shown = self.shown;
        let mut changed = false;
        if !(shown && active)
            && (!self.media.videos.is_empty() || !self.media.animations.is_empty())
        {
            let loud = self
                .media
                .loud
                .as_ref()
                .map(|(message, part)| format!("{message}/{part}"));
            let quiet: Vec<String> = self
                .media
                .videos
                .keys()
                .filter(|key| Some(*key) != loud.as_ref())
                .cloned()
                .collect();
            for key in quiet {
                if let Some(video) = self.media.videos.remove(&key) {
                    video.release(cx);
                }
            }
            for (_, playing) in self.media.animations.drain() {
                playing.release(cx);
            }
            changed = true;
        }
        for video in self.media.videos.values_mut() {
            if let Some(frame) = video.player.frame() {
                video.set_frame(frame, cx);
                changed = true;
            }
        }
        let now = Instant::now();
        for playing in self.media.animations.values_mut() {
            changed |= playing.advance(now, cx);
        }
        if let Some(voice) = &self.media.voice
            && voice.player.state() == Playback::Playing
        {
            changed |= shown;
            let second = voice.player.position().as_secs();
            if self.media.second != Some(second) {
                self.media.second = Some(second);
                crate::now_playing::refresh(cx);
            }
        }
        self.media.schedule(shown, cx);
        if changed {
            cx.notify();
        }
    }

    // --- What plays, for the status bar ---

    /// The voice message playing or paused here: who, where.
    pub(crate) fn now_playing(&self) -> Option<NowPlaying> {
        let voice = self.media.voice.as_ref()?;
        Some(NowPlaying {
            who: voice.who.clone(),
            position: voice.player.position(),
            playing: voice.player.state() == Playback::Playing,
        })
    }

    /// ⏸ / ▶ of the bar and of the status bar's line.
    pub(crate) fn toggle_now_playing(&mut self, cx: &mut Context<Self>) {
        let Some(voice) = &self.media.voice else {
            return;
        };
        match voice.player.state() {
            Playback::Playing => voice.player.pause(),
            Playback::Paused | Playback::Ended => self.play_voice(cx),
        }
        self.media_changed(cx);
    }
}

/// Where a click at `x` falls on a waveform: a share of its width.
fn fraction_at(bounds: Option<Bounds<Pixels>>, x: Pixels) -> Option<f32> {
    let bounds = bounds?;
    let width = f32::from(bounds.size.width);
    (width > 0.).then(|| (f32::from(x - bounds.origin.x) / width).clamp(0., 1.))
}

/// A waveform's levels (0–31) as `bars` bars: each the loudest of the samples it covers; no data
/// — an even line.
fn resample(levels: &[u8], bars: usize) -> Vec<u8> {
    if levels.is_empty() {
        return vec![WAVE_FLAT; bars];
    }
    (0..bars)
        .map(|bar| {
            let start = bar * levels.len() / bars;
            let end = ((bar + 1) * levels.len() / bars).max(start + 1);
            levels[start..end.min(levels.len())]
                .iter()
                .copied()
                .max()
                .unwrap_or(0)
                .min(31)
        })
        .collect()
}

/// "0:07", "1:24", "1:02:03".
fn clock(time: Duration) -> String {
    let seconds = time.as_secs();
    match seconds >= 3600 {
        true => format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        ),
        false => format!("{}:{:02}", seconds / 60, seconds % 60),
    }
}

/// What a part that can't be played says under its ▶.
fn short_error(error: &MediaError) -> SharedString {
    match error {
        MediaError::Unsupported(_) => tr("Can't be played here").into(),
        MediaError::Device(_) => tr("No sound device").into(),
        MediaError::Io(_) | MediaError::Format(_) => tr("Can't be played").into(),
    }
}

/// ▶ for a button `side` across: the triangle nudged right, where its weight looks centered.
fn play_glyph(side: f32, color: Hsla) -> AnyElement {
    icon(IconName::Play, color)
        .size(px((side * 0.42).round()))
        .ml(px(side * 0.05))
        .into_any_element()
}

/// ⏸: two bars, `side` tall.
fn pause_glyph(side: f32, color: Hsla) -> Div {
    let bar = || {
        div()
            .w(px((side * 0.24).round().max(2.)))
            .h(px(side))
            .rounded(px(1.))
            .bg(color)
    };
    div()
        .flex()
        .items_center()
        .gap(px((side * 0.22).round().max(2.)))
        .child(bar())
        .child(bar())
}

/// The speed's pill: "1×", "1.5×", "2×".
fn speed_pill(id: impl Into<gpui::ElementId>, speed: f32, ui: UiColors) -> gpui::Stateful<Div> {
    let label = match speed.fract() == 0. {
        true => format!("{}×", speed as u32),
        false => format!("{speed:.1}×").replace('.', tr(".")),
    };
    div()
        .id(id)
        .flex_none()
        .h(px(16.))
        .px(px(4.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .border_1()
        .border_color(ui.accent_text)
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(ui.accent_text)
        .cursor_pointer()
        .hover(|style| style.bg(ui.hover))
        .child(label)
}

/// A frame of a video at `size`, its corners rounded by `radius`.
fn draw_frame(frame: &Shown, size: (f32, f32), radius: Pixels, round: bool) -> AnyElement {
    match frame {
        Shown::Image(image) => img(image.clone())
            .w(px(size.0))
            .h(px(size.1))
            .map(|image| match round {
                true => image.rounded_full(),
                false => image.rounded(radius),
            })
            .object_fit(ObjectFit::Cover)
            .into_any_element(),
        Shown::Surface(frame) => match frame {
            #[cfg(target_os = "macos")]
            // The decoder's buffer fit into the box (its corners rounded where it is drawn — a
            // surface can't be cut to cover the box): the box has the video's proportions.
            Frame::Surface(buffer) => gpui::surface(buffer.clone())
                .w(px(size.0))
                .h(px(size.1))
                .object_fit(ObjectFit::Contain)
                .map(|surface| match round {
                    true => surface.rounded_full(),
                    false => surface.rounded(radius),
                })
                .into_any_element(),
            _ => div().into_any_element(),
        },
    }
}

/// A ring `side` across: the track, and the arc of `fraction` from the top, clockwise.
fn ring(side: f32, line: f32, fraction: f32, color: Hsla, track: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let center = bounds.center();
            let radius = px((side - line) / 2.);
            let top = point(center.x, center.y - radius);
            let bottom = point(center.x, center.y + radius);
            let circle = |path: &mut PathBuilder| {
                path.move_to(top);
                path.arc_to(point(radius, radius), px(0.), false, true, bottom);
                path.arc_to(point(radius, radius), px(0.), false, true, top);
            };
            let mut back = PathBuilder::stroke(px(line));
            circle(&mut back);
            if let Ok(path) = back.build() {
                window.paint_path(path, track);
            }
            let fraction = fraction.clamp(0., 1.);
            if fraction < 0.005 {
                return;
            }
            let mut arc = PathBuilder::stroke(px(line));
            if fraction >= 0.995 {
                circle(&mut arc);
            } else {
                let angle = fraction * std::f32::consts::TAU;
                let end = point(
                    center.x + radius * angle.sin(),
                    center.y - radius * angle.cos(),
                );
                arc.move_to(top);
                arc.arc_to(point(radius, radius), px(0.), fraction > 0.5, true, end);
            }
            if let Ok(path) = arc.build() {
                window.paint_path(path, color);
            }
        },
    )
    .size(px(side))
    .flex_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_waveform_is_resampled_to_the_bars_that_fit() {
        assert_eq!(resample(&[], 3), vec![WAVE_FLAT; 3]);
        // Fewer bars: each the loudest of its samples.
        assert_eq!(resample(&[1, 9, 2, 3, 31, 0], 3), vec![9, 3, 31]);
        // More bars: samples repeat.
        assert_eq!(resample(&[5, 10], 4), vec![5, 5, 10, 10]);
        // Out of range values are clamped.
        assert_eq!(resample(&[40], 1), vec![31]);
    }

    #[test]
    fn times_read_as_a_player_shows_them() {
        assert_eq!(clock(Duration::from_secs(7)), "0:07");
        assert_eq!(clock(Duration::from_secs(84)), "1:24");
        assert_eq!(clock(Duration::from_secs(3723)), "1:02:03");
    }

    #[test]
    fn a_media_element_is_on_screen_as_of_the_frame_before() {
        let sightings = Sightings::default();
        sightings.frame.set(1);
        assert!(sightings.record("a", true), "came on screen");
        assert!(!sightings.record("b", false), "never was");
        sightings.frame.set(2);
        assert!(sightings.on_screen("a", 1));
        assert!(!sightings.record("a", true), "still there");
        sightings.frame.set(3);
        assert!(sightings.record("a", false), "left");
        // Not drawn for a frame: it counts as gone.
        sightings.frame.set(5);
        assert!(sightings.record("a", true));
    }
}
