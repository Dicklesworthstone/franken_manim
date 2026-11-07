//! Exact completed tiles for the camera rasterizer.
//!
//! The key is an ordered run of owned, immutable source identities. Identities
//! are issued only after the retained camera compiler checks every source
//! dependency; they retain no scene records, images or projected geometry.
//! Camera values and tiling form a separate complete frame closure. Only final
//! RGBA16F bytes are cached, so depth tests and adaptive samples retain their
//! original local arithmetic whenever a tile must rasterize.

use super::{CacheStats, PixelTileCacheError, frame_row_start, rgba16f_row_bytes, tile_rect};
use crate::three_d::{PreparedThreeDProjection, same_projection_camera};
use crate::{Camera, Tiling};
use fmn_frame::FrameBuffer;
use std::sync::Arc;

/// One source may expand to several ordered draws (notably a dot cloud).
#[derive(Debug, Clone)]
pub(crate) struct CameraSourceKey {
    generation: Arc<()>,
    part: usize,
    volatile: bool,
}

impl CameraSourceKey {
    pub(crate) fn new(generation: &Arc<()>, part: usize, volatile: bool) -> Self {
        Self {
            generation: Arc::clone(generation),
            part,
            volatile,
        }
    }

    fn matches(&self, other: &Self) -> bool {
        !self.volatile
            && !other.volatile
            && self.part == other.part
            && Arc::ptr_eq(&self.generation, &other.generation)
    }
}

// Charge an owner for every key slot, even when many slots share one token.
// This conservatively includes both Arc reference counts without retaining
// historical source payloads. Allocator bookkeeping is not included.
const SOURCE_BYTES: usize =
    std::mem::size_of::<CameraSourceKey>() + std::mem::size_of::<[usize; 2]>();

#[derive(Debug)]
struct CameraTile {
    index: usize,
    rectangle: (u32, u32, u32, u32),
    sources: Vec<CameraSourceKey>,
    pixels: Vec<u8>,
    valid: bool,
    reused: bool,
}

impl CameraTile {
    fn key_bytes(&self) -> usize {
        self.sources.capacity().saturating_mul(SOURCE_BYTES)
    }

    fn payload_bytes(&self) -> usize {
        self.key_bytes().saturating_add(self.pixels.capacity())
    }

    fn clear_payload(&mut self) {
        self.sources = Vec::new();
        self.pixels = Vec::new();
        self.valid = false;
        self.reused = false;
    }

    fn prepare(
        &mut self,
        commands: usize,
        sources: impl Iterator<Item = CameraSourceKey>,
        pixel_bytes: usize,
        allowance: usize,
    ) -> bool {
        // Dirty payloads cease to be reusable before any fallible allocation.
        // They become valid only after the entire frame succeeds.
        self.valid = false;
        self.reused = false;
        self.sources.clear();
        if self.sources.try_reserve_exact(commands).is_err()
            || self
                .pixels
                .try_reserve_exact(pixel_bytes.saturating_sub(self.pixels.len()))
                .is_err()
            || self.payload_bytes() > allowance
        {
            return false;
        }
        self.sources.extend(sources);
        self.pixels.resize(pixel_bytes, 0);
        self.sources.len() == commands
    }
}

#[derive(Debug)]
struct CameraClosure {
    camera: Camera,
    tiling: Tiling,
}

/// A sorted sparse table avoids unaccounted hash-table buckets. Retained and
/// pending entries share the same allocations and the same byte allowance.
#[derive(Debug)]
pub(super) struct CameraTileCache {
    tiles: Vec<CameraTile>,
    closure: Option<CameraClosure>,
    max_bytes: usize,
    stats: CacheStats,
    pending_stats: CacheStats,
}

impl CameraTileCache {
    pub(super) fn new(max_bytes: usize) -> Self {
        Self {
            tiles: Vec::new(),
            closure: None,
            max_bytes,
            stats: CacheStats::default(),
            pending_stats: CacheStats::default(),
        }
    }

    pub(super) fn clear(&mut self) {
        self.tiles = Vec::new();
        self.closure = None;
        self.stats = CacheStats::default();
        self.pending_stats = CacheStats::default();
    }

    pub(super) fn set_budget(&mut self, max_bytes: usize) {
        if self.max_bytes != max_bytes {
            self.clear();
            self.max_bytes = max_bytes;
        }
    }

    pub(super) fn active(&self) -> bool {
        self.closure.is_some()
    }

    pub(super) fn len(&self) -> usize {
        self.tiles.len()
    }

    pub(super) fn stats(&self) -> CacheStats {
        self.stats
    }

    pub(super) fn key_bytes(&self) -> usize {
        self.tiles.iter().fold(
            self.tiles
                .capacity()
                .saturating_mul(std::mem::size_of::<CameraTile>()),
            |bytes, tile| bytes.saturating_add(tile.key_bytes()),
        )
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.tiles.iter().fold(self.key_bytes(), |bytes, tile| {
            bytes.saturating_add(tile.pixels.capacity())
        })
    }

    fn discard(&mut self, index: Result<usize, usize>, used: &mut usize) {
        if let Ok(index) = index {
            let tile = &mut self.tiles[index];
            *used = used.saturating_sub(tile.payload_bytes());
            // Keep the original sorted slot in place until preparation ends.
            // No admitted tile has an empty source run, so this is a tombstone.
            tile.clear_payload();
        }
    }

    pub(super) fn prepare(
        &mut self,
        projection: &PreparedThreeDProjection,
        sources: &[CameraSourceKey],
    ) -> Result<(), PixelTileCacheError> {
        self.stats = CacheStats::default();
        self.pending_stats = CacheStats::default();
        let camera = projection.camera();
        let tiling = projection.tiling();
        if !self.closure.as_ref().is_some_and(|previous| {
            previous.tiling == tiling && same_projection_camera(&previous.camera, camera)
        }) {
            self.tiles = Vec::new();
            self.closure = Some(CameraClosure {
                camera: camera.clone(),
                tiling,
            });
        }
        for tile in &mut self.tiles {
            tile.reused = false;
        }
        let result = self.prepare_tiles(projection, sources);
        // Compact once, including after a fallible preparation. Earlier slots
        // never move during lookup; newly admitted indices only append.
        self.tiles.retain(|tile| !tile.sources.is_empty());
        if self
            .tiles
            .windows(2)
            .any(|tiles| tiles[0].index > tiles[1].index)
        {
            self.tiles.sort_unstable_by_key(|tile| tile.index);
        }
        debug_assert!(self.retained_bytes() <= self.max_bytes);
        result
    }

    fn prepare_tiles(
        &mut self,
        projection: &PreparedThreeDProjection,
        sources: &[CameraSourceKey],
    ) -> Result<(), PixelTileCacheError> {
        let (width, height) = projection.camera().pixel_shape();
        let edge = projection.tiling().fine_tile.max(1);
        let columns = width.div_ceil(edge);
        let tile_count = usize::try_from(columns)
            .ok()
            .and_then(|columns| {
                columns.checked_mul(usize::try_from(height.div_ceil(edge)).ok()?)
            })
            .ok_or(PixelTileCacheError::TooLarge)?;
        // Disabled or impossibly small budgets must not scan scene draws.
        if self.max_bytes < std::mem::size_of::<CameraTile>() + SOURCE_BYTES + 8 {
            self.tiles = Vec::new();
            self.pending_stats.poisoned = tile_count;
            return Ok(());
        }
        let mut used = self.retained_bytes();
        let original_len = self.tiles.len();
        let mut final_growth = false;
        for index in 0..tile_count {
            let rectangle = tile_rect(index, columns, edge, width, height)?;
            let (x, y, w, h) = rectangle;
            let draws = projection.tile_draws([x, y, x + w, y + h]);
            let commands = draws.clone().count();
            let slot = self.tiles[..original_len]
                .binary_search_by_key(&index, |tile| tile.index);
            if commands == 0 {
                self.discard(slot, &mut used);
                self.pending_stats.empty += 1;
                continue;
            }
            if draws
                .clone()
                .any(|draw| sources.get(draw).is_none_or(|source| source.volatile))
            {
                self.discard(slot, &mut used);
                self.pending_stats.poisoned += 1;
                continue;
            }
            if let Ok(slot) = slot {
                let tile = &mut self.tiles[slot];
                if tile.valid
                    && tile.sources.len() == commands
                    && tile
                        .sources
                        .iter()
                        .zip(draws.clone())
                        .all(|(before, draw)| before.matches(&sources[draw]))
                {
                    tile.reused = true;
                    self.pending_stats.hits += 1;
                    continue;
                }
            }
            let pixel_bytes = rgba16f_row_bytes(w)?
                .checked_mul(usize::try_from(h).map_err(|_| PixelTileCacheError::TooLarge)?)
                .ok_or(PixelTileCacheError::TooLarge)?;
            let old_bytes = slot
                .ok()
                .map_or(0, |slot| self.tiles[slot].payload_bytes());
            let slot_bytes = if slot.is_err() && self.tiles.len() == self.tiles.capacity() {
                std::mem::size_of::<CameraTile>()
            } else {
                0
            };
            let required = commands
                .checked_mul(SOURCE_BYTES)
                .and_then(|keys| keys.checked_add(pixel_bytes))
                .and_then(|payload| payload.checked_add(slot_bytes));
            let available = self.max_bytes.saturating_sub(used.saturating_sub(old_bytes));
            if required.is_none_or(|required| required > available) {
                self.discard(slot, &mut used);
                self.pending_stats.poisoned += 1;
                continue;
            }
            let position = match slot {
                Ok(position) => position,
                Err(_) => {
                    let before_capacity = self.tiles.capacity();
                    if self.tiles.len() == before_capacity {
                        if final_growth {
                            self.pending_stats.poisoned += 1;
                            continue;
                        }
                        let desired_growth = before_capacity.max(1);
                        let affordable_growth = self
                            .max_bytes
                            .saturating_sub(used)
                            .saturating_sub(commands.saturating_mul(SOURCE_BYTES))
                            .saturating_sub(pixel_bytes)
                            / std::mem::size_of::<CameraTile>();
                        let growth = desired_growth
                            .min(affordable_growth)
                            .min(tile_count.saturating_sub(before_capacity));
                        // Geometric growth bounds metadata copies. Once only a
                        // partial growth fits, do not repeatedly grow by one
                        // after later tiles release payloads in this frame.
                        final_growth = growth < desired_growth;
                        if growth == 0 || self.tiles.try_reserve_exact(growth).is_err() {
                            final_growth = true;
                            self.pending_stats.poisoned += 1;
                            continue;
                        }
                        used = used.saturating_add(
                            self.tiles
                                .capacity()
                                .saturating_sub(before_capacity)
                                .saturating_mul(std::mem::size_of::<CameraTile>()),
                        );
                        // reserve_exact may report more capacity than requested.
                        // Refuse the complete cache rather than exceed its bound.
                        if used > self.max_bytes {
                            self.tiles = Vec::new();
                            self.pending_stats = CacheStats {
                                poisoned: tile_count,
                                ..CacheStats::default()
                            };
                            return Ok(());
                        }
                    }
                    let position = self.tiles.len();
                    self.tiles.push(CameraTile {
                        index,
                        rectangle,
                        sources: Vec::new(),
                        pixels: Vec::new(),
                        valid: false,
                        reused: false,
                    });
                    position
                }
            };
            let allowance = self.max_bytes.saturating_sub(used.saturating_sub(old_bytes));
            let tile = &mut self.tiles[position];
            if tile.prepare(
                commands,
                draws.map(|draw| sources[draw].clone()),
                pixel_bytes,
                allowance,
            ) {
                used = used
                    .saturating_sub(old_bytes)
                    .saturating_add(tile.payload_bytes());
                self.pending_stats.misses += 1;
            } else {
                self.tiles[position].clear_payload();
                used = used.saturating_sub(old_bytes);
                self.pending_stats.poisoned += 1;
            }
        }
        Ok(())
    }

    /// Restore directly into the worker's local band (or an inline destination
    /// band). Returning true means no coverage, lighting, depth or AA work runs.
    pub(super) fn restore_tile(
        &self,
        index: usize,
        destination: &mut [u8],
        stride: usize,
    ) -> bool {
        let Ok(slot) = self.tiles.binary_search_by_key(&index, |tile| tile.index) else {
            return false;
        };
        let tile = &self.tiles[slot];
        if !tile.valid || !tile.reused {
            return false;
        }
        let (x, _, width, height) = tile.rectangle;
        let row_bytes = width as usize * 8;
        for row in 0..height as usize {
            let source = row * row_bytes;
            let target = row * stride + x as usize * 8;
            destination[target..target + row_bytes]
                .copy_from_slice(&tile.pixels[source..source + row_bytes]);
        }
        true
    }

    pub(super) fn complete(
        &mut self,
        frame: &FrameBuffer,
    ) -> Result<(), PixelTileCacheError> {
        let stride = frame.layout().stride(0);
        let plane = frame.plane(0);
        // Validate every read and payload before changing any checkpoint. The
        // successful raster's destination was validated before worker dispatch.
        for tile in &self.tiles {
            if tile.reused {
                continue;
            }
            let (x, y, width, height) = tile.rectangle;
            let row_bytes = rgba16f_row_bytes(width)?;
            let expected = row_bytes
                .checked_mul(height as usize)
                .ok_or(PixelTileCacheError::TooLarge)?;
            if tile.pixels.len() != expected {
                return Err(PixelTileCacheError::PayloadShape {
                    tile: tile.index,
                    expected,
                    actual: tile.pixels.len(),
                });
            }
            let end = frame_row_start(y, height as usize - 1, stride, x)?
                .checked_add(row_bytes)
                .ok_or(PixelTileCacheError::TooLarge)?;
            if end > plane.len() {
                return Err(PixelTileCacheError::PayloadShape {
                    tile: tile.index,
                    expected: end,
                    actual: plane.len(),
                });
            }
        }
        for tile in &mut self.tiles {
            if tile.reused {
                continue;
            }
            let (x, y, width, height) = tile.rectangle;
            let row_bytes = width as usize * 8;
            for row in 0..height as usize {
                let source = (y as usize + row) * stride + x as usize * 8;
                let target = row * row_bytes;
                tile.pixels[target..target + row_bytes]
                    .copy_from_slice(&plane[source..source + row_bytes]);
            }
        }
        // A failed or interrupted render never reaches this publication point.
        for tile in &mut self.tiles {
            tile.valid = true;
        }
        self.stats = self.pending_stats;
        Ok(())
    }
}
