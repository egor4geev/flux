//! Pictures in an editor tab (part 10.2): image files — PNG, JPEG, GIF, WebP, BMP, ICO, TIFF, SVG —
//! and Claude's pictures from the chat (a thumbnail clicked). The picture fits the tab at first (a
//! small one keeps its size), ⌘= / ⌘− and ⌘ with the wheel zoom, 1:1 shows it pixel for point, a
//! checkerboard shows through transparency, and a large one is looked around by scrolling or
//! dragging. Raster pictures are decoded once in the background; an SVG is drawn by resvg at the
//! size it is shown, so it stays sharp at any zoom. An SVG file is text as well: Text / Preview in
//! the toolbar switch between this tab and the file's editor tab (JetBrains IDEs have the pair).

use std::cell::Cell;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, LazyLock};
use std::time::SystemTime;

use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, Hsla, KeyBinding, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, Pixels, Point, RenderImage,
    ScrollHandle, ScrollWheelEvent, SharedString, Size, Task, Window, actions, canvas, div, fill,
    img, point, prelude::*, px, size,
};
use resvg::{tiny_skia, usvg};

use crate::i18n::{tr, trf};
use crate::icons::IconName;
use crate::theme::{self, Theme, UiColors};
use crate::ui;

pub const CONTEXT: &str = "ImageView";

/// Zoom steps of ⌘= / ⌘−, as image viewers have them.
const STEPS: &[f32] = &[
    0.05, 0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 1., 1.25, 1.5, 2., 3., 4., 6., 8., 12., 16., 24., 32.,
];
const MIN_ZOOM: f32 = 0.02;
const MAX_ZOOM: f32 = 32.;
/// Room around the picture inside the tab.
const PADDING: f32 = 16.;
/// A side of a checkerboard square.
const CELL: f32 = 8.;
/// The largest side of an SVG drawn for the screen: beyond it the drawing is stretched.
const MAX_RASTER: f32 = 8192.;

actions!(
    image_view,
    [
        /// The picture larger (the next zoom step).
        ZoomIn,
        /// The picture smaller.
        ZoomOut,
        /// One pixel of the picture to one point of the screen.
        ActualSize,
        /// The whole picture in the tab (never larger than it is).
        FitToWindow,
        /// An SVG file's picture: its text, in the file's editor tab.
        ShowText,
        /// An SVG file's text: its picture (the window handles it while an SVG's text is active).
        ShowPreview,
    ]
);

/// UI scenarios can't click a chat's thumbnail: this opens the picture of
/// `FLUX_SCENARIO_PREVIEW_IMAGE` as a chat's picture would come (bytes and a name).
#[cfg(feature = "scenario")]
pub mod scenario {
    gpui::actions!(image_view, [PreviewScenarioImage]);
}

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-=", ZoomIn, Some(CONTEXT)),
        KeyBinding::new("cmd-+", ZoomIn, Some(CONTEXT)),
        KeyBinding::new("cmd-shift-=", ZoomIn, Some(CONTEXT)),
        KeyBinding::new("cmd--", ZoomOut, Some(CONTEXT)),
    ]);
}

/// What an image tab shows.
#[derive(Debug, Clone, PartialEq)]
pub enum ImageSource {
    /// A file on disk.
    File(PathBuf),
    /// A picture of the conversation (pasted, attached, returned by a tool): a name for the tab,
    /// its media type ("image/png") and its bytes.
    Bytes {
        name: String,
        media_type: String,
        data: Arc<[u8]>,
    },
}

/// A file Flux shows as a picture rather than as text (by its extension).
pub fn is_image(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    matches!(
        extension.as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff" | "svg")
    )
}

/// An SVG file: a picture with a text of its own.
pub fn is_svg(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"))
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImageViewEvent {
    /// Text in the toolbar of an SVG file: the file in its editor tab.
    EditAsText(PathBuf),
}

/// How large the picture is shown.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Zoom {
    /// The whole picture in the tab, never larger than 100 %.
    Fit,
    Scale(f32),
}

/// A decoded picture.
enum Picture {
    Raster {
        image: Arc<RenderImage>,
        width: u32,
        height: u32,
    },
    Svg {
        tree: Arc<usvg::Tree>,
        /// The drawing at the scale it was drawn for, in device pixels per SVG unit.
        raster: Option<(f32, Arc<RenderImage>)>,
        /// A drawing at another scale on its way.
        drawing: Option<(f32, Task<()>)>,
    },
}

enum Load {
    Loading,
    Ready(Picture),
    Failed(String),
}

/// What reading and decoding gave.
struct Loaded {
    picture: Result<Decoded, String>,
    bytes: u64,
    format: &'static str,
    modified: Option<SystemTime>,
}

enum Decoded {
    Raster(Arc<RenderImage>, u32, u32),
    Svg(Arc<usvg::Tree>),
}

/// A drag that moves a large picture: where it started and the scroll it had then.
struct Pan {
    start: Point<Pixels>,
    offset: Point<Pixels>,
}

pub struct ImageView {
    source: ImageSource,
    title: SharedString,
    focus_handle: FocusHandle,
    scroll: ScrollHandle,
    load: Load,
    zoom: Zoom,
    /// The size of the area the picture is shown in, as last drawn.
    viewport: Rc<Cell<Size<Pixels>>>,
    pan: Option<Pan>,
    /// The size of the file (or the bytes), "PNG"… — for the toolbar.
    bytes: u64,
    format: &'static str,
    /// When the file was last written, as read: a change on disk reads it again.
    modified: Option<SystemTime>,
    /// The text of an SVG being edited in its tab, shown instead of the file's (live preview).
    text: Option<String>,
    _loading: Option<Task<()>>,
}

impl EventEmitter<ImageViewEvent> for ImageView {}

impl ImageView {
    pub fn new(source: ImageSource, cx: &mut Context<Self>) -> Self {
        let title = match &source {
            ImageSource::File(path) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            ImageSource::Bytes { name, .. } => name.clone(),
        };
        let mut view = Self {
            source,
            title: title.into(),
            focus_handle: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            load: Load::Loading,
            zoom: Zoom::Fit,
            viewport: Rc::new(Cell::new(Size::default())),
            pan: None,
            bytes: 0,
            format: "",
            modified: None,
            text: None,
            _loading: None,
        };
        view.load(cx);
        view
    }

    pub fn source(&self) -> &ImageSource {
        &self.source
    }

    pub fn path(&self) -> Option<&Path> {
        match &self.source {
            ImageSource::File(path) => Some(path),
            ImageSource::Bytes { .. } => None,
        }
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    /// The file was renamed or moved: the tab follows it.
    pub fn set_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(name) = path.file_name() {
            self.title = name.to_string_lossy().into_owned().into();
        }
        self.source = ImageSource::File(path);
        cx.notify();
    }

    /// The file may have changed on disk: read again when it was written since.
    pub fn reload_if_changed(&mut self, cx: &mut Context<Self>) {
        let ImageSource::File(path) = &self.source else {
            return;
        };
        if self.text.is_some() {
            return;
        }
        let path = path.clone();
        let known = self.modified;
        cx.spawn(async move |this, cx| {
            let modified = cx
                .background_spawn(
                    async move { std::fs::metadata(&path).and_then(|m| m.modified()) },
                )
                .await
                .ok();
            if modified.is_some() && modified != known {
                this.update(cx, |this, cx| this.load(cx)).ok();
            }
        })
        .detach();
    }

    /// An SVG's text from its editor (unsaved edits): the preview shows it; `None` — the file.
    pub fn set_text(&mut self, text: Option<String>, cx: &mut Context<Self>) {
        if self.text == text {
            return;
        }
        let back_to_file = text.is_none() && self.text.is_some();
        self.text = text;
        if self.text.is_some() || back_to_file {
            self.load(cx);
        }
    }

    /// Reads and decodes the picture in the background; what was shown stays until it is ready.
    fn load(&mut self, cx: &mut Context<Self>) {
        let source = self.source.clone();
        let text = self.text.clone();
        let read = cx.background_spawn(async move { read(&source, text) });
        self._loading = Some(cx.spawn(async move |this, cx| {
            let loaded = read.await;
            this.update(cx, |this, cx| this.loaded(loaded, cx)).ok();
        }));
    }

    fn loaded(&mut self, loaded: Loaded, cx: &mut Context<Self>) {
        self.bytes = loaded.bytes;
        self.format = loaded.format;
        if loaded.modified.is_some() {
            self.modified = loaded.modified;
        }
        self.load = match loaded.picture {
            Ok(Decoded::Raster(image, width, height)) => Load::Ready(Picture::Raster {
                image,
                width,
                height,
            }),
            Ok(Decoded::Svg(tree)) => Load::Ready(Picture::Svg {
                tree,
                raster: None,
                drawing: None,
            }),
            Err(error) => Load::Failed(error),
        };
        cx.notify();
    }

    /// The picture's own size: pixels, or the SVG's units.
    fn natural_size(&self) -> Option<(f32, f32)> {
        match &self.load {
            Load::Ready(Picture::Raster { width, height, .. }) => {
                Some((*width as f32, *height as f32))
            }
            Load::Ready(Picture::Svg { tree, .. }) => {
                let size = tree.size();
                Some((size.width(), size.height()))
            }
            _ => None,
        }
    }

    /// The scale the picture is shown at in a viewport this large.
    fn scale(&self, viewport: Size<Pixels>) -> f32 {
        match self.zoom {
            Zoom::Scale(scale) => scale,
            Zoom::Fit => {
                let Some((width, height)) = self.natural_size() else {
                    return 1.;
                };
                let room_w = (f32::from(viewport.width) - 2. * PADDING).max(1.);
                let room_h = (f32::from(viewport.height) - 2. * PADDING).max(1.);
                (room_w / width.max(1.))
                    .min(room_h / height.max(1.))
                    .min(1.)
            }
        }
    }

    fn set_scale(&mut self, scale: f32, cx: &mut Context<Self>) {
        let viewport = self.viewport.get();
        let old = self.scale(viewport);
        let scale = scale.clamp(MIN_ZOOM, MAX_ZOOM);
        // The point in the middle of the view stays in the middle.
        let offset = self.scroll.offset();
        let half = point(viewport.width / 2., viewport.height / 2.);
        let middle = point(
            (f32::from(half.x) - f32::from(offset.x)) / old,
            (f32::from(half.y) - f32::from(offset.y)) / old,
        );
        self.zoom = Zoom::Scale(scale);
        self.scroll.set_offset(point(
            px(f32::from(half.x) - middle.x * scale).min(px(0.)),
            px(f32::from(half.y) - middle.y * scale).min(px(0.)),
        ));
        cx.notify();
    }

    fn zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        let scale = self.scale(self.viewport.get());
        let next = STEPS
            .iter()
            .copied()
            .find(|step| *step > scale * 1.001)
            .unwrap_or(MAX_ZOOM);
        self.set_scale(next, cx);
    }

    fn zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        let scale = self.scale(self.viewport.get());
        let next = STEPS
            .iter()
            .rev()
            .copied()
            .find(|step| *step < scale * 0.999)
            .unwrap_or(MIN_ZOOM);
        self.set_scale(next, cx);
    }

    fn actual_size(&mut self, _: &ActualSize, _: &mut Window, cx: &mut Context<Self>) {
        self.set_scale(1., cx);
    }

    fn fit_to_window(&mut self, _: &FitToWindow, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom = Zoom::Fit;
        self.scroll.set_offset(Point::default());
        cx.notify();
    }

    /// ⌘ with the wheel or the trackpad zooms; otherwise the wheel scrolls.
    fn on_wheel(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !event.modifiers.platform {
            return;
        }
        let delta = event.delta.pixel_delta(px(20.));
        let step = f32::from(delta.y).clamp(-60., 60.);
        if step == 0. {
            return;
        }
        let scale = self.scale(self.viewport.get());
        self.set_scale(scale * (1. + step / 200.), cx);
        cx.stop_propagation();
        window.refresh();
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.pan = Some(Pan {
            start: event.position,
            offset: self.scroll.offset(),
        });
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(pan) = &self.pan else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.pan = None;
            return cx.notify();
        }
        let max = self.scroll.max_offset();
        let offset = pan.offset + (event.position - pan.start);
        self.scroll.set_offset(point(
            offset.x.clamp(-max.width, px(0.)),
            offset.y.clamp(-max.height, px(0.)),
        ));
        cx.notify();
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.pan = None;
        cx.notify();
    }

    /// The SVG drawn for `scale` device pixels per unit: the drawing in hand while a sharper one
    /// is being drawn in the background.
    fn svg_image(&mut self, scale: f32, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        let Load::Ready(Picture::Svg {
            tree,
            raster,
            drawing,
        }) = &mut self.load
        else {
            return None;
        };
        let current = raster.as_ref().map(|(scale, _)| *scale);
        let close = |a: f32, b: f32| (a / b - 1.).abs() < 0.02;
        let wanted_differs = current.is_none_or(|current| !close(current, scale));
        let already_drawing = drawing.as_ref().is_some_and(|(at, _)| close(*at, scale));
        if wanted_differs && !already_drawing {
            let tree = tree.clone();
            let task = cx.spawn(async move |this, cx| {
                let image = cx
                    .background_spawn(async move { draw_svg(&tree, scale) })
                    .await;
                this.update(cx, |this, cx| {
                    if let Load::Ready(Picture::Svg {
                        raster, drawing, ..
                    }) = &mut this.load
                    {
                        if drawing.as_ref().is_some_and(|(at, _)| *at == scale) {
                            *drawing = None;
                        }
                        if let Some(image) = image {
                            *raster = Some((scale, image));
                        }
                    }
                    cx.notify();
                })
                .ok();
            });
            *drawing = Some((scale, task));
        }
        raster.as_ref().map(|(_, image)| image.clone())
    }

    fn show_text(&mut self, _: &ShowText, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.path().filter(|path| is_svg(path)) {
            cx.emit(ImageViewEvent::EditAsText(path.to_path_buf()));
        }
    }

    fn render_toolbar(&self, scale: f32, ui: UiColors, cx: &mut Context<Self>) -> impl IntoElement {
        let fit = self.zoom == Zoom::Fit;
        let percent = format!("{}%", (scale * 100.).round());
        let mut details: Vec<String> = Vec::new();
        if let Some((width, height)) = self.natural_size() {
            details.push(trf(
                "{0} × {1} px",
                &[
                    &format!("{}", width.round()),
                    &format!("{}", height.round()),
                ],
            ));
        }
        if self.bytes > 0 {
            details.push(format_size(self.bytes));
        }
        if !self.format.is_empty() {
            details.push(self.format.to_string());
        }
        let svg_file = self
            .path()
            .filter(|path| is_svg(path))
            .map(Path::to_path_buf);
        div()
            .flex_none()
            .h(px(34.))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .border_b_1()
            .border_color(ui.divider)
            .text_size(px(theme::TEXT_SM))
            .child(
                ui::toggle_button("image-fit", IconName::Fit, fit, ui)
                    .tooltip(ui::tooltip(tr("Fit to Window"), None))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.fit_to_window(&FitToWindow, window, cx)
                    })),
            )
            .child(
                div()
                    .id("image-actual-size")
                    .flex_none()
                    .h(px(ui::ICON_BUTTON_SIZE))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded(px(ui::RADIUS_SM))
                    .text_color(if self.zoom == Zoom::Scale(1.) {
                        ui.accent_text
                    } else {
                        ui.text_muted
                    })
                    .when(self.zoom == Zoom::Scale(1.), |button| {
                        button.bg(ui.accent_soft)
                    })
                    .when(self.zoom != Zoom::Scale(1.), |button| {
                        button.hover(move |style| style.bg(ui.hover))
                    })
                    .cursor_pointer()
                    .child("1:1")
                    .tooltip(ui::tooltip(tr("Actual Size"), None))
                    .on_click(
                        cx.listener(|this, _, window, cx| {
                            this.actual_size(&ActualSize, window, cx)
                        }),
                    ),
            )
            .child(div().w(px(1.)).h(px(16.)).mx_1().bg(ui.divider))
            .child(
                ui::icon_button("image-zoom-out", IconName::Minus, ui)
                    .tooltip(ui::tooltip(tr("Zoom Out"), ui::shortcut_for(&ZoomOut, cx)))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.zoom_out(&ZoomOut, window, cx)),
                    ),
            )
            .child(
                div()
                    .min_w(px(44.))
                    .flex()
                    .justify_center()
                    .text_color(ui.text_muted)
                    .child(percent),
            )
            .child(
                ui::icon_button("image-zoom-in", IconName::Plus, ui)
                    .tooltip(ui::tooltip(tr("Zoom In"), ui::shortcut_for(&ZoomIn, cx)))
                    .on_click(cx.listener(|this, _, window, cx| this.zoom_in(&ZoomIn, window, cx))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_color(ui.dim)
                    .whitespace_nowrap()
                    .child(details.join(" · ")),
            )
            .when_some(svg_file, |bar, path| {
                bar.child(div().w_2()).child(svg_switch(
                    "image-svg",
                    false,
                    ui,
                    cx.listener(move |_, _, _, cx| {
                        cx.emit(ImageViewEvent::EditAsText(path.clone()))
                    }),
                    |_, _, _| {},
                ))
            })
    }
}

/// Text | Preview — an SVG file's two views; `text` — which one is in sight. The SVG editor's bar
/// in the workspace shows the same pair.
pub fn svg_switch(
    id: &'static str,
    text: bool,
    ui: UiColors,
    on_text: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    on_preview: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let segment = move |label: SharedString, on: bool| {
        div()
            .h(px(22.))
            .px_2()
            .flex()
            .items_center()
            .rounded(px(ui::RADIUS_XS))
            .text_size(px(theme::TEXT_SM))
            .when(on, |segment| {
                segment.bg(ui.pressed).text_color(ui.foreground)
            })
            .when(!on, |segment| {
                segment
                    .text_color(ui.text_muted)
                    .cursor_pointer()
                    .hover(move |style| style.text_color(ui.foreground))
            })
            .child(label)
    };
    div()
        .id(id)
        .flex_none()
        .p(px(2.))
        .flex()
        .items_center()
        .gap(px(2.))
        .rounded(px(ui::RADIUS_SM))
        .border_1()
        .border_color(ui.input_border)
        .child(
            segment(tr("Text").into(), text)
                .id(SharedString::from(format!("{id}-text")))
                .on_click(on_text),
        )
        .child(
            segment(tr("Preview").into(), !text)
                .id(SharedString::from(format!("{id}-preview")))
                .on_click(on_preview),
        )
}

/// "12 KB", "1.4 MB", "830 B".
fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.;
    let value = bytes as f64;
    if value < KB {
        trf("{0} B", &[&bytes.to_string()])
    } else if value < KB * KB {
        trf("{0} KB", &[&format!("{:.0}", value / KB)])
    } else {
        trf("{0} MB", &[&format!("{:.1}", value / KB / KB)])
    }
}

impl Focusable for ImageView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ImageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let viewport = self.viewport.get();
        let scale = self.scale(viewport);
        let body = match &self.load {
            Load::Loading => message(tr("Loading…").to_string(), ui).into_any_element(),
            Load::Failed(error) => {
                message(format!("{}\n{error}", tr("Couldn't show the picture")), ui)
                    .into_any_element()
            }
            Load::Ready(_) => {
                let (width, height) = self.natural_size().unwrap_or((1., 1.));
                let shown = size(px(width * scale), px(height * scale));
                let image = match &self.load {
                    Load::Ready(Picture::Raster { image, .. }) => Some(image.clone()),
                    _ => {
                        let device = (scale * window.scale_factor())
                            .min(MAX_RASTER / width.max(height).max(1.));
                        self.svg_image(device, cx)
                    }
                };
                let content = size(
                    shown.width.max(viewport.width - px(2. * PADDING)) + px(2. * PADDING),
                    shown.height.max(viewport.height - px(2. * PADDING)) + px(2. * PADDING),
                );
                let large = f32::from(shown.width) + 2. * PADDING > f32::from(viewport.width)
                    || f32::from(shown.height) + 2. * PADDING > f32::from(viewport.height);
                let picture = div()
                    .relative()
                    .flex_none()
                    .w(shown.width)
                    .h(shown.height)
                    .child(checkerboard(ui).absolute().size_full())
                    .children(image.map(|image| {
                        img(gpui::ImageSource::Render(image))
                            .absolute()
                            .size_full()
                            .object_fit(ObjectFit::Fill)
                    }));
                div()
                    .id("image-scroll")
                    .absolute()
                    .size_full()
                    .overflow_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        div()
                            .id("image-content")
                            .w(content.width)
                            .h(content.height)
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(large, |content| {
                                if self.pan.is_some() {
                                    content.cursor_grabbing()
                                } else {
                                    content.cursor_grab()
                                }
                            })
                            .on_scroll_wheel(cx.listener(Self::on_wheel))
                            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                            .on_mouse_move(cx.listener(Self::on_mouse_move))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                            .child(picture),
                    )
                    .into_any_element()
            }
        };
        let measured = self.viewport.clone();
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::actual_size))
            .on_action(cx.listener(Self::fit_to_window))
            .when(self.path().is_some_and(is_svg), |view| {
                view.on_action(cx.listener(Self::show_text))
            })
            .size_full()
            .flex()
            .flex_col()
            .text_color(ui.foreground)
            .child(self.render_toolbar(scale, ui, cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        canvas(
                            move |bounds, window, _| {
                                if measured.get() != bounds.size {
                                    measured.set(bounds.size);
                                    window.refresh();
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(body),
            )
    }
}

/// A line in the middle of the tab (loading, an error).
fn message(text: String, ui: UiColors) -> impl IntoElement {
    div()
        .absolute()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(theme::TEXT_SM))
        .text_color(ui.dim)
        .child(text)
}

/// Squares behind a picture with transparency; only those in sight are painted.
fn checkerboard(ui: UiColors) -> gpui::Canvas<()> {
    let light: Hsla = UiColors::tint(ui.foreground, 0.10);
    let dark: Hsla = UiColors::tint(ui.foreground, 0.04);
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let visible = bounds.intersect(&window.content_mask().bounds);
            if visible.size.width <= px(0.) || visible.size.height <= px(0.) {
                return;
            }
            window.paint_quad(fill(visible, dark));
            let first_column = ((visible.left() - bounds.left()) / px(CELL)).floor() as i64;
            let last_column = ((visible.right() - bounds.left()) / px(CELL)).ceil() as i64;
            let first_row = ((visible.top() - bounds.top()) / px(CELL)).floor() as i64;
            let last_row = ((visible.bottom() - bounds.top()) / px(CELL)).ceil() as i64;
            for row in first_row..last_row {
                for column in first_column..last_column {
                    if (row + column) % 2 != 0 {
                        continue;
                    }
                    let origin = point(
                        bounds.left() + px(column as f32 * CELL),
                        bounds.top() + px(row as f32 * CELL),
                    );
                    let cell = gpui::Bounds::new(origin, size(px(CELL), px(CELL)));
                    let cell = cell.intersect(&visible);
                    if cell.size.width > px(0.) && cell.size.height > px(0.) {
                        window.paint_quad(fill(cell, light));
                    }
                }
            }
        },
    )
}

// --- Reading and decoding (background) ---

fn read(source: &ImageSource, text: Option<String>) -> Loaded {
    let (bytes, modified, svg): (Vec<u8>, Option<SystemTime>, bool) = match source {
        ImageSource::File(path) => {
            let svg = is_svg(path);
            let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
            let bytes = match text {
                Some(text) if svg => text.into_bytes(),
                _ => match std::fs::read(path) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        return Loaded {
                            picture: Err(error.to_string()),
                            bytes: 0,
                            format: "",
                            modified,
                        };
                    }
                },
            };
            (bytes, modified, svg)
        }
        ImageSource::Bytes {
            media_type, data, ..
        } => (data.to_vec(), None, media_type.contains("svg")),
    };
    let size = bytes.len() as u64;
    if svg {
        return Loaded {
            picture: parse_svg(&bytes).map(Decoded::Svg),
            bytes: size,
            format: "SVG",
            modified,
        };
    }
    let format = image::guess_format(&bytes).ok();
    Loaded {
        picture: decode_raster(&bytes, format),
        bytes: size,
        format: format.map_or("", format_name),
        modified,
    }
}

fn format_name(format: image::ImageFormat) -> &'static str {
    match format {
        image::ImageFormat::Png => "PNG",
        image::ImageFormat::Jpeg => "JPEG",
        image::ImageFormat::Gif => "GIF",
        image::ImageFormat::WebP => "WebP",
        image::ImageFormat::Bmp => "BMP",
        image::ImageFormat::Ico => "ICO",
        image::ImageFormat::Tiff => "TIFF",
        _ => "",
    }
}

/// A raster picture as gpui draws it: BGRA frames (all of a GIF's, it plays).
fn decode_raster(bytes: &[u8], format: Option<image::ImageFormat>) -> Result<Decoded, String> {
    use image::AnimationDecoder;
    let frames: Vec<image::Frame> = if format == Some(image::ImageFormat::Gif) {
        let decoder =
            image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
        decoder
            .into_frames()
            .collect_frames()
            .map_err(|e| e.to_string())?
    } else {
        let image = match format {
            Some(format) => image::load_from_memory_with_format(bytes, format),
            None => image::load_from_memory(bytes),
        }
        .map_err(|e| e.to_string())?;
        vec![image::Frame::new(image.into_rgba8())]
    };
    let mut frames = frames;
    let Some(first) = frames.first() else {
        return Err("no frames".to_string());
    };
    let (width, height) = first.buffer().dimensions();
    for frame in &mut frames {
        for pixel in frame.buffer_mut().as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
    }
    Ok(Decoded::Raster(
        Arc::new(RenderImage::new(frames)),
        width,
        height,
    ))
}

/// The fonts an SVG's text is set in: the system's, loaded once.
static FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
    let mut fonts = usvg::fontdb::Database::new();
    fonts.load_system_fonts();
    Arc::new(fonts)
});

fn parse_svg(bytes: &[u8]) -> Result<Arc<usvg::Tree>, String> {
    let options = usvg::Options {
        fontdb: FONTS.clone(),
        ..Default::default()
    };
    usvg::Tree::from_data(bytes, &options)
        .map(Arc::new)
        .map_err(|e| e.to_string())
}

/// The SVG drawn at `scale` device pixels per unit, as a BGRA image with straight alpha (what
/// gpui's raster pictures are).
fn draw_svg(tree: &usvg::Tree, scale: f32) -> Option<Arc<RenderImage>> {
    let size = tree.size();
    let width = (size.width() * scale).ceil().clamp(1., MAX_RASTER) as u32;
    let height = (size.height() * scale).ceil().clamp(1., MAX_RASTER) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    let transform = tiny_skia::Transform::from_scale(
        width as f32 / size.width().max(1e-3),
        height as f32 / size.height().max(1e-3),
    );
    resvg::render(tree, transform, &mut pixmap.as_mut());
    let mut data = pixmap.take();
    for pixel in data.as_chunks_mut::<4>().0 {
        let alpha = pixel[3];
        if alpha != 0 && alpha != 255 {
            for channel in &mut pixel[..3] {
                *channel =
                    ((*channel as u16 * 255 + alpha as u16 / 2) / alpha as u16).min(255) as u8;
            }
        }
        pixel.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(width, height, data)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_are_told_by_their_extension() {
        for name in [
            "a.png", "b.JPG", "c.jpeg", "d.gif", "e.webp", "f.bmp", "g.ico", "h.svg", "i.tiff",
        ] {
            assert!(is_image(Path::new(name)), "{name}");
        }
        for name in ["a.rs", "b.txt", "c", "png", "d.svgz"] {
            assert!(!is_image(Path::new(name)), "{name}");
        }
        assert!(is_svg(Path::new("icon.SVG")));
        assert!(!is_svg(Path::new("icon.png")));
    }

    #[test]
    fn a_png_is_decoded_to_its_size() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(3, 2, image::Rgba([255, 0, 0, 128]))
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let loaded = read(
            &ImageSource::Bytes {
                name: "x.png".into(),
                media_type: "image/png".into(),
                data: Arc::from(png.as_slice()),
            },
            None,
        );
        assert_eq!(loaded.format, "PNG");
        let Ok(Decoded::Raster(image, width, height)) = loaded.picture else {
            panic!("not decoded");
        };
        assert_eq!((width, height), (3, 2));
        // BGRA: red is in the third byte.
        assert_eq!(&image.as_bytes(0).unwrap()[..4], &[0, 0, 255, 128]);
    }

    #[test]
    fn an_svg_is_drawn_at_the_asked_scale() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="5"><rect width="10" height="5" fill="#00f"/></svg>"##;
        let tree = parse_svg(svg).unwrap();
        let image = draw_svg(&tree, 3.).unwrap();
        let size = image.size(0);
        assert_eq!((size.width.0, size.height.0), (30, 15));
        // Blue, opaque, in BGRA.
        assert_eq!(&image.as_bytes(0).unwrap()[..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn sizes_read_short() {
        assert_eq!(format_size(830), "830 B");
        assert_eq!(format_size(12 * 1024), "12 KB");
        assert_eq!(format_size(3 * 1024 * 1024 / 2), "1.5 MB");
    }
}
