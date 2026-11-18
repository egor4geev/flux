//! A video in the media viewer (FluxGram part 4, [`crate::image_popup`]): played with sound by
//! the system's decoder (`flux-media`), fit into the viewer; under it a player's bar — ▶ / ⏸
//! (space, a click on the picture), where it is and its length, the seek bar (a click or a drag),
//! the volume, the speed, the window's full screen. A GIF plays looped. Frames are asked for at
//! the video's rate only while it plays; dropping the view lets the decoder go.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use flux_media::Playback;
use flux_media::video::{Frame, VideoOptions, VideoPlayer};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    AnyElement, Bounds, ClickEvent, Context, Div, MouseButton, MouseDownEvent, MouseMoveEvent,
    ObjectFit, Pixels, Render, RenderImage, SharedString, Task, Window, canvas, div, img,
    prelude::*, px, relative,
};

use crate::i18n::tr;
use crate::icons::{IconName, icon};
use crate::theme::{self, Theme};
use crate::ui;

/// The speeds the viewer's speed button goes round.
const SPEEDS: [f32; 4] = [1.0, 1.5, 2.0, 0.5];
const BAR_HEIGHT: f32 = 44.;

/// A frame on screen.
enum Shown {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Surface(Frame),
    Image(Arc<RenderImage>),
}

pub struct VideoView {
    path: PathBuf,
    player: Option<VideoPlayer>,
    error: Option<SharedString>,
    frame: Option<Shown>,
    volume: f32,
    speed: f32,
    /// The seek bar as last drawn: a click's place in it.
    seek_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    volume_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    seeking: bool,
    ticker: Option<Task<()>>,
    _listen: Option<Task<()>>,
}

impl VideoView {
    /// Opens the file and plays it with sound (a GIF looped).
    pub fn new(path: PathBuf, looped: bool, cx: &mut Context<Self>) -> Self {
        let mut view = VideoView {
            path,
            player: None,
            error: None,
            frame: None,
            volume: 1.,
            speed: 1.,
            seek_bounds: Rc::default(),
            volume_bounds: Rc::default(),
            seeking: false,
            ticker: None,
            _listen: None,
        };
        let options = VideoOptions {
            muted: false,
            looped,
        };
        match VideoPlayer::open(&view.path, options).and_then(|player| {
            player.play()?;
            Ok(player)
        }) {
            Ok(player) => {
                let (sender, mut receiver) = mpsc::unbounded::<()>();
                player.on_change(move || {
                    sender.unbounded_send(()).ok();
                });
                view._listen = Some(cx.spawn(async move |this, cx| {
                    while receiver.next().await.is_some() {
                        while receiver.try_recv().is_ok() {}
                        if this.update(cx, |this, cx| this.changed(cx)).is_err() {
                            break;
                        }
                    }
                }));
                view.player = Some(player);
                view.changed(cx);
            }
            Err(error) => {
                log::warn!("video {}: {error}", view.path.display());
                view.error = Some(tr("This video can't be played here").into());
            }
        }
        view
    }

    /// The player changed (played, paused, the end, another sound): the frames' timer follows.
    fn changed(&mut self, cx: &mut Context<Self>) {
        let playing = self
            .player
            .as_ref()
            .is_some_and(|player| player.state() == Playback::Playing);
        if !playing {
            self.ticker = None;
            // The frame it stopped at.
            self.pull_frame(cx);
        } else if self.ticker.is_none() {
            // At half the video's interval: polled at the interval itself, about one frame in
            // nine is missed.
            let step = (self
                .player
                .as_ref()
                .map_or(Duration::from_millis(33), VideoPlayer::frame_interval)
                / 2)
            .max(Duration::from_millis(8));
            self.ticker = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(step).await;
                    if this.update(cx, |this, cx| this.pull_frame(cx)).is_err() {
                        break;
                    }
                }
            }));
        }
        cx.notify();
    }

    /// The decoder's new frame, if there is one (drawn with the time on the bar); none — nothing
    /// is drawn.
    fn pull_frame(&mut self, cx: &mut Context<Self>) {
        let Some(frame) = self.player.as_ref().and_then(VideoPlayer::frame) else {
            return;
        };
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
        cx.notify();
    }

    /// Space, a click on the picture: plays or pauses.
    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        let Some(player) = &self.player else {
            return;
        };
        match player.state() {
            Playback::Playing => player.pause(),
            Playback::Paused | Playback::Ended => {
                if let Err(error) = player.play() {
                    log::warn!("video: {error}");
                }
            }
        }
        self.changed(cx);
    }

    fn seek(&mut self, at: f32, cx: &mut Context<Self>) {
        if let Some(player) = &self.player {
            player.seek(player.duration().mul_f32(at.clamp(0., 1.)));
            self.pull_frame(cx);
            cx.notify();
        }
    }

    fn set_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        self.volume = volume.clamp(0., 1.);
        if let Some(player) = &self.player {
            player.set_volume(self.volume);
        }
        cx.notify();
    }

    fn cycle_speed(&mut self, cx: &mut Context<Self>) {
        let at = SPEEDS
            .iter()
            .position(|speed| (speed - self.speed).abs() < 0.01)
            .unwrap_or(0);
        self.speed = SPEEDS[(at + 1) % SPEEDS.len()];
        if let Some(player) = &self.player {
            player.set_speed(self.speed);
        }
        cx.notify();
    }

    fn render_bar(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let (playing, position, duration) = match &self.player {
            Some(player) => (
                player.state() == Playback::Playing,
                player.position(),
                player.duration(),
            ),
            None => (false, Duration::ZERO, Duration::ZERO),
        };
        let fraction = (position.as_secs_f32() / duration.as_secs_f32().max(0.001)).clamp(0., 1.);
        let white = gpui::white();
        let track = gpui::white().opacity(0.25);
        let (seek_bounds, seek_down, seek_move) = (
            self.seek_bounds.clone(),
            self.seek_bounds.clone(),
            self.seek_bounds.clone(),
        );
        let seek = div()
            .id("video-seek")
            .relative()
            .flex_1()
            .h(px(16.))
            .flex()
            .items_center()
            .cursor_pointer()
            .child(
                div().w_full().h(px(3.)).rounded(px(1.5)).bg(track).child(
                    div()
                        .h_full()
                        .rounded(px(1.5))
                        .bg(ui.accent)
                        .w(relative(fraction)),
                ),
            )
            .child(
                canvas(
                    move |bounds, _, _| seek_bounds.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if let Some(at) = share_at(seek_down.get(), event.position.x) {
                        this.seeking = true;
                        this.seek(at, cx);
                    }
                }),
            )
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                if this.seeking
                    && event.pressed_button == Some(MouseButton::Left)
                    && let Some(at) = share_at(seek_move.get(), event.position.x)
                {
                    this.seek(at, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.seeking = false),
            );
        let (volume_bounds, volume_down) = (self.volume_bounds.clone(), self.volume_bounds.clone());
        let volume = div()
            .id("video-volume")
            .relative()
            .flex_none()
            .w(px(70.))
            .h(px(16.))
            .flex()
            .items_center()
            .cursor_pointer()
            .tooltip(ui::tooltip(tr("Volume"), None))
            .child(
                div().w_full().h(px(3.)).rounded(px(1.5)).bg(track).child(
                    div()
                        .h_full()
                        .rounded(px(1.5))
                        .bg(white)
                        .w(relative(self.volume)),
                ),
            )
            .child(
                canvas(
                    move |bounds, _, _| volume_bounds.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if let Some(at) = share_at(volume_down.get(), event.position.x) {
                        this.set_volume(at, cx);
                    }
                }),
            );
        let speed = match self.speed.fract() == 0. {
            true => format!("{}×", self.speed as u32),
            false => format!("{:.1}×", self.speed).replace('.', tr(".")),
        };
        let glyph = match playing {
            true => div()
                .flex()
                .gap(px(3.))
                .child(div().w(px(3.)).h(px(12.)).rounded(px(1.)).bg(white))
                .child(div().w(px(3.)).h(px(12.)).rounded(px(1.)).bg(white))
                .into_any_element(),
            false => icon(IconName::Play, white).size(px(14.)).into_any_element(),
        };
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .h(px(BAR_HEIGHT))
            .px(px(14.))
            .flex()
            .items_center()
            .gap(px(12.))
            .bg(gpui::black().opacity(0.55))
            .text_size(px(theme::TEXT_SM))
            .text_color(white)
            .occlude()
            .child(
                div()
                    .id("video-toggle")
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .tooltip(ui::tooltip(
                        if playing { tr("Pause") } else { tr("Play") },
                        None,
                    ))
                    .child(glyph)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle(cx))),
            )
            .child(div().flex_none().whitespace_nowrap().child(format!(
                "{} / {}",
                clock(position),
                clock(duration)
            )))
            .child(seek)
            .child(volume)
            .child(
                div()
                    .id("video-speed")
                    .flex_none()
                    .h(px(18.))
                    .px(px(5.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(white.opacity(0.7))
                    .text_size(px(theme::TEXT_XS))
                    .cursor_pointer()
                    .tooltip(ui::tooltip(tr("Speed"), None))
                    .child(speed)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cycle_speed(cx))),
            )
            .child(
                div()
                    .id("video-full-screen")
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .tooltip(ui::tooltip(tr("Full Screen"), None))
                    .child(icon(IconName::WindowMaximize, white).size(px(14.)))
                    .on_click(|_: &ClickEvent, window, _| window.toggle_fullscreen()),
            )
    }
}

impl Render for VideoView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = self
            .player
            .as_ref()
            .and_then(VideoPlayer::size)
            .map(|(width, height)| (width as f32, height as f32));
        let picture: AnyElement = match &self.frame {
            Some(Shown::Image(image)) => img(image.clone())
                .size_full()
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            #[cfg(target_os = "macos")]
            Some(Shown::Surface(Frame::Surface(buffer))) => gpui::surface(buffer.clone())
                .size_full()
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            Some(Shown::Surface(_)) => div().into_any_element(),
            None => div().into_any_element(),
        };
        let _ = size;
        div()
            .id("video-view")
            .relative()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .id("video-picture")
                    .size_full()
                    .cursor_pointer()
                    .occlude()
                    .child(picture)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle(cx))),
            )
            .children(self.error.clone().map(|error| {
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .px(px(12.))
                            .h(px(30.))
                            .flex()
                            .items_center()
                            .rounded_full()
                            .bg(gpui::black().opacity(0.6))
                            .text_size(px(theme::TEXT_SM))
                            .text_color(gpui::white())
                            .child(error),
                    )
            }))
            .when(self.player.is_some(), |view| {
                view.child(self.render_bar(cx))
            })
    }
}

impl Drop for VideoView {
    fn drop(&mut self) {
        // The player stops and lets its decoder go with it; the picture of a frame stays in the
        // atlases until the window drops it with the popup.
        self.player = None;
    }
}

/// Where `x` falls on a bar: a share of its width.
fn share_at(bounds: Option<Bounds<Pixels>>, x: Pixels) -> Option<f32> {
    let bounds = bounds?;
    let width = f32::from(bounds.size.width);
    (width > 0.).then(|| (f32::from(x - bounds.origin.x) / width).clamp(0., 1.))
}

/// "0:31", "1:24".
fn clock(time: Duration) -> String {
    let seconds = time.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}
