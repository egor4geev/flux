//! The picture popup: Claude's pictures — a thumbnail clicked in the chat or in the message box —
//! over the window, as a photo viewer shows them. The window dims under a veil; the picture fits it
//! with a margin (a small one keeps its size), rounded and lifted by a shadow, and zooms and pans
//! as in the image tab: [`ImageView`] draws it, bare ([`crate::image_view::Place::Popup`]). ← / → and the
//! arrows at the sides go through the pictures of the message (or of the message box), the counter
//! says which one is in sight. The toolbar shows the name and the size, the zoom, Open in Tab (the
//! image tab, what a click did before) and ×; Esc or a click beside the picture closes the popup.
//!
//! It is an overlay window of its own kind ([`crate::workspace::ModalKind::Viewer`]): over the whole
//! window under the title bar, growing in and leaving as a dialog does.
//!
//! The media viewer (FluxGram part 4): a plugin's conversation and gallery open it with their
//! pictures and videos in one row ([`show_media`]). A video plays in its place with sound
//! ([`VideoView`]: space or a click plays and pauses, the seek bar, the volume, the speed, full
//! screen); one not downloaded yet shows its poster with the spinner until its file comes
//! ([`ImagePopup::set_video_path`]). Only the video in sight has a player: going to another item
//! lets its decoder go. A strip of thumbnails under the picture goes to any of them; the toolbar
//! saves the item in sight (Save As…), shows it in Finder (Explorer), opens it in the system's app.
//! The item coming into sight goes out as [`MediaShown`]: a gallery asks its plugin for the full
//! size of a picture the user arrows to (FluxGram part 5).

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    App, ClickEvent, Context, DismissEvent, Div, Entity, EventEmitter, FocusHandle, Focusable,
    KeyBinding, ObjectFit, Render, SharedString, Subscription, Window, actions, div, img,
    prelude::*, px,
};

use crate::i18n::tr;
use crate::icons::{IconName, icon};
use crate::image_view::{
    ActualSize, FitToWindow, ImageSource, ImageView, ImageViewEvent, ZoomIn, ZoomOut,
};
use crate::motion::{self, Appear};
use crate::theme::{self, Theme, UiColors};
use crate::ui;
use crate::video_view::VideoView;
use crate::workspace::Workspace;

pub const CONTEXT: &str = "ImagePopup";

/// How deep the veil over the window is: the picture stands out, as in a photo viewer (a dialog's
/// shade is much lighter).
const VEIL: f32 = 0.6;
/// The toolbar and the arrows keep this far from the edges (inside the picture's margin).
const EDGE: f32 = 12.;
const TOOLBAR_HEIGHT: f32 = 36.;
const ARROW_SIZE: f32 = 36.;
/// The strip of thumbnails at the bottom.
const THUMB_WIDTH: f32 = 56.;
const THUMB_HEIGHT: f32 = 40.;

actions!(
    image_popup,
    [
        /// Esc: closes the popup.
        Dismiss,
        /// ←: the previous picture of the message.
        PreviousPicture,
        /// →: the next one.
        NextPicture,
        /// Space: the video in sight plays or pauses.
        TogglePlay,
    ]
);

/// An item of the media viewer: a picture, or a video.
#[derive(Clone, PartialEq)]
pub enum MediaItem {
    Picture(ImageSource),
    Video(VideoItem),
}

/// A video of the media viewer.
#[derive(Clone, PartialEq)]
pub struct VideoItem {
    /// Its file; none — on the way (the poster and a spinner until then).
    pub path: Option<PathBuf>,
    pub poster: Option<ImageSource>,
    /// In seconds.
    pub duration: u32,
    /// A GIF: plays looped.
    pub gif: bool,
}

/// UI scenarios: the popup with the pictures of `FLUX_SCENARIO_POPUP_IMAGES` (paths joined as in
/// `PATH`), from the one `FLUX_SCENARIO_POPUP_INDEX` says (the first by default), as a chat's
/// pictures come.
#[cfg(feature = "scenario")]
pub mod scenario {
    gpui::actions!(image_popup, [PreviewScenarioImages]);

    pub fn pictures() -> Option<(Vec<super::ImageSource>, usize)> {
        let paths = std::env::var_os("FLUX_SCENARIO_POPUP_IMAGES")?;
        let pictures: Vec<_> = std::env::split_paths(&paths)
            .filter_map(|path| std::fs::read(path).ok())
            .enumerate()
            .map(|(index, data)| super::conversation_picture(index, "image/png", data.into()))
            .collect();
        let index = std::env::var("FLUX_SCENARIO_POPUP_INDEX")
            .ok()
            .and_then(|index| index.parse().ok())
            .unwrap_or(0);
        (!pictures.is_empty()).then_some((pictures, index))
    }
}

pub fn init(cx: &mut App) {
    let context = Some(CONTEXT);
    crate::keymap::bind_keys(
        cx,
        [
            KeyBinding::new("escape", Dismiss, context),
            KeyBinding::new("left", PreviousPicture, context),
            KeyBinding::new("right", NextPicture, context),
            KeyBinding::new("space", TogglePlay, context),
            // The image tab's zoom keys.
            KeyBinding::new("cmd-=", ZoomIn, context),
            KeyBinding::new("cmd-+", ZoomIn, context),
            KeyBinding::new("cmd-shift-=", ZoomIn, context),
            KeyBinding::new("cmd--", ZoomOut, context),
        ],
    );
}

/// A picture of the conversation as the popup and the image tab take it: "image-2.png" for the
/// `index`th of its message (from 0), its media type ("image/png") and its bytes.
pub fn conversation_picture(index: usize, media_type: &str, data: Arc<[u8]>) -> ImageSource {
    let extension = match media_type {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "png",
    };
    ImageSource::Bytes {
        name: format!("image-{}.{extension}", index + 1),
        media_type: media_type.to_string(),
        data,
    }
}

/// Opens the popup over the window with `pictures`, the `index`th in sight.
pub fn open(
    workspace: &mut Workspace,
    pictures: Vec<ImageSource>,
    index: usize,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if pictures.is_empty() {
        return;
    }
    workspace.toggle_viewer(window, cx, |_, cx| ImagePopup::new(pictures, index, cx));
}

/// [`open`] from a view of the window that doesn't hold the workspace (the message box).
pub fn show(pictures: Vec<ImageSource>, index: usize, window: &mut Window, cx: &mut App) {
    let Some(workspace) = window.root::<Workspace>().flatten() else {
        return;
    };
    workspace.update(cx, |workspace, cx| {
        open(workspace, pictures, index, window, cx)
    });
}

/// The media viewer over the window with pictures and videos, the `index`th in sight: a plugin's
/// conversation, a gallery. The popup, to tell it of a video's file when it comes.
pub fn show_media(
    items: Vec<MediaItem>,
    index: usize,
    window: &mut Window,
    cx: &mut App,
) -> Option<Entity<ImagePopup>> {
    let workspace = window.root::<Workspace>().flatten()?;
    if items.is_empty() {
        return None;
    }
    workspace.update(cx, |workspace, cx| {
        workspace.toggle_viewer(window, cx, |_, cx| ImagePopup::with_media(items, index, cx));
        // Over a dialog (a profile's gallery) it is the window above it.
        workspace
            .over_view::<ImagePopup>()
            .or_else(|| workspace.modal_view::<ImagePopup>())
    })
}

/// The veil over the window under the popup; it takes the window's clicks.
pub fn veil(ui: UiColors) -> Div {
    div().occlude().bg(UiColors::tint(ui.shadow, VEIL))
}

pub struct ImagePopup {
    focus_handle: FocusHandle,
    pictures: Vec<ImageSource>,
    /// The pictures' views, made as they (or their neighbors) come into sight: each keeps its zoom.
    views: Vec<Option<Entity<ImageView>>>,
    /// The pictures whose full size is on the way (a plugin's conversation downloads it): a
    /// spinner over them until it comes ([`Self::replace_picture`]).
    loading: Vec<bool>,
    /// The videos among the items (the media viewer): a video's picture is its poster.
    videos: Vec<Option<VideoItem>>,
    /// The player of the video in sight (only its).
    player: Option<(usize, Entity<VideoView>)>,
    index: usize,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for ImagePopup {}

/// The item at this place came into sight: ← / →, the strip.
pub struct MediaShown(pub usize);

impl EventEmitter<MediaShown> for ImagePopup {}

impl Focusable for ImagePopup {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ImagePopup {
    fn new(pictures: Vec<ImageSource>, index: usize, cx: &mut Context<Self>) -> Self {
        let index = index.min(pictures.len().saturating_sub(1));
        let mut popup = Self {
            focus_handle: cx.focus_handle(),
            views: vec![None; pictures.len()],
            loading: vec![false; pictures.len()],
            videos: vec![None; pictures.len()],
            player: None,
            pictures,
            index,
            _subscriptions: Vec::new(),
        };
        popup.show(index, cx);
        popup
    }

    /// The media viewer: pictures and videos.
    fn with_media(items: Vec<MediaItem>, index: usize, cx: &mut Context<Self>) -> Self {
        let (pictures, videos) = items
            .into_iter()
            .enumerate()
            .map(|(at, item)| match item {
                MediaItem::Picture(source) => (source, None),
                MediaItem::Video(video) => (
                    video.poster.clone().unwrap_or_else(|| {
                        conversation_picture(at, "image/png", Arc::from(&[][..]))
                    }),
                    Some(video),
                ),
            })
            .unzip::<_, _, Vec<_>, Vec<_>>();
        let index = index.min(pictures.len().saturating_sub(1));
        let mut popup = Self {
            focus_handle: cx.focus_handle(),
            views: vec![None; pictures.len()],
            loading: vec![false; pictures.len()],
            videos,
            player: None,
            pictures,
            index,
            _subscriptions: Vec::new(),
        };
        popup.show(index, cx);
        popup
    }

    /// The `index`th video's file came: it plays if it is in sight.
    pub fn set_video_path(&mut self, index: usize, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(Some(video)) = self.videos.get_mut(index) {
            video.path = Some(path);
            cx.notify();
        }
    }

    /// The player of the video in sight, made when it comes into sight; another one's goes.
    fn player(&mut self, cx: &mut Context<Self>) -> Option<Entity<VideoView>> {
        let video = self.videos.get(self.index).cloned().flatten();
        let Some((path, gif)) = video.and_then(|video| Some((video.path?, video.gif))) else {
            self.player = None;
            return None;
        };
        if let Some((at, player)) = &self.player
            && *at == self.index
        {
            return Some(player.clone());
        }
        let player = cx.new(|cx| VideoView::new(path, gif, cx));
        self._subscriptions
            .push(cx.observe(&player, |_, _, cx| cx.notify()));
        self.player = Some((self.index, player.clone()));
        Some(player)
    }

    /// The file of the item in sight: a picture's, a video's.
    fn item_path(&self) -> Option<PathBuf> {
        match self.videos.get(self.index) {
            Some(Some(video)) => video.path.clone(),
            _ => match self.pictures.get(self.index)? {
                ImageSource::File(path) => Some(path.clone()),
                ImageSource::Bytes { .. } => None,
            },
        }
    }

    /// Save As… of the item in sight: a copy where the user says.
    fn save_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(source) = self.item_path() else {
            return;
        };
        let name = source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_default();
        let downloads = home.join("Downloads");
        let directory = if downloads.is_dir() { downloads } else { home };
        let target = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn_in(window, async move |_, cx| {
            if let Ok(Ok(Some(target))) = target.await {
                let copied = cx
                    .background_executor()
                    .spawn(async move { std::fs::copy(&source, &target).map(|_| ()) })
                    .await;
                if let Err(error) = copied {
                    log::warn!("couldn't save the viewer's file: {error}");
                }
            }
        })
        .detach();
    }

    /// The buttons for the file in sight: Save As…, Show in Finder (Explorer), Open.
    fn file_buttons(&self, ui: UiColors, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        if self.item_path().is_none() {
            return Vec::new();
        }
        let reveal = if cfg!(target_os = "windows") {
            tr("Show in Explorer")
        } else {
            tr("Show in Finder")
        };
        vec![
            separator(ui).into_any_element(),
            ui::icon_button("image-popup-save", IconName::Download, ui)
                .tooltip(ui::tooltip(tr("Save As…"), None))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save_as(window, cx)))
                .into_any_element(),
            ui::icon_button("image-popup-reveal", IconName::FolderOpen, ui)
                .tooltip(ui::tooltip(reveal, None))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    if let Some(path) = this.item_path() {
                        crate::plugin_conversation::reveal_path(&path, cx);
                    }
                }))
                .into_any_element(),
            ui::icon_button("image-popup-open", IconName::LayoutEditor, ui)
                .tooltip(ui::tooltip(tr("Open"), None))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    if let Some(path) = this.item_path() {
                        crate::plugin_conversation::open_with_system(&path, cx);
                    }
                }))
                .into_any_element(),
        ]
    }

    /// The toolbar over a video: the counter, its name and length, the file's buttons and ×.
    fn render_video_toolbar(
        &self,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let video = self.videos.get(self.index).cloned().flatten();
        let name: SharedString = video
            .as_ref()
            .and_then(|video| video.path.as_ref())
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| tr("Video").to_string())
            .into();
        let length = video.as_ref().map_or(0, |video| video.duration);
        let details = format!("{}:{:02}", length / 60, length % 60);
        let counter = (self.pictures.len() > 1)
            .then(|| format!("{} / {}", self.index + 1, self.pictures.len()));
        let toolbar = crate::popup::panel(ui)
            .id("image-popup-toolbar")
            .occlude()
            .min_w_0()
            .h(px(TOOLBAR_HEIGHT))
            .pl(px(12.))
            .pr(px(5.))
            .flex()
            .items_center()
            .gap_1()
            .text_size(px(theme::TEXT_SM))
            .children(counter.map(|counter| {
                div()
                    .flex_none()
                    .mr_1()
                    .whitespace_nowrap()
                    .text_color(ui.text_muted)
                    .child(counter)
            }))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(ui.foreground)
                    .child(name),
            )
            .child(
                div()
                    .min_w_0()
                    .ml_1()
                    .truncate()
                    .text_color(ui.dim)
                    .child(details),
            )
            .children(self.file_buttons(ui, cx))
            .child(separator(ui))
            .child(
                ui::icon_button("image-popup-close", IconName::Close, ui)
                    .tooltip(ui::tooltip(tr("Close"), ui::shortcut_for(&Dismiss, cx)))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
            );
        div()
            .absolute()
            .top(px(EDGE))
            .left(px(EDGE))
            .right(px(EDGE))
            .flex()
            .justify_center()
            .child(toolbar)
    }

    /// The strip of thumbnails at the bottom (the media viewer): a click goes to one.
    fn render_strip(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let scale = window.scale_factor();
        let device = |side: f32| (side * scale).ceil() as u32;
        let view = cx.entity_id();
        let first = self.index.saturating_sub(6);
        let last = (self.index + 7).min(self.pictures.len());
        let thumbs: Vec<gpui::AnyElement> = (first..last)
            .map(|at| {
                let picture = match &self.pictures[at] {
                    ImageSource::File(path) => crate::plugin_images::file(
                        path,
                        device(THUMB_WIDTH),
                        device(THUMB_HEIGHT),
                        view,
                        cx,
                    ),
                    ImageSource::Bytes { .. } => None,
                };
                let video = self.videos.get(at).cloned().flatten();
                let label = video.map(|video| match video.gif {
                    true => tr("GIF").to_string(),
                    false => format!("{}:{:02}", video.duration / 60, video.duration % 60),
                });
                let current = at == self.index;
                div()
                    .id(("image-popup-thumb", at))
                    .relative()
                    .flex_none()
                    .w(px(THUMB_WIDTH))
                    .h(px(THUMB_HEIGHT))
                    .rounded(px(ui::RADIUS_SM))
                    .overflow_hidden()
                    .bg(UiColors::tint(ui.foreground, 0.08))
                    .border_2()
                    .border_color(if current {
                        ui.accent
                    } else {
                        gpui::transparent_black()
                    })
                    .opacity(if current { 1. } else { 0.7 })
                    .cursor_pointer()
                    .hover(|style| style.opacity(1.))
                    .children(
                        picture.map(|image| img(image).size_full().object_fit(ObjectFit::Cover)),
                    )
                    .children(label.map(|label| {
                        div()
                            .absolute()
                            .right(px(2.))
                            .bottom(px(2.))
                            .px(px(3.))
                            .rounded(px(3.))
                            .bg(gpui::black().opacity(0.55))
                            .text_size(px(9.))
                            .text_color(gpui::white())
                            .child(label)
                    }))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.show(at, cx)))
                    .into_any_element()
            })
            .collect();
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(EDGE))
            .flex()
            .justify_center()
            .child(
                div()
                    .id("image-popup-strip")
                    .occlude()
                    .flex()
                    .gap(px(6.))
                    .p(px(4.))
                    .rounded(px(ui::RADIUS_MD))
                    .children(thumbs),
            )
    }

    /// Brings the `index`th picture into sight; its neighbors start loading, so ← / → show them
    /// at once.
    fn show(&mut self, index: usize, cx: &mut Context<Self>) {
        self.index = index;
        for index in index.saturating_sub(1)..=index + 1 {
            self.view(index, cx);
        }
        cx.emit(MediaShown(index));
        cx.notify();
    }

    /// The view of the `index`th picture, made the first time it is asked for.
    fn view(&mut self, index: usize, cx: &mut Context<Self>) -> Option<Entity<ImageView>> {
        if let Some(Some(video)) = self.videos.get(index)
            && video.poster.is_none()
        {
            return None;
        }
        let source = self.pictures.get(index)?.clone();
        if let Some(view) = &self.views[index] {
            return Some(view.clone());
        }
        let view = cx.new(|cx| ImageView::popup(source, cx));
        self._subscriptions.extend([
            cx.observe(&view, |_, _, cx| cx.notify()),
            cx.subscribe(&view, |_, _, event: &ImageViewEvent, cx| {
                if *event == ImageViewEvent::Dismiss {
                    cx.emit(DismissEvent);
                }
            }),
        ]);
        self.views[index] = Some(view.clone());
        Some(view)
    }

    /// The `index`th picture's full size is on the way (a spinner over it), or came.
    pub fn set_loading(&mut self, index: usize, loading: bool, cx: &mut Context<Self>) {
        if let Some(slot) = self.loading.get_mut(index) {
            *slot = loading;
            cx.notify();
        }
    }

    /// The `index`th picture is now `source`: its full size came (a plugin's conversation).
    pub fn replace_picture(&mut self, index: usize, source: ImageSource, cx: &mut Context<Self>) {
        if index >= self.pictures.len() || self.pictures[index] == source {
            return;
        }
        self.pictures[index] = source;
        self.views[index] = None;
        if index.abs_diff(self.index) <= 1 {
            self.view(index, cx);
        }
        cx.notify();
    }

    /// The picture `step` away from the one in sight, if there is one.
    fn neighbor(&self, step: isize) -> Option<usize> {
        self.index
            .checked_add_signed(step)
            .filter(|index| *index < self.pictures.len())
    }

    fn go(&mut self, step: isize, cx: &mut Context<Self>) {
        if let Some(index) = self.neighbor(step) {
            self.show(index, cx);
        }
    }

    /// The picture in sight in an image tab (what a thumbnail's click did before the popup).
    fn open_in_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(source) = self.pictures.get(self.index).cloned() else {
            return;
        };
        let Some(workspace) = window.root::<Workspace>().flatten() else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            workspace.dismiss_modal(window, cx);
            workspace.open_image(source, true, window, cx);
        });
    }

    /// The view in sight takes a zoom key.
    fn zoom(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        act: impl FnOnce(&mut ImageView, &mut Window, &mut Context<ImageView>),
    ) {
        if let Some(view) = self.view(self.index, cx) {
            view.update(cx, |view, cx| act(view, window, cx));
        }
    }

    /// The toolbar at the top: the counter, the name and the size, the zoom, Open in Tab and ×.
    fn render_toolbar(
        &self,
        view: &Entity<ImageView>,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let image = view.read(cx);
        let name = image.title();
        let details = image.details();
        let zoom = image.zoom_controls(view, image.target_scale(), ui, cx);
        let counter = (self.pictures.len() > 1)
            .then(|| format!("{} / {}", self.index + 1, self.pictures.len()));
        let toolbar = crate::popup::panel(ui)
            .id("image-popup-toolbar")
            .occlude()
            .min_w_0()
            .h(px(TOOLBAR_HEIGHT))
            .pl(px(12.))
            .pr(px(5.))
            .flex()
            .items_center()
            .gap_1()
            .text_size(px(theme::TEXT_SM))
            .children(counter.map(|counter| {
                div()
                    .flex_none()
                    .mr_1()
                    .whitespace_nowrap()
                    .text_color(ui.text_muted)
                    .child(counter)
            }))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(ui.foreground)
                    .child(name),
            )
            .child(
                div()
                    .min_w_0()
                    .ml_1()
                    .truncate()
                    .text_color(ui.dim)
                    .child(details),
            )
            .child(separator(ui))
            .children(zoom)
            .child(separator(ui))
            .child(
                ui::text_button("image-popup-open-in-tab", tr("Open in Tab"), false, ui)
                    .on_click(cx.listener(|this, _, window, cx| this.open_in_tab(window, cx))),
            )
            .children(self.file_buttons(ui, cx))
            .child(
                ui::icon_button("image-popup-close", IconName::Close, ui)
                    .tooltip(ui::tooltip(tr("Close"), ui::shortcut_for(&Dismiss, cx)))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
            );
        // The row spans the window but takes no clicks: beside the toolbar, a click still closes.
        div()
            .absolute()
            .top(px(EDGE))
            .left(px(EDGE))
            .right(px(EDGE))
            .flex()
            .justify_center()
            .child(toolbar)
    }

    /// The arrow at a side: the previous picture (`step` −1, on the left) or the next one; at the
    /// end of the pictures it is dimmed.
    fn render_arrow(
        &self,
        step: isize,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let available = self.neighbor(step).is_some();
        let (id, label, keys) = if step < 0 {
            (
                "image-popup-previous",
                tr("Previous Picture"),
                ui::shortcut_for(&PreviousPicture, cx),
            )
        } else {
            (
                "image-popup-next",
                tr("Next Picture"),
                ui::shortcut_for(&NextPicture, cx),
            )
        };
        let chevron = icon(IconName::ChevronRight, ui.foreground).size(px(16.));
        let chevron = if step < 0 {
            chevron.with_transformation(gpui::Transformation::rotate(gpui::radians(
                std::f32::consts::PI,
            )))
        } else {
            chevron
        };
        // The surface stays opaque under the mouse: the veil is behind it.
        let hover = motion::mix(ui.elevated, UiColors::tint(ui.hover, 1.), ui.hover.a);
        let button = div()
            .id(id)
            .occlude()
            .size(px(ARROW_SIZE))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(ui.elevated)
            .border_1()
            .border_color(ui.elevated_border)
            .shadow(ui::popover_shadow(ui))
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .active(motion::press)
            .tooltip(ui::tooltip(label, keys))
            .child(chevron)
            .when(available, |button| {
                button.on_click(cx.listener(move |this, _, _, cx| this.go(step, cx)))
            });
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .when(step < 0, |side| side.left(px(EDGE)))
            .when(step > 0, |side| side.right(px(EDGE)))
            .flex()
            .items_center()
            .child(ui::available(button, available))
    }
}

/// Over a picture whose full size is on the way: a spinner and "Loading…" in a pill in the
/// middle, white on dark glass in both themes as over a picture in the chat. It takes no clicks.
fn loading_pill(index: usize) -> Div {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(30.))
                .px(px(12.))
                .rounded_full()
                .bg(gpui::black().opacity(0.6))
                .text_size(px(theme::TEXT_SM))
                .text_color(gpui::white())
                .child(ui::pulse_dot(
                    ("image-popup-loading", index),
                    8.,
                    gpui::white(),
                ))
                .child(tr("Loading…")),
        )
}

/// A thin line between the toolbar's groups.
fn separator(ui: UiColors) -> Div {
    div().flex_none().w(px(1.)).h(px(16.)).mx_1().bg(ui.divider)
}

impl Render for ImagePopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let player = self.player(cx);
        let view = match player {
            Some(_) => None,
            None => self.view(self.index, cx),
        };
        let video = self.videos.get(self.index).is_some_and(Option::is_some);
        let media = self.videos.iter().any(Option::is_some);
        let several = self.pictures.len() > 1;
        div()
            .id("image-popup")
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .on_action(cx.listener(|this, _: &PreviousPicture, _, cx| this.go(-1, cx)))
            .on_action(cx.listener(|this, _: &NextPicture, _, cx| this.go(1, cx)))
            .on_action(cx.listener(|this, _: &TogglePlay, _, cx| {
                if let Some((_, player)) = &this.player {
                    player.update(cx, |player, cx| player.toggle(cx));
                }
            }))
            .on_action(cx.listener(|this, action: &ZoomIn, window, cx| {
                this.zoom(window, cx, |view, window, cx| {
                    view.zoom_in(action, window, cx)
                })
            }))
            .on_action(cx.listener(|this, action: &ZoomOut, window, cx| {
                this.zoom(window, cx, |view, window, cx| {
                    view.zoom_out(action, window, cx)
                })
            }))
            .on_action(cx.listener(|this, action: &ActualSize, window, cx| {
                this.zoom(window, cx, |view, window, cx| {
                    view.actual_size(action, window, cx)
                })
            }))
            .on_action(cx.listener(|this, action: &FitToWindow, window, cx| {
                this.zoom(window, cx, |view, window, cx| {
                    view.fit_to_window(action, window, cx)
                })
            }))
            .relative()
            .size_full()
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_MD))
            .text_color(ui.foreground)
            // Another picture fades in in its place.
            .children(view.clone().map(|view| {
                Appear::fade().apply(
                    ("image-popup-picture", self.index),
                    div().absolute().size_full().child(view),
                )
            }))
            // A video plays in a box of the window's middle, as a picture fits it.
            .children(player.map(|player| {
                div()
                    .absolute()
                    .inset_0()
                    .px(px(EDGE + ARROW_SIZE + 16.))
                    .pt(px(EDGE + TOOLBAR_HEIGHT + 16.))
                    .pb(px(EDGE + THUMB_HEIGHT + 24.))
                    .child(
                        div()
                            .size_full()
                            .rounded(px(ui::RADIUS_MD))
                            .overflow_hidden()
                            .bg(gpui::black())
                            .child(player),
                    )
            }))
            .when(self.loading.get(self.index) == Some(&true), |popup| {
                popup.child(loading_pill(self.index))
            })
            .children(
                view.filter(|_| !video)
                    .map(|view| self.render_toolbar(&view, ui, cx)),
            )
            .when(video, |popup| {
                popup.child(self.render_video_toolbar(ui, cx))
            })
            .when(media && several, |popup| {
                popup.child(self.render_strip(window, cx))
            })
            .when(several, |popup| {
                popup
                    .child(self.render_arrow(-1, ui, cx))
                    .child(self.render_arrow(1, ui, cx))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_pictures_are_named_by_their_place_and_type() {
        let data: Arc<[u8]> = Arc::from(&[1u8, 2, 3][..]);
        let ImageSource::Bytes {
            name, media_type, ..
        } = conversation_picture(1, "image/jpeg", data.clone())
        else {
            panic!("bytes");
        };
        assert_eq!(
            (name.as_str(), media_type.as_str()),
            ("image-2.jpg", "image/jpeg")
        );
        let ImageSource::Bytes { name, .. } = conversation_picture(0, "image/png", data) else {
            panic!("bytes");
        };
        assert_eq!(name, "image-1.png");
    }
}
