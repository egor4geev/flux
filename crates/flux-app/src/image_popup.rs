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

use std::sync::Arc;

use gpui::{
    App, Context, DismissEvent, Div, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding,
    Render, Subscription, Window, actions, div, prelude::*, px,
};

use crate::i18n::tr;
use crate::icons::{IconName, icon};
use crate::image_view::{
    ActualSize, FitToWindow, ImageSource, ImageView, ImageViewEvent, ZoomIn, ZoomOut,
};
use crate::motion::{self, Appear};
use crate::theme::{self, Theme, UiColors};
use crate::ui;
use crate::workspace::Workspace;

pub const CONTEXT: &str = "ImagePopup";

/// How deep the veil over the window is: the picture stands out, as in a photo viewer (a dialog's
/// shade is much lighter).
const VEIL: f32 = 0.6;
/// The toolbar and the arrows keep this far from the edges (inside the picture's margin).
const EDGE: f32 = 12.;
const TOOLBAR_HEIGHT: f32 = 36.;
const ARROW_SIZE: f32 = 36.;

actions!(
    image_popup,
    [
        /// Esc: closes the popup.
        Dismiss,
        /// ←: the previous picture of the message.
        PreviousPicture,
        /// →: the next one.
        NextPicture,
    ]
);

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

/// The veil over the window under the popup; it takes the window's clicks.
pub fn veil(ui: UiColors) -> Div {
    div().occlude().bg(UiColors::tint(ui.shadow, VEIL))
}

pub struct ImagePopup {
    focus_handle: FocusHandle,
    pictures: Vec<ImageSource>,
    /// The pictures' views, made as they (or their neighbors) come into sight: each keeps its zoom.
    views: Vec<Option<Entity<ImageView>>>,
    index: usize,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for ImagePopup {}

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
            pictures,
            index,
            _subscriptions: Vec::new(),
        };
        popup.show(index, cx);
        popup
    }

    /// Brings the `index`th picture into sight; its neighbors start loading, so ← / → show them
    /// at once.
    fn show(&mut self, index: usize, cx: &mut Context<Self>) {
        self.index = index;
        for index in index.saturating_sub(1)..=index + 1 {
            self.view(index, cx);
        }
        cx.notify();
    }

    /// The view of the `index`th picture, made the first time it is asked for.
    fn view(&mut self, index: usize, cx: &mut Context<Self>) -> Option<Entity<ImageView>> {
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

/// A thin line between the toolbar's groups.
fn separator(ui: UiColors) -> Div {
    div().flex_none().w(px(1.)).h(px(16.)).mx_1().bg(ui.divider)
}

impl Render for ImagePopup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let view = self.view(self.index, cx);
        let several = self.pictures.len() > 1;
        div()
            .id("image-popup")
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .on_action(cx.listener(|this, _: &PreviousPicture, _, cx| this.go(-1, cx)))
            .on_action(cx.listener(|this, _: &NextPicture, _, cx| this.go(1, cx)))
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
            .children(view.map(|view| self.render_toolbar(&view, ui, cx)))
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
