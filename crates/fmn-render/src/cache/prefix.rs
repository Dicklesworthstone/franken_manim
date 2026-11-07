//! Exact accumulator checkpoints for unchanged leading tile commands.
//!
//! A completed RGBA16F tile cannot be used as a compositing checkpoint: decoding
//! it loses the f64 accumulator bits before the next source-over operation.
//! These checkpoints retain that accumulator, including the background, and
//! resume the original painter-ordered command run without regrouping it.
//! Adaptive AA also retains cumulative boundary counts. Its complex pixels
//! still supersample the complete command run; a resolved prefix is never
//! composited with a supersampled tail.

use super::{OutputTransform, TileKey, TileWork, tile_rect};
use crate::{Binning, RenderPlan};
use fmn_core::color::PremulRgba;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

/// Work actually avoided or retained during a successful cached render.
///
/// Whole-tile hits are counted separately by [`super::CacheStats`]. A prefix
/// hit means every row resumed an exact accumulator and skipped the leading
/// commands' native-resolution coverage, stroke and compositing work. Adaptive
/// AA may still rerasterize those commands at complex pixels' subpixel samples.
/// Failed renders publish no prefix counters or newly captured checkpoints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PrefixCacheStats {
    /// Dirty tiles that resumed a previously completed prefix checkpoint.
    pub hits: usize,
    /// Newly completed checkpoints admitted after all raster workers succeed.
    pub captures: usize,
    /// Native-pass painter commands skipped, once per resumed tile (not per row).
    pub commands_skipped: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PrefixMode {
    Canonical,
    Adaptive,
}

pub(crate) struct PrefixRow<'a> {
    pub pixels: &'a [PremulRgba],
    pub edges: Option<&'a [u8]>,
    pub commands: usize,
}

#[derive(Clone, Copy)]
pub(super) struct PrefixInputs<'a> {
    pub binning: &'a Binning,
    pub plan: &'a RenderPlan,
    pub camera: u64,
    pub output: OutputTransform,
}

#[derive(Debug, Default)]
struct PrefixPixels {
    pixels: Vec<PremulRgba>,
    edges: Vec<u8>,
}

impl PrefixPixels {
    fn retained_bytes(&self) -> usize {
        self.pixels
            .capacity()
            .saturating_mul(std::mem::size_of::<PremulRgba>())
            .saturating_add(self.edges.capacity())
    }
}

#[derive(Debug)]
struct PendingPixels {
    state: PrefixPixels,
    rows: usize,
    valid: bool,
}

#[derive(Debug)]
enum Pixels {
    Ready(PrefixPixels),
    Pending(Mutex<PendingPixels>),
}

#[derive(Debug)]
struct PrefixTile {
    key: TileKey,
    width: usize,
    height: usize,
    pixels: Pixels,
    reuse: bool,
    rows_reused: AtomicUsize,
}

impl PrefixTile {
    fn retained_bytes(&self) -> usize {
        let bytes = match &self.pixels {
            Pixels::Ready(state) => state.retained_bytes(),
            Pixels::Pending(pixels) => pixels
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .state
                .retained_bytes(),
        };
        self.key
            .retained_bytes()
            .saturating_add(bytes)
            .saturating_add(std::mem::size_of::<(usize, Self)>())
    }
}

/// One checkpoint per admitted tile, with a deterministic byte budget.
///
/// Captures are lazy: a changed tile first exposes its unchanged leading
/// commands, then its next changed frame can reuse the completed checkpoint.
/// No animation annotation or consecutive frame sequence is assumed. A live
/// writable tail may expose a safe immutable prefix immediately.
#[derive(Debug)]
pub(super) struct PrefixCache {
    tiles: HashMap<usize, PrefixTile>,
    max_bytes: usize,
    remaining_bytes: usize,
    mode: Option<PrefixMode>,
    stats: PrefixCacheStats,
}

impl PrefixCache {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            tiles: HashMap::new(),
            max_bytes,
            remaining_bytes: max_bytes,
            mode: None,
            stats: PrefixCacheStats::default(),
        }
    }

    pub fn clear(&mut self) {
        self.tiles.clear();
        self.stats = PrefixCacheStats::default();
    }

    pub fn stats(&self) -> PrefixCacheStats {
        self.stats
    }

    pub fn retained_bytes(&self) -> usize {
        self.tiles.values().fold(0usize, |total, tile| {
            total.saturating_add(tile.retained_bytes())
        })
    }

    pub fn begin_frame(&mut self, tile_count: usize, mode: Option<PrefixMode>) {
        self.stats = PrefixCacheStats::default();
        let mode = mode.filter(|_| self.max_bytes != 0);
        if self.mode != mode {
            self.tiles.clear();
            self.mode = mode;
        }
        if self.mode.is_none() {
            self.tiles.clear();
            self.remaining_bytes = self.max_bytes;
            return;
        }
        self.tiles.retain(|&index, tile| {
            tile.reuse = false;
            *tile.rows_reused.get_mut() = 0;
            // A failed/interrupted render never promotes its partial captures.
            index < tile_count && matches!(tile.pixels, Pixels::Ready(_))
        });
        self.remaining_bytes = self.max_bytes.saturating_sub(self.retained_bytes());
    }

    pub fn prepare_tile(
        &mut self,
        tile: usize,
        work: &TileWork,
        unchanged_prefix: Option<usize>,
        inputs: PrefixInputs<'_>,
    ) {
        if self.mode.is_none() {
            return;
        }
        let previous = self.tiles.remove(&tile);
        if let Some(previous) = &previous {
            self.remaining_bytes = self
                .remaining_bytes
                .saturating_add(previous.retained_bytes());
        }
        if matches!(work, TileWork::Empty) {
            return;
        }
        if matches!(work, TileWork::Reuse) {
            if let Some(previous) = previous {
                self.admit(tile, previous);
            }
            return;
        }
        let PrefixInputs {
            binning,
            plan,
            camera,
            output,
        } = inputs;
        let previous_common = previous.as_ref().and_then(|previous| {
            previous
                .key
                .common_prefix_len(binning, plan, tile, camera, output)
        });
        if let Some(mut previous) = previous {
            if previous_common == Some(previous.key.command_words.len()) {
                previous.reuse = true;
                self.admit(tile, previous);
                return;
            }
            self.prepare_capture(
                tile,
                unchanged_prefix.or(previous_common),
                inputs,
                Some(previous),
            );
        } else {
            self.prepare_capture(tile, unchanged_prefix, inputs, None);
        }
    }

    fn prepare_capture(
        &mut self,
        tile: usize,
        unchanged_prefix: Option<usize>,
        inputs: PrefixInputs<'_>,
        previous: Option<PrefixTile>,
    ) {
        let PrefixInputs {
            binning,
            plan,
            camera,
            output,
        } = inputs;
        let Some(draws) = binning.tile(tile) else {
            return;
        };
        let commands = unchanged_prefix.unwrap_or_else(|| {
            // Full tile keys deliberately refuse live views. Their first
            // volatile command still defines a safe preceding checkpoint.
            draws
                .iter()
                .position(|&draw| {
                    plan.shapes()
                        .instances()
                        .get(draw as usize)
                        .is_none_or(|instance| instance.volatile)
                })
                .unwrap_or(0)
        });
        if commands == 0 || commands > draws.len() {
            return;
        }
        let viewport = binning.viewport();
        let edge = binning.tiling().fine_tile.max(1);
        let cols = viewport.width.div_ceil(edge);
        let Ok((_, _, width, height)) =
            tile_rect(tile, cols, edge, viewport.width, viewport.height)
        else {
            return;
        };
        let (Ok(width), Ok(height)) = (usize::try_from(width), usize::try_from(height)) else {
            return;
        };
        let Some(cells) = width.checked_mul(height) else {
            return;
        };
        let adaptive = self.mode == Some(PrefixMode::Adaptive);
        let pixel_bytes = std::mem::size_of::<PremulRgba>() + usize::from(adaptive);
        let key_cell_bytes =
            std::mem::size_of::<[u64; 15]>() + std::mem::size_of::<crate::revision::Revisions>();
        let required = commands
            .checked_mul(key_cell_bytes)
            .and_then(|keys| cells.checked_mul(pixel_bytes)?.checked_add(keys))
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<(usize, PrefixTile)>()));
        if required.is_none_or(|bytes| bytes > self.remaining_bytes) {
            return;
        }
        let (previous_key, mut state) = match previous {
            Some(PrefixTile {
                key,
                pixels: Pixels::Ready(state),
                ..
            }) => (Some(key), state),
            _ => (None, PrefixPixels::default()),
        };
        let Some(key) = TileKey::build_prefix_reusing(
            previous_key,
            binning,
            plan,
            tile,
            camera,
            output,
            commands,
        ) else {
            return;
        };
        if state
            .pixels
            .try_reserve_exact(cells.saturating_sub(state.pixels.len()))
            .is_err()
        {
            return;
        }
        state.pixels.resize(
            cells,
            PremulRgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            },
        );
        if adaptive {
            if state
                .edges
                .try_reserve_exact(cells.saturating_sub(state.edges.len()))
                .is_err()
            {
                return;
            }
            state.edges.resize(cells, 0);
        }
        self.admit(
            tile,
            PrefixTile {
                key,
                width,
                height,
                pixels: Pixels::Pending(Mutex::new(PendingPixels {
                    state,
                    rows: 0,
                    valid: true,
                })),
                reuse: false,
                rows_reused: AtomicUsize::new(0),
            },
        );
    }

    fn admit(&mut self, index: usize, tile: PrefixTile) {
        let bytes = tile.retained_bytes();
        if bytes <= self.remaining_bytes && self.tiles.try_reserve(1).is_ok() {
            self.remaining_bytes -= bytes;
            self.tiles.insert(index, tile);
        }
    }

    pub fn row(&self, tile: usize, row: usize, width: usize) -> Option<PrefixRow<'_>> {
        let tile = self.tiles.get(&tile)?;
        if !tile.reuse || row >= tile.height || width != tile.width {
            return None;
        }
        let Pixels::Ready(state) = &tile.pixels else {
            return None;
        };
        let start = row.checked_mul(tile.width)?;
        let end = start.checked_add(tile.width)?;
        let pixels = state.pixels.get(start..end)?;
        let edges = if self.mode == Some(PrefixMode::Adaptive) {
            Some(state.edges.get(start..end)?)
        } else {
            None
        };
        tile.rows_reused.fetch_add(1, Ordering::Relaxed);
        Some(PrefixRow {
            pixels,
            edges,
            commands: tile.key.command_words.len(),
        })
    }

    pub fn capture_commands(&self, tile: usize) -> usize {
        self.tiles.get(&tile).map_or(0, |tile| {
            if matches!(tile.pixels, Pixels::Pending(_)) {
                tile.key.command_words.len()
            } else {
                0
            }
        })
    }

    pub fn capture_row(
        &self,
        tile: usize,
        row: usize,
        pixels: impl ExactSizeIterator<Item = PremulRgba>,
        edges: Option<&[u8]>,
    ) {
        let Some(tile) = self.tiles.get(&tile) else {
            return;
        };
        let Pixels::Pending(pending) = &tile.pixels else {
            return;
        };
        let Ok(mut pending) = pending.lock() else {
            return;
        };
        let edges_valid = match (self.mode, edges) {
            (Some(PrefixMode::Canonical), None) => true,
            (Some(PrefixMode::Adaptive), Some(edges)) => edges.len() == tile.width,
            _ => false,
        };
        if !pending.valid
            || row != pending.rows
            || row >= tile.height
            || pixels.len() != tile.width
            || !edges_valid
        {
            pending.valid = false;
            return;
        }
        let start = row * tile.width;
        for (target, pixel) in pending.state.pixels[start..start + tile.width]
            .iter_mut()
            .zip(pixels)
        {
            *target = pixel;
        }
        if let Some(edges) = edges {
            pending.state.edges[start..start + tile.width].copy_from_slice(edges);
        }
        pending.rows += 1;
    }

    pub fn complete_frame(&mut self) {
        let stats = &mut self.stats;
        self.tiles.retain(|_, tile| {
            if matches!(tile.pixels, Pixels::Ready(_)) {
                if tile.reuse && *tile.rows_reused.get_mut() == tile.height {
                    stats.hits = stats.hits.saturating_add(1);
                    stats.commands_skipped = stats
                        .commands_skipped
                        .saturating_add(tile.key.command_words.len());
                }
                return true;
            }
            let Pixels::Pending(pending) =
                std::mem::replace(&mut tile.pixels, Pixels::Ready(PrefixPixels::default()))
            else {
                return false;
            };
            let Ok(pending) = pending.into_inner() else {
                return false;
            };
            if !pending.valid || pending.rows != tile.height {
                return false;
            }
            tile.pixels = Pixels::Ready(pending.state);
            stats.captures = stats.captures.saturating_add(1);
            true
        });
    }
}
