//! Pictures of plugins (API 0.5, FluxGram part 1): the `image` element, avatars, and the photos,
//! stickers and link previews of a conversation. A plugin names a picture by its absolute path;
//! Flux decodes it off the window's thread (the background executor) at the size it is shown and
//! keeps the decoded pictures within a memory budget, the least recently drawn going first — not
//! gpui's `img(path)`, whose asset cache never lets a picture go. An evicted picture leaves the
//! windows' sprite atlases too (`App::drop_image`).
//!
//! A picture that isn't there yet shows its placeholder: the tiny JPEG a message carries
//! (`image-spec.preview`), decoded and blurred here as well, or a dim box of the picture's size.
//! Animated GIF and WebP show their first frame.
//!
//! A picture is cut to the proportions of its box (its middle) before it is scaled: gpui rounds an
//! image's own corners, so a picture that overflowed its box (covering it) would lose the rounded
//! and the round corners to the box's straight edges.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{App, EntityId, Global, RenderImage};
use image::{DynamicImage, Frame, RgbaImage};

/// What the decoded pictures may take, all windows together.
const BUDGET: usize = 128 * 1024 * 1024;
/// A picture drawn this recently is on screen: older ones go first, and it goes only when the
/// screen alone is over the budget.
const IN_SIGHT: Duration = Duration::from_millis(600);
/// Sizes are rounded up to a step of this many device pixels: a column resized pixel by pixel
/// doesn't decode its pictures for every pixel.
const SIZE_STEP: u32 = 64;
/// The largest side of a decoded picture whose shown size isn't known.
const MAX_SIDE: u32 = 2048;
/// A preview (a JPEG of a few dozen pixels) is enlarged this many times and blurred.
const PREVIEW_SCALE: u32 = 4;
const PREVIEW_BLUR: f32 = 6.;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
    /// A file at a size, in device pixels (the longer side rounded up to [`SIZE_STEP`], the
    /// other keeping the box's proportions).
    File {
        path: PathBuf,
        width: u32,
        height: u32,
    },
    /// A message's preview, by the hash of its bytes, cut to proportions (out of [`ASPECT_BASE`]).
    Preview(u64, u32, u32),
}

/// A preview's proportions are kept as its box's sides scaled to this longer side.
const ASPECT_BASE: u32 = 256;

enum Slot {
    /// Being decoded; the views that asked are drawn again when it is ready.
    Loading(HashSet<EntityId>),
    Ready {
        image: Arc<RenderImage>,
        bytes: usize,
        used: Instant,
    },
    /// The file isn't there or isn't a picture: the placeholder stays.
    Failed,
}

/// The decoded pictures (a global): by path and size, within [`BUDGET`].
#[derive(Default)]
pub struct Pictures {
    slots: HashMap<Key, Slot>,
    /// The bytes of the ready pictures.
    total: usize,
    /// Paths refused once (outside the plugin's folders): logged once each.
    refused: HashSet<PathBuf>,
    /// Files the user handed to a plugin (its conversation's field), by plugin: its pictures too.
    granted: HashMap<String, HashSet<PathBuf>>,
}

impl Global for Pictures {}

/// The bytes the decoded pictures take now (for the measurements and the tests).
#[allow(dead_code)]
pub fn memory(cx: &App) -> usize {
    cx.try_global::<Pictures>()
        .map_or(0, |pictures| pictures.total)
}

/// Whether a plugin may show the picture at `path`: an absolute path without `..` in the plugin's
/// install folder or its data folder, or a file the user handed to the plugin ([`grant`]). Others
/// are refused with a line in the log, once.
pub fn allowed(plugin: &str, path: &Path, cx: &mut App) -> bool {
    let inside = |root: &Path| path.starts_with(root);
    let granted = cx.try_global::<Pictures>().is_some_and(|pictures| {
        pictures
            .granted
            .get(plugin)
            .is_some_and(|files| files.contains(path))
    });
    let ok = path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        && (granted
            || inside(&flux_plugin::paths::data_dir(plugin))
            || crate::icons::plugin_folder(plugin).is_some_and(|folder| inside(&folder)));
    if !ok {
        let pictures = cx.default_global::<Pictures>();
        if pictures.refused.insert(path.to_path_buf()) {
            log::warn!(
                "plugin {plugin}: picture {} is outside its folders, not shown",
                path.display()
            );
        }
    }
    ok
}

/// The user handed the file at `path` to the plugin — picked, dropped or pasted into its
/// conversation's field: the plugin may show it as a picture (a sent message's photo from
/// ~/Downloads), until Flux quits.
pub fn grant(plugin: &str, path: &Path, cx: &mut App) {
    let pictures = cx.default_global::<Pictures>();
    pictures.refused.remove(path);
    pictures
        .granted
        .entry(plugin.to_string())
        .or_default()
        .insert(path.to_path_buf());
}

/// The picture at `path` decoded to fit `width` × `height` device pixels (0 — unknown: up to
/// [`MAX_SIDE`]). `None` while it is decoded — `view` is drawn again when it is ready — or when it
/// can't be read.
pub fn file(
    path: &Path,
    width: u32,
    height: u32,
    view: EntityId,
    cx: &mut App,
) -> Option<Arc<RenderImage>> {
    let (max_width, max_height) = decoded_size(width, height);
    let key = Key::File {
        path: path.to_path_buf(),
        width: max_width,
        height: max_height,
    };
    let path = path.to_path_buf();
    get(key, view, cx, move || {
        decode_file(&path, max_width, max_height)
    })
}

/// The size a picture shown at `width` × `height` device pixels is decoded at: the longer side
/// rounded up to [`SIZE_STEP`] (within twice [`MAX_SIDE`]), the other keeping the proportions —
/// the picture is cut to them. An unknown side (0) is [`MAX_SIDE`].
fn decoded_size(width: u32, height: u32) -> (u32, u32) {
    let (width, height) = match (width, height) {
        (0, 0) => (MAX_SIDE, MAX_SIDE),
        (0, height) => (MAX_SIDE, height),
        (width, 0) => (width, MAX_SIDE),
        sides => sides,
    };
    let longer = width.max(height);
    let stepped = (longer.div_ceil(SIZE_STEP) * SIZE_STEP).min(MAX_SIDE * 2);
    let scale = |side: u32| ((side as f64 * stepped as f64 / longer as f64).round() as u32).max(1);
    (scale(width), scale(height))
}

/// A message's preview (`image-spec.preview`: a tiny JPEG) for a box of `width` × `height`
/// (any unit: only the proportions count), enlarged and blurred; `None` while it is decoded or
/// when the bytes aren't a picture.
pub fn preview(
    bytes: &[u8],
    width: f32,
    height: f32,
    view: EntityId,
    cx: &mut App,
) -> Option<Arc<RenderImage>> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    let (aspect_width, aspect_height) = aspect(width, height);
    let bytes = bytes.to_vec();
    get(
        Key::Preview(hasher.finish(), aspect_width, aspect_height),
        view,
        cx,
        move || decode_preview(&bytes, aspect_width, aspect_height),
    )
}

/// A box's proportions as whole sides, the longer one [`ASPECT_BASE`].
fn aspect(width: f32, height: f32) -> (u32, u32) {
    if !(width > 0. && height > 0.) {
        return (ASPECT_BASE, ASPECT_BASE);
    }
    let longer = width.max(height);
    let side = |side: f32| ((side / longer * ASPECT_BASE as f32).round() as u32).max(1);
    (side(width), side(height))
}

/// The middle of a picture with the proportions `width` : `height` (all of it when they match).
fn cut_to(image: DynamicImage, width: u32, height: u32) -> DynamicImage {
    let (have_width, have_height) = (image.width().max(1), image.height().max(1));
    let (width, height) = (width.max(1) as u64, height.max(1) as u64);
    // The widest part of the proportions that fits, then the tallest.
    let mut cut_width = have_width as u64;
    let mut cut_height = (cut_width * height).div_ceil(width);
    if cut_height > have_height as u64 {
        cut_height = have_height as u64;
        cut_width = ((cut_height * width).div_ceil(height)).min(have_width as u64);
    }
    let (cut_width, cut_height) = (cut_width.max(1) as u32, cut_height.max(1) as u32);
    if (cut_width, cut_height) == (have_width, have_height) {
        return image;
    }
    image.crop_imm(
        (have_width - cut_width) / 2,
        (have_height - cut_height) / 2,
        cut_width,
        cut_height,
    )
}

fn get(
    key: Key,
    view: EntityId,
    cx: &mut App,
    decode: impl FnOnce() -> Option<RgbaImage> + Send + 'static,
) -> Option<Arc<RenderImage>> {
    let pictures = cx.default_global::<Pictures>();
    match pictures.slots.get_mut(&key) {
        Some(Slot::Ready { image, used, .. }) => {
            *used = Instant::now();
            return Some(image.clone());
        }
        Some(Slot::Loading(views)) => {
            views.insert(view);
            return None;
        }
        Some(Slot::Failed) => return None,
        None => {}
    }
    pictures
        .slots
        .insert(key.clone(), Slot::Loading(HashSet::from([view])));
    let task = cx.background_executor().spawn(async move { decode() });
    cx.spawn(async move |cx| {
        let decoded = task.await;
        cx.update(|cx| ready(key, decoded, cx)).ok();
    })
    .detach();
    None
}

/// A picture is decoded: it takes its place, the views that asked are drawn again, and the least
/// recently drawn pictures go while the budget is exceeded.
fn ready(key: Key, decoded: Option<RgbaImage>, cx: &mut App) {
    let views = {
        let pictures = cx.default_global::<Pictures>();
        let views = match pictures.slots.remove(&key) {
            Some(Slot::Loading(views)) => views,
            _ => HashSet::new(),
        };
        let slot = match decoded {
            Some(buffer) => {
                let bytes = buffer.as_raw().len();
                pictures.total += bytes;
                Slot::Ready {
                    image: Arc::new(RenderImage::new([Frame::new(buffer)])),
                    bytes,
                    used: Instant::now(),
                }
            }
            None => Slot::Failed,
        };
        pictures.slots.insert(key, slot);
        views
    };
    for image in evict(cx.default_global::<Pictures>(), Instant::now()) {
        cx.drop_image(image, None);
    }
    for view in views {
        cx.notify(view);
    }
}

/// Takes the least recently drawn pictures out while the total is over the budget — those on
/// screen only when the screen alone is over it — and returns them for the atlases to let go.
fn evict(pictures: &mut Pictures, now: Instant) -> Vec<Arc<RenderImage>> {
    if pictures.total <= BUDGET {
        return Vec::new();
    }
    let mut ready: Vec<(Instant, Key)> = pictures
        .slots
        .iter()
        .filter_map(|(key, slot)| match slot {
            Slot::Ready { used, .. } => Some((*used, key.clone())),
            _ => None,
        })
        .collect();
    ready.sort_by_key(|(used, _)| *used);
    let mut gone = Vec::new();
    for (used, key) in ready {
        if pictures.total <= BUDGET {
            break;
        }
        // What is on screen stays unless the screen alone is over twice the budget.
        if now.duration_since(used) < IN_SIGHT && pictures.total <= BUDGET * 2 {
            break;
        }
        if let Some(Slot::Ready { image, bytes, .. }) = pictures.slots.remove(&key) {
            pictures.total -= bytes;
            gone.push(image);
        }
    }
    gone
}

/// A file decoded, cut to the size's proportions and scaled down to it (never up), in gpui's BGRA
/// order.
fn decode_file(path: &Path, max_width: u32, max_height: u32) -> Option<RgbaImage> {
    let reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let decoded = cut_to(reader.decode().ok()?, max_width, max_height);
    let decoded = if decoded.width() > max_width || decoded.height() > max_height {
        decoded.thumbnail(max_width.max(1), max_height.max(1))
    } else {
        decoded
    };
    Some(to_bgra(decoded))
}

/// A preview cut to the proportions, enlarged and blurred: it stands for the picture, not shows it.
fn decode_preview(bytes: &[u8], width: u32, height: u32) -> Option<RgbaImage> {
    let decoded = cut_to(image::load_from_memory(bytes).ok()?, width, height);
    let (width, height) = (decoded.width().max(1), decoded.height().max(1));
    let larger = decoded.resize_exact(
        width * PREVIEW_SCALE,
        height * PREVIEW_SCALE,
        image::imageops::FilterType::Triangle,
    );
    Some(to_bgra(larger.blur(PREVIEW_BLUR)))
}

fn to_bgra(image: DynamicImage) -> RgbaImage {
    let mut buffer = image.into_rgba8();
    for pixel in buffer.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(pictures: &mut Pictures, name: &str, bytes: usize, used: Instant) {
        let buffer = RgbaImage::new(1, 1);
        pictures.total += bytes;
        pictures.slots.insert(
            Key::File {
                path: PathBuf::from(name),
                width: 64,
                height: 64,
            },
            Slot::Ready {
                image: Arc::new(RenderImage::new([Frame::new(buffer)])),
                bytes,
                used,
            },
        );
    }

    #[test]
    fn the_least_recently_drawn_go_first_and_the_screen_stays() {
        let now = Instant::now();
        let long_ago = now - Duration::from_secs(10);
        let mut pictures = Pictures::default();
        picture(&mut pictures, "old", BUDGET / 2, long_ago);
        picture(
            &mut pictures,
            "older",
            BUDGET / 2,
            long_ago - Duration::from_secs(1),
        );
        picture(&mut pictures, "seen", BUDGET / 2, now);
        let gone = evict(&mut pictures, now);
        assert_eq!(gone.len(), 1);
        assert_eq!(pictures.total, BUDGET);
        assert!(!pictures.slots.contains_key(&Key::File {
            path: PathBuf::from("older"),
            width: 64,
            height: 64,
        }));
        // Only pictures on screen: they stay up to twice the budget.
        let mut pictures = Pictures::default();
        picture(&mut pictures, "a", BUDGET, now);
        picture(&mut pictures, "b", BUDGET / 2, now);
        assert!(evict(&mut pictures, now).is_empty());
    }

    #[test]
    fn a_picture_is_cut_to_its_boxs_proportions() {
        // The longer side steps up, the other keeps the box's proportions.
        assert_eq!(decoded_size(400, 300), (448, 336));
        assert_eq!(decoded_size(68, 68), (128, 128));
        assert_eq!(decoded_size(0, 0), (MAX_SIDE, MAX_SIDE));
        // A wide picture in a square box (a round video, an album's square): its middle.
        let wide = DynamicImage::new_rgba8(300, 100);
        let cut = cut_to(wide, 128, 128);
        assert_eq!((cut.width(), cut.height()), (100, 100));
        let tall = DynamicImage::new_rgba8(90, 400);
        let cut = cut_to(tall, 4, 3);
        assert_eq!((cut.width(), cut.height()), (90, 68));
        let same = DynamicImage::new_rgba8(40, 30);
        let cut = cut_to(same, 448, 336);
        assert_eq!((cut.width(), cut.height()), (40, 30));
        assert_eq!(aspect(200., 100.), (256, 128));
    }
}
