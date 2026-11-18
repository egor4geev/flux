//! A plugin's gallery (API 0.5, FluxGram part 3): the `gallery` element — pictures in a grid of
//! squares under their groups' headings ("November 2026"), as a chat's photos and videos by month.
//! Three to five squares a row by the width, small gaps; it takes the height left, as a tree does,
//! and scrolls by itself — in a dialog too: a dialog with a gallery is as tall as it may be
//! (`PluginView::fills_dialog`); in a Settings block it has [`NATURAL_HEIGHT`].
//!
//! Only the rows on screen (and a little around them) are drawn: the grid is laid out as rows of
//! known heights, and the scroll offset picks the ones in sight. Each square is drawn as the
//! conversation's pictures are (`plugin_conversation::render_picture`: the preview, the progress,
//! the label, the ▶). A click on a picture whose full size is there (`full`; not a video's, `play`)
//! opens it in Flux's picture viewer with the gallery's full pictures around it; any other click —
//! a square with only its small picture (`path`), a video, one not downloaded — is
//! `activated(key)`. The squares on screen go to the plugin as `visible(first, last)` (their keys),
//! at most every [`VISIBLE_EVERY`] while scrolling and once more when it settles; within a screen
//! of the end, `load-older` while `more` is `more` (once per state of it).
//!
//! Nothing here asks for frames by itself: an idle gallery draws nothing.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use flux_plugin::api::events::UiEvent;
use flux_plugin::api::ui::{GalleryItem, GallerySpec, History};
use gpui::{
    AnyElement, Bounds, ClickEvent, Context, EventEmitter, Pixels, Render, ScrollHandle,
    SharedString, Task, WeakEntity, Window, div, prelude::*, px,
};

use crate::theme::Theme;
use crate::ui;

/// Between squares and rows.
const GAP: f32 = 3.;
/// The grid's inset from the window's edges.
const INSET: f32 = 8.;
/// A group's heading row.
const HEADING_HEIGHT: f32 = 32.;
/// A square is at least this wide while there are three a row or more…
const MIN_SIDE: f32 = 110.;
/// …and a row holds three to five.
const MIN_COLUMNS: usize = 3;
const MAX_COLUMNS: usize = 5;
/// Drawn beyond the screen above and below: scrolling shows no empty rows.
const OVERDRAW: f32 = 400.;
/// The height of a gallery whose window keeps its content's height (a Settings block).
pub const NATURAL_HEIGHT: f32 = 440.;
/// `visible` goes out at most this often while scrolling.
const VISIBLE_EVERY: Duration = Duration::from_millis(200);

/// What the gallery tells its window: an event for the plugin.
pub enum GalleryEvent {
    Ui(UiEvent),
}

/// A row of the grid.
#[derive(Clone, Debug, PartialEq)]
enum Row {
    /// A group's heading.
    Heading(SharedString),
    /// Squares: the items' indices.
    Squares(std::ops::Range<usize>),
}

/// The grid at a width: its rows, where each starts, the square's side.
#[derive(Clone, Debug, PartialEq)]
struct Grid {
    rows: Vec<(Row, f32)>,
    side: f32,
    height: f32,
}

impl Grid {
    fn new(items: &[GalleryItem], width: f32) -> Self {
        let width = (width - 2. * INSET).max(MIN_SIDE);
        let columns =
            (((width + GAP) / (MIN_SIDE + GAP)).floor() as usize).clamp(MIN_COLUMNS, MAX_COLUMNS);
        let side = ((width - GAP * (columns - 1) as f32) / columns as f32).floor();
        let mut rows = Vec::new();
        let mut y = 0.;
        let mut group: Option<&str> = None;
        let mut start = 0;
        while start < items.len() {
            let heading = items[start].group.as_deref();
            if heading.is_some() && heading != group {
                rows.push((
                    Row::Heading(heading.unwrap_or_default().to_string().into()),
                    y,
                ));
                y += HEADING_HEIGHT;
            }
            group = heading;
            // A row ends at its width or where the next group starts.
            let mut end = start + 1;
            while end < items.len()
                && end - start < columns
                && items[end].group.as_deref() == heading
            {
                end += 1;
            }
            rows.push((Row::Squares(start..end), y));
            y += side + GAP;
            start = end;
        }
        Self {
            rows,
            side,
            height: y,
        }
    }

    fn row_height(&self, row: &Row) -> f32 {
        match row {
            Row::Heading(_) => HEADING_HEIGHT,
            Row::Squares(_) => self.side + GAP,
        }
    }

    /// The rows touching `top..bottom`, by index.
    fn rows_in(&self, top: f32, bottom: f32) -> impl Iterator<Item = usize> + '_ {
        self.rows
            .iter()
            .enumerate()
            .filter(move |(_, (row, y))| *y + self.row_height(row) > top && *y < bottom)
            .map(|(ix, _)| ix)
    }

    /// The first and the last item of the squares touching `top..bottom`.
    fn items_in(&self, top: f32, bottom: f32) -> Option<(usize, usize)> {
        let mut seen = self
            .rows_in(top, bottom)
            .filter_map(|ix| match &self.rows[ix].0 {
                Row::Squares(items) if !items.is_empty() => Some(items.clone()),
                _ => None,
            });
        let first = seen.next()?;
        let last = seen.last().unwrap_or_else(|| first.clone());
        Some((first.start, last.end - 1))
    }
}

pub struct Gallery {
    plugin: SharedString,
    spec: GallerySpec,
    /// The window keeps its content's height: the gallery has [`NATURAL_HEIGHT`].
    natural: bool,
    scroll: ScrollHandle,
    /// The gallery's area as last drawn.
    viewport: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The last `visible` sent, when, and the one waiting for its turn.
    visible_sent: Option<(String, String)>,
    visible_at: Option<Instant>,
    visible_timer: Option<Task<()>>,
    /// `load-older` went out in this state of `more`.
    asked_older: bool,
}

impl EventEmitter<GalleryEvent> for Gallery {}

impl Gallery {
    pub fn new(plugin: SharedString, spec: GallerySpec, natural: bool) -> Self {
        Self {
            plugin,
            spec,
            natural,
            scroll: ScrollHandle::new(),
            viewport: Rc::default(),
            visible_sent: None,
            visible_at: None,
            visible_timer: None,
            asked_older: false,
        }
    }

    /// The element's spec in the window's new view: its items, whether there are more; whether
    /// the window keeps its content's height.
    pub fn set_spec(&mut self, spec: &GallerySpec, natural: bool, cx: &mut Context<Self>) {
        if self.natural != natural {
            self.natural = natural;
            cx.notify();
        }
        if self.spec == *spec {
            return;
        }
        if self.spec.more != spec.more || self.spec.items.len() != spec.items.len() {
            self.asked_older = false;
        }
        self.spec = spec.clone();
        cx.notify();
    }

    fn emit(&self, event: UiEvent, cx: &mut Context<Self>) {
        cx.emit(GalleryEvent::Ui(event));
    }

    fn grid(&self) -> Grid {
        let width = self
            .viewport
            .get()
            .map_or(400., |bounds| f32::from(bounds.size.width));
        Grid::new(&self.spec.items, width)
    }

    /// After a frame: the squares on screen go to the plugin (coalesced), the end in sight asks
    /// for more.
    fn observe(&mut self, cx: &mut Context<Self>) {
        let Some(viewport) = self.viewport.get() else {
            return;
        };
        let grid = self.grid();
        let top = -f32::from(self.scroll.offset().y);
        let height = f32::from(viewport.size.height);
        if self.spec.more == History::More && !self.asked_older && top + 2. * height >= grid.height
        {
            self.asked_older = true;
            self.emit(UiEvent::LoadOlder, cx);
        }
        let Some((first, last)) = grid.items_in(top, top + height) else {
            return;
        };
        let pair = (
            self.spec.items[first].key.clone(),
            self.spec.items[last].key.clone(),
        );
        if self.visible_sent.as_ref() == Some(&pair) {
            self.visible_timer = None;
            return;
        }
        let since = self.visible_at.map(|at| at.elapsed());
        if since.is_none_or(|since| since >= VISIBLE_EVERY) {
            self.visible_at = Some(Instant::now());
            self.visible_sent = Some(pair.clone());
            self.visible_timer = None;
            self.emit(UiEvent::Visible(pair), cx);
        } else if self.visible_timer.is_none() {
            let wait = VISIBLE_EVERY - since.unwrap_or_default();
            self.visible_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                this.update(cx, |this, cx| {
                    this.visible_timer = None;
                    this.observe(cx);
                })
                .ok();
            }));
        }
    }

    /// A click on a square: the viewer for a picture whose full size is there, the plugin
    /// otherwise (it downloads the picture, or opens the video).
    fn click(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let mut pictures = Vec::new();
        let mut shown = None;
        for (ix, item) in self.spec.items.iter().enumerate() {
            if item.image.play {
                continue;
            }
            let Some(path) = item.image.full.as_ref() else {
                continue;
            };
            let path = PathBuf::from(path);
            if !crate::plugin_images::allowed(&self.plugin, &path, cx) || !path.exists() {
                continue;
            }
            if ix == index {
                shown = Some(pictures.len());
            }
            pictures.push(crate::image_view::ImageSource::File(path));
        }
        match shown {
            Some(shown) => crate::image_popup::show(pictures, shown, window, cx),
            None => {
                if let Some(item) = self.spec.items.get(index) {
                    self.emit(UiEvent::Activated(item.key.clone()), cx);
                }
            }
        }
    }

    fn render_square(
        &self,
        index: usize,
        side: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let item = &self.spec.items[index];
        let picture = crate::plugin_conversation::render_picture(
            &item.image,
            (side, side),
            &self.plugin,
            cx.entity_id(),
            window,
            cx,
        );
        div()
            .id(SharedString::from(format!("gallery-{}", item.key)))
            .flex_none()
            .size(px(side))
            .cursor_pointer()
            .child(picture)
            .on_click(
                cx.listener(move |this, _: &ClickEvent, window, cx| this.click(index, window, cx)),
            )
            .into_any_element()
    }
}

impl Render for Gallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let frame = div()
            .w_full()
            .flex()
            .flex_col()
            .map(|frame| match self.natural {
                true => frame.flex_none().h(px(NATURAL_HEIGHT)),
                false => frame.flex_1().min_h_0(),
            });
        if self.spec.items.is_empty() {
            return frame
                .items_center()
                .justify_center()
                .text_color(ui.text_muted)
                .child(self.spec.empty_text.clone().unwrap_or_default())
                .into_any_element();
        }
        let grid = self.grid();
        let top = -f32::from(self.scroll.offset().y);
        let height = self
            .viewport
            .get()
            .map_or(NATURAL_HEIGHT, |bounds| f32::from(bounds.size.height));
        let rows: Vec<AnyElement> = grid
            .rows_in(top - OVERDRAW, top + height + OVERDRAW)
            .collect::<Vec<_>>()
            .into_iter()
            .map(|ix| {
                let (row, y) = grid.rows[ix].clone();
                let placed = div()
                    .absolute()
                    .top(px(y))
                    .left(px(INSET))
                    .right(px(INSET))
                    .h(px(grid.row_height(&row)));
                match row {
                    Row::Heading(heading) => placed
                        .flex()
                        .items_end()
                        .pb(px(6.))
                        .px(px(2.))
                        .child(ui::section_label(heading, ui).truncate())
                        .into_any_element(),
                    Row::Squares(items) => placed
                        .flex()
                        .gap(px(GAP))
                        .children(
                            items.map(|index| self.render_square(index, grid.side, window, cx)),
                        )
                        .into_any_element(),
                }
            })
            .collect();
        let viewport = self.viewport.clone();
        let weak: WeakEntity<Self> = cx.entity().downgrade();
        frame
            .relative()
            .on_children_prepainted(move |children, _, cx| {
                viewport.set(children.first().copied());
                weak.update(cx, |this, cx| this.observe(cx)).ok();
            })
            .child(
                div()
                    .id("plugin-gallery")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .h(px(grid.height + INSET))
                            .children(rows),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_plugin::api::ui::{ImageShape, ImageSpec};

    fn item(key: &str, group: Option<&str>) -> GalleryItem {
        GalleryItem {
            key: key.to_string(),
            image: ImageSpec {
                path: None,
                full: None,
                preview: None,
                width: 0,
                height: 0,
                progress: None,
                shape: ImageShape::Square,
                label: None,
                play: false,
            },
            group: group.map(str::to_string),
        }
    }

    #[test]
    fn rows_fill_up_to_the_width_and_break_at_groups() {
        let items: Vec<GalleryItem> = (0..5)
            .map(|ix| item(&ix.to_string(), Some("November")))
            .chain((5..7).map(|ix| item(&ix.to_string(), Some("October"))))
            .collect();
        // Three a row at 400 (384 inside the insets): 126 + 3 + 126 + 3 + 126.
        let grid = Grid::new(&items, 400.);
        let shape: Vec<Row> = grid.rows.iter().map(|(row, _)| row.clone()).collect();
        assert_eq!(
            shape,
            vec![
                Row::Heading("November".into()),
                Row::Squares(0..3),
                Row::Squares(3..5),
                Row::Heading("October".into()),
                Row::Squares(5..7),
            ]
        );
        assert_eq!(grid.side, 126.);
        // Wider: five a row at most.
        let wide = Grid::new(&items, 1200.);
        assert_eq!(wide.rows[1].0, Row::Squares(0..5));
        // The squares on a screen: the first and the last item.
        let top = grid.rows[2].1;
        assert_eq!(grid.items_in(top, top + 10.), Some((3, 4)));
        assert_eq!(grid.items_in(0., grid.height), Some((0, 6)));
    }
}
