use crate::{
    AtlasKey, AtlasTextureId, AtlasTextureKind, AtlasTile, Bounds, DevicePixels, PlatformAtlas,
    Point, Size, platform::AtlasTextureList,
};
use anyhow::{Context as _, Result};
use collections::FxHashMap;
use derive_more::{Deref, DerefMut};
use etagere::AtlasAllocator;
use metal::Device;
use parking_lot::Mutex;
use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) struct MetalAtlas {
    state: Mutex<MetalAtlasState>,
    /// Flux patch: the number of the last frame the GPU has completed
    /// ([`MetalAtlas::frame_completed`]).
    completed_frame: AtomicU64,
}

impl MetalAtlas {
    pub(crate) fn new(device: Device) -> Self {
        MetalAtlas {
            state: Mutex::new(MetalAtlasState {
                device: AssertSend(device),
                monochrome_textures: Default::default(),
                polychrome_textures: Default::default(),
                tiles_by_key: Default::default(),
                removed_tiles: Vec::new(),
                removed_since_draw: false,
                committed_frame: 0,
            }),
            completed_frame: AtomicU64::new(0),
        }
    }

    pub(crate) fn metal_texture(&self, id: AtlasTextureId) -> metal::Texture {
        self.state.lock().texture(id).metal_texture.clone()
    }

    /// Flux patch: the renderer commits a frame that samples the atlas; returns its number for
    /// [`MetalAtlas::frame_completed`]. The removed tiles no frame samples any more are freed.
    pub(crate) fn frame_committed(&self) -> u64 {
        let completed = self.completed_frame.load(Ordering::Acquire);
        let mut lock = self.state.lock();
        lock.free_removed_tiles(completed);
        lock.committed_frame += 1;
        lock.committed_frame
    }

    /// Flux patch: the GPU has completed frame `frame` — and every frame committed before it: a
    /// command queue runs its command buffers in order. Called on Metal's thread, by the command
    /// buffer's completion handler.
    pub(crate) fn frame_completed(&self, frame: u64) {
        self.completed_frame.fetch_max(frame, Ordering::AcqRel);
    }
}

struct MetalAtlasState {
    device: AssertSend<Device>,
    monochrome_textures: AtlasTextureList<MetalAtlasTexture>,
    polychrome_textures: AtlasTextureList<MetalAtlasTexture>,
    tiles_by_key: FxHashMap<AtlasKey, AtlasTile>,
    /// Flux patch: tiles removed whose space isn't free yet ([`RemovedTile`]).
    removed_tiles: Vec<RemovedTile>,
    /// Some of `removed_tiles` were removed after the window last began to draw.
    removed_since_draw: bool,
    /// The number of the last frame the renderer committed ([`MetalAtlas::frame_committed`]).
    committed_frame: u64,
}

/// Flux patch: a tile removed from the atlas ([`PlatformAtlas::remove`]). Its space is freed for
/// other tiles once no frame can sample it: neither a frame the GPU hasn't completed yet (the
/// renderer doesn't wait for the previous frame, and a tile is written by the CPU at once), nor
/// the window's scene drawn before the removal, which the window may present again until it draws
/// a new one.
struct RemovedTile {
    tile: AtlasTile,
    /// The last frame that may sample it: the last one committed when the window began to draw
    /// after the removal — the first [`PlatformAtlas::get_or_insert_with`] since, as only the
    /// window's drawing (its paint) asks for tiles. `None` until then.
    last_frame: Option<u64>,
}

impl PlatformAtlas for MetalAtlas {
    fn get_or_insert_with<'a>(
        &self,
        key: &AtlasKey,
        build: &mut dyn FnMut() -> Result<Option<(Size<DevicePixels>, Cow<'a, [u8]>)>>,
    ) -> Result<Option<AtlasTile>> {
        let mut lock = self.state.lock();
        // Flux patch: the window draws a new scene, without the tiles removed before: no frame
        // committed from now on samples them.
        if lock.removed_since_draw {
            lock.removed_since_draw = false;
            let frame = lock.committed_frame;
            for removed in &mut lock.removed_tiles {
                removed.last_frame.get_or_insert(frame);
            }
        }
        if let Some(tile) = lock.tiles_by_key.get(key) {
            Ok(Some(tile.clone()))
        } else {
            let Some((size, bytes)) = build()? else {
                return Ok(None);
            };
            // Flux patch: the space of the removed tiles no frame samples any more is taken
            // again first.
            lock.free_removed_tiles(self.completed_frame.load(Ordering::Acquire));
            let tile = lock
                .allocate(size, key.texture_kind())
                .context("failed to allocate")?;
            let texture = lock.texture(tile.texture_id);
            texture.upload(tile.bounds, &bytes);
            lock.tiles_by_key.insert(key.clone(), tile.clone());
            Ok(Some(tile))
        }
    }

    /// Flux patch: the key goes at once (drawn again, it gets a new tile); the tile's space is
    /// freed once no frame samples it ([`RemovedTile`]), and a texture left without tiles is
    /// released. gpui 0.2.2 kept the key and never freed a tile: a texture was released only once
    /// every one of its tiles had been removed, so the frames of an animation, each a new image,
    /// filled one texture after another, and a texture also holding a picture still shown was
    /// never released.
    fn remove(&self, key: &AtlasKey) {
        let mut lock = self.state.lock();
        let Some(tile) = lock.tiles_by_key.remove(key) else {
            return;
        };
        lock.removed_tiles.push(RemovedTile {
            tile,
            last_frame: None,
        });
        lock.removed_since_draw = true;
    }
}

impl MetalAtlasState {
    /// Flux patch: frees the space of the removed tiles no frame samples any more — their last
    /// frame is known and the GPU has completed it (`completed`).
    fn free_removed_tiles(&mut self, completed: u64) {
        let mut index = 0;
        while index < self.removed_tiles.len() {
            if self.removed_tiles[index]
                .last_frame
                .is_some_and(|frame| frame <= completed)
            {
                let removed = self.removed_tiles.swap_remove(index);
                self.free_tile(&removed.tile);
            } else {
                index += 1;
            }
        }
    }

    /// Flux patch: gives a tile's space back to its texture's allocator; a texture left without
    /// tiles is released (its slot taken by the next new texture).
    fn free_tile(&mut self, tile: &AtlasTile) {
        let id = tile.texture_id;
        let textures = match id.kind {
            AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
            AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
        };
        let Some(Some(texture)) = textures.textures.get_mut(id.index as usize) else {
            return;
        };
        texture.allocator.deallocate(tile.tile_id.into());
        texture.live_atlas_keys -= 1;
        if texture.live_atlas_keys == 0 {
            textures.textures[id.index as usize] = None;
            textures.free_list.push(id.index as usize);
            self.log_textures("released", id);
        }
    }

    /// Flux patch: a debug line of the atlas's textures, as one is created or released
    /// (`FLUX_LOG=gpui::platform::mac::metal_atlas=debug`).
    fn log_textures(&self, change: &str, id: AtlasTextureId) {
        if !log::log_enabled!(log::Level::Debug) {
            return;
        }
        let (mut count, mut bytes) = (0, 0u64);
        for texture in self
            .monochrome_textures
            .textures
            .iter()
            .chain(&self.polychrome_textures.textures)
            .flatten()
        {
            count += 1;
            bytes += texture.metal_texture.width()
                * texture.metal_texture.height()
                * texture.bytes_per_pixel() as u64;
        }
        log::debug!(
            "texture {change}: {:?} {}; {count} textures, {:.1} MB, {} tiles removed not freed \
             yet; frame {}",
            id.kind,
            id.index,
            bytes as f64 / (1024. * 1024.),
            self.removed_tiles.len(),
            self.committed_frame,
        );
    }

    fn allocate(
        &mut self,
        size: Size<DevicePixels>,
        texture_kind: AtlasTextureKind,
    ) -> Option<AtlasTile> {
        {
            let textures = match texture_kind {
                AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
                AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
            };

            if let Some(tile) = textures
                .iter_mut()
                .rev()
                .find_map(|texture| texture.allocate(size))
            {
                return Some(tile);
            }
        }

        let texture = self.push_texture(size, texture_kind);
        texture.allocate(size)
    }

    fn push_texture(
        &mut self,
        min_size: Size<DevicePixels>,
        kind: AtlasTextureKind,
    ) -> &mut MetalAtlasTexture {
        const DEFAULT_ATLAS_SIZE: Size<DevicePixels> = Size {
            width: DevicePixels(1024),
            height: DevicePixels(1024),
        };
        // Max texture size on all modern Apple GPUs. Anything bigger than that crashes in validateWithDevice.
        const MAX_ATLAS_SIZE: Size<DevicePixels> = Size {
            width: DevicePixels(16384),
            height: DevicePixels(16384),
        };
        let size = min_size.min(&MAX_ATLAS_SIZE).max(&DEFAULT_ATLAS_SIZE);
        let texture_descriptor = metal::TextureDescriptor::new();
        texture_descriptor.set_width(size.width.into());
        texture_descriptor.set_height(size.height.into());
        let pixel_format;
        let usage;
        match kind {
            AtlasTextureKind::Monochrome => {
                pixel_format = metal::MTLPixelFormat::A8Unorm;
                usage = metal::MTLTextureUsage::ShaderRead;
            }
            AtlasTextureKind::Polychrome => {
                pixel_format = metal::MTLPixelFormat::BGRA8Unorm;
                usage = metal::MTLTextureUsage::ShaderRead;
            }
        }
        texture_descriptor.set_pixel_format(pixel_format);
        texture_descriptor.set_usage(usage);
        let metal_texture = self.device.new_texture(&texture_descriptor);

        let texture_list = match kind {
            AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
            AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
        };

        let index = texture_list.free_list.pop();

        let atlas_texture = MetalAtlasTexture {
            id: AtlasTextureId {
                index: index.unwrap_or(texture_list.textures.len()) as u32,
                kind,
            },
            // Flux patch: an allocator tracking each tile, so the space of one removed is free
            // again at once (`free_tile`); gpui 0.2.2's bucketed one frees a tile's space only
            // with every tile of its bucket, and a bucket takes at most 4095 tiles in its life.
            allocator: AtlasAllocator::new(size.into()),
            metal_texture: AssertSend(metal_texture),
            live_atlas_keys: 0,
        };
        let id = atlas_texture.id;

        if let Some(ix) = index {
            texture_list.textures[ix] = Some(atlas_texture);
        } else {
            texture_list.textures.push(Some(atlas_texture));
        }
        self.log_textures("created", id);
        let texture_list = match kind {
            AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
            AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
        };
        texture_list.textures[id.index as usize].as_mut().unwrap()
    }

    fn texture(&self, id: AtlasTextureId) -> &MetalAtlasTexture {
        let textures = match id.kind {
            crate::AtlasTextureKind::Monochrome => &self.monochrome_textures,
            crate::AtlasTextureKind::Polychrome => &self.polychrome_textures,
        };
        textures[id.index as usize].as_ref().unwrap()
    }
}

struct MetalAtlasTexture {
    id: AtlasTextureId,
    allocator: AtlasAllocator,
    metal_texture: AssertSend<metal::Texture>,
    /// Flux patch: the tiles allocated and not freed yet — a removed tile counts until its space
    /// is freed ([`RemovedTile`]).
    live_atlas_keys: u32,
}

impl MetalAtlasTexture {
    fn allocate(&mut self, size: Size<DevicePixels>) -> Option<AtlasTile> {
        let allocation = self.allocator.allocate(size.into())?;
        let tile = AtlasTile {
            texture_id: self.id,
            tile_id: allocation.id.into(),
            bounds: Bounds {
                origin: allocation.rectangle.min.into(),
                size,
            },
            padding: 0,
        };
        self.live_atlas_keys += 1;
        Some(tile)
    }

    fn upload(&self, bounds: Bounds<DevicePixels>, bytes: &[u8]) {
        let region = metal::MTLRegion::new_2d(
            bounds.origin.x.into(),
            bounds.origin.y.into(),
            bounds.size.width.into(),
            bounds.size.height.into(),
        );
        self.metal_texture.replace_region(
            region,
            0,
            bytes.as_ptr() as *const _,
            bounds.size.width.to_bytes(self.bytes_per_pixel()) as u64,
        );
    }

    fn bytes_per_pixel(&self) -> u8 {
        use metal::MTLPixelFormat::*;
        match self.metal_texture.pixel_format() {
            A8Unorm | R8Unorm => 1,
            RGBA8Unorm | BGRA8Unorm => 4,
            _ => unimplemented!(),
        }
    }
}

impl From<Size<DevicePixels>> for etagere::Size {
    fn from(size: Size<DevicePixels>) -> Self {
        etagere::Size::new(size.width.into(), size.height.into())
    }
}

impl From<etagere::Point> for Point<DevicePixels> {
    fn from(value: etagere::Point) -> Self {
        Point {
            x: DevicePixels::from(value.x),
            y: DevicePixels::from(value.y),
        }
    }
}

impl From<etagere::Size> for Size<DevicePixels> {
    fn from(size: etagere::Size) -> Self {
        Size {
            width: DevicePixels::from(size.width),
            height: DevicePixels::from(size.height),
        }
    }
}

impl From<etagere::Rectangle> for Bounds<DevicePixels> {
    fn from(rectangle: etagere::Rectangle) -> Self {
        Bounds {
            origin: rectangle.min.into(),
            size: rectangle.size().into(),
        }
    }
}

#[derive(Deref, DerefMut)]
struct AssertSend<T>(T);

unsafe impl<T> Send for AssertSend<T> {}
