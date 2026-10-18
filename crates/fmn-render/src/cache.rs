//! The retained compositor's tile cache (§10.8, D-20).
//!
//! > … completed tiles cached keyed on *(command list, resource revisions,
//! > camera revision, output transform)*; static layer runs above/below animated
//! > content cached as units. **Reused tiles are byte-identical, so reuse is
//! > PG-5-safe and works under `certified`.** A `wait()` frame reduces to clock
//! > advance plus reuse; a blinking cursor does not re-rasterize a 4K frame.
//!
//! ## The whole design is the key
//!
//! Everything interesting about a tile cache is whether its key is *complete* —
//! whether every input that could change a tile's bytes is in it. Get that wrong
//! and the failure is the worst kind available in this program: a stale tile
//! under `certified` is a wrong frame that is **bit-reproducibly wrong**, so
//! every determinism check agrees with it.
//!
//! So [`TileKey`] is built from exactly §10.8's four parts, and the adversarial
//! test in this module is one case per mutation class rather than a spot check.
//! The rule that makes it maintainable is that a key is never *assembled* by a
//! caller: [`TileKey::build`] reads the binning and the plan, so a new input
//! cannot be forgotten at one call site and remembered at another.
//!
//! ## A hit bypasses rasterization; it does not check it
//!
//! §10.8 is explicit, and it is worth restating because the lazy alternative is
//! so tempting: "a cache hit bypasses rasterization entirely rather than
//! re-rasterizing-and-comparing". A cache that rasterizes to validate itself has
//! no reason to exist, and — worse — it would make an incomplete key *look*
//! correct in testing while costing everything it was meant to save.
//!
//! ## Generic key discipline, concrete pixel reuse
//!
//! [`TileCache`] remains generic because key completeness is independent of an
//! engine's payload representation. [`PixelTileCache`] is the certified/fast CPU
//! binding: tightly packed `Rgba16F` fine tiles restored directly into a pooled
//! frame by [`crate::engine::FrameJob::render_into_cached`]. A hit therefore
//! bypasses coverage, stroke, compositing and writeback rather than recomputing
//! pixels and comparing them after the fact.
//!
//! ## Live views are never traded for reuse
//!
//! §8.2: "while a *writable* live view exists, the affected object … is
//! conservatively invalidated each frame — a live view never silently receives
//! weaker semantics to buy speed." A view can mutate points with no Stage method
//! called and therefore no revision bumped, so it cannot be folded into the
//! revision part of the key. It **poisons** the tile instead:
//! [`TileKey::build`] returns `None`, and a tile with no key can never hit.

use crate::bin::{Binning, ScreenMap, Viewport};
use crate::plan::RenderPlan;
use crate::revision::{Mixer, Revisions, mix};
use crate::table::Instance;
use fmn_frame::{FrameBuffer, PixelFormat};
use fmn_hash::Digest;
use std::collections::HashMap;

/// Everything about the output that can change a tile's bytes without changing
/// the scene.
///
/// §10.8's key names "output transform"; this is that, made concrete. The pixel
/// format is in it because a tile's *bytes* are what is cached — the same
/// coverage in RGBA8 and in P010 is not the same tile — and the viewport is in
/// it because a tile's pixel coordinates come from the grid the viewport
/// defines.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutputTransform {
    /// The surface the frame is being rendered into.
    pub viewport: Viewport,
    /// The object→screen mapping.
    pub map: ScreenMap,
    /// An opaque identifier for the destination pixel format.
    ///
    /// Opaque because `fmn_frame::PixelFormat` is Reel's to enumerate and this
    /// crate must not fork that enumeration; a caller passes its discriminant.
    pub pixel_format: u32,
}

impl OutputTransform {
    /// The exact output-transform inputs, preserving every floating-point bit.
    fn words(&self) -> [u64; 7] {
        [
            u64::from(self.viewport.width),
            u64::from(self.viewport.height),
            self.map.scale.to_bits(),
            self.map.origin[0].to_bits(),
            self.map.origin[1].to_bits(),
            u64::from(self.map.y_up),
            u64::from(self.pixel_format),
        ]
    }

    /// A diagnostic fingerprint, never sufficient to authorize pixel reuse.
    fn fold(&self) -> u64 {
        mix(&self.words())
    }
}

/// §10.8's tile-cache key, including the inputs behind its four fingerprints.
///
/// The public fingerprints diagnose misses. Reuse compares the ordered command
/// words, all seven revision axes, the retained-table epoch and the exact output
/// transform; equal short hashes alone never authorize cached pixels.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TileKey {
    /// A fingerprint of the tile's command run and its class words.
    pub commands: u64,
    /// A fingerprint of every listed instance's seven revision axes.
    pub revisions: u64,
    /// The camera revision.
    pub camera: u64,
    /// A fingerprint of the output transform.
    pub output: u64,
    command_words: Vec<[u64; 15]>,
    revision_axes: Vec<Revisions>,
    output_words: [u64; 7],
    epoch: u64,
}

fn command_words(instance: &Instance, flags: u32) -> [u64; 15] {
    let mut words = [0; 15];
    words[0] = u64::from(instance.shape);
    words[1] = u64::from(instance.style);
    words[2] = u64::from(flags);
    let coefficients: [f64; 12] = instance.placement.coefficients();
    for (word, coefficient) in words[3..].iter_mut().zip(coefficients) {
        *word = coefficient.to_bits();
    }
    words
}

impl TileKey {
    /// Build the key for one fine tile, or `None` if the tile is uncacheable.
    ///
    /// Writable live views, stale binning, a mismatched output transform and
    /// insufficient key storage all refuse caching. The caller must rasterize
    /// without storing a result under an incomplete key.
    #[must_use]
    pub fn build(
        binning: &Binning,
        plan: &RenderPlan,
        tile: usize,
        camera: u64,
        output: OutputTransform,
    ) -> Option<TileKey> {
        Self::build_reusing(None, binning, plan, tile, camera, output)
    }

    fn retained_bytes(&self) -> usize {
        self.command_words
            .capacity()
            .saturating_mul(std::mem::size_of::<[u64; 15]>())
            .saturating_add(
                self.revision_axes
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Revisions>()),
            )
    }

    fn build_reusing(
        previous: Option<TileKey>,
        binning: &Binning,
        plan: &RenderPlan,
        tile: usize,
        camera: u64,
        output: OutputTransform,
    ) -> Option<TileKey> {
        if tile >= binning.tile_count()
            || !binning.matches_plan(plan)
            || binning.viewport() != output.viewport
            || binning.map() != output.map
        {
            return None;
        }
        let draws = binning.tile(tile)?;
        let flags = binning.tile_flags(tile)?;
        let instances = plan.shapes().instances();
        let mut key = previous.unwrap_or_else(|| TileKey {
            commands: 0,
            revisions: 0,
            camera,
            output: 0,
            command_words: Vec::new(),
            revision_axes: Vec::new(),
            output_words: output.words(),
            epoch: plan.epoch().get(),
        });
        key.command_words.clear();
        key.revision_axes.clear();
        key.command_words.try_reserve_exact(draws.len()).ok()?;
        key.revision_axes.try_reserve_exact(draws.len()).ok()?;

        let mut commands = Mixer::new();
        let mut revisions = Mixer::new();
        for (index, &draw) in draws.iter().enumerate() {
            let instance = instances.get(draw as usize)?;
            if instance.volatile {
                return None;
            }
            // Instance indices may shift when an unrelated object is inserted.
            // Shape/style rows remain stable within the retained-table epoch.
            let words = command_words(instance, *flags.get(index)?);
            for word in words {
                commands.write_u64(word);
            }
            revisions.write_u64(instance.revisions.fold());
            key.command_words.push(words);
            key.revision_axes.push(instance.revisions);
        }
        key.commands = commands.finish();
        key.revisions = revisions.finish();
        key.camera = camera;
        key.output = output.fold();
        key.output_words = output.words();
        key.epoch = plan.epoch().get();
        Some(key)
    }

    /// Compare borrowed inputs directly: a warm hit allocates and hashes nothing.
    fn matches_inputs(
        &self,
        binning: &Binning,
        plan: &RenderPlan,
        tile: usize,
        camera: u64,
        output: OutputTransform,
    ) -> bool {
        if tile >= binning.tile_count()
            || !binning.matches_plan(plan)
            || binning.viewport() != output.viewport
            || binning.map() != output.map
            || self.camera != camera
            || self.output_words != output.words()
            || self.epoch != plan.epoch().get()
        {
            return false;
        }
        let (Some(draws), Some(flags)) = (binning.tile(tile), binning.tile_flags(tile)) else {
            return false;
        };
        if draws.len() != self.command_words.len() || draws.len() != self.revision_axes.len() {
            return false;
        }
        let instances = plan.shapes().instances();
        draws.iter().enumerate().all(|(index, &draw)| {
            let (Some(instance), Some(&flags)) = (instances.get(draw as usize), flags.get(index))
            else {
                return false;
            };
            !instance.volatile
                && self.command_words[index] == command_words(instance, flags)
                && self.revision_axes[index] == instance.revisions
        })
    }

    /// Which input part differs from `other`, even when its fingerprints collide.
    #[must_use]
    pub fn diff(&self, other: &TileKey) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.command_words != other.command_words || self.epoch != other.epoch {
            out.push("commands");
        }
        if self.revision_axes != other.revision_axes {
            out.push("revisions");
        }
        if self.camera != other.camera {
            out.push("camera");
        }
        if self.output_words != other.output_words {
            out.push("output");
        }
        out
    }
}

/// What a frame's compositing did and did not have to rasterize.
///
/// §17.2 requires the perf rig to record "resource-reuse and tile-cache hit
/// rates, dirty-tile percentage". These are those counters, and they are also
/// what makes the `wait()` fast path a *measured* claim: a frame that changed
/// nothing must report every tile a hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStats {
    /// Tiles whose contents were reused.
    pub hits: usize,
    /// Tiles that had to be rasterized.
    pub misses: usize,
    /// Tiles that could not be cached: live views, invalid inputs or insufficient key storage.
    pub poisoned: usize,
    /// Tiles with no commands — nothing to rasterize and nothing to cache.
    pub empty: usize,
}

impl CacheStats {
    /// Hits over tiles that were eligible for a hit.
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        let eligible = self.hits + self.misses;
        if eligible == 0 {
            return 0.0;
        }
        self.hits as f64 / eligible as f64
    }

    /// The dirty-tile percentage §17.2 asks for.
    #[must_use]
    pub fn dirty_fraction(&self) -> f64 {
        let total = self.hits + self.misses + self.poisoned;
        if total == 0 {
            return 0.0;
        }
        (self.misses + self.poisoned) as f64 / total as f64
    }
}

/// A retained tile cache over an engine-supplied payload.
#[derive(Debug, Clone, Default)]
pub struct TileCache<T> {
    // A dirty tile lends its key buffers to pending work. Its old payload may
    // be recycled, but cannot hit again until a completed raster is stored.
    entries: HashMap<usize, (Option<TileKey>, T)>,
    stats: CacheStats,
}

/// What compositing one tile needs to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TileWork {
    /// Nothing is drawn here; the tile is background.
    Empty,
    /// Reuse the cached contents. Byte-identical by construction: the payload is
    /// handed back unchanged, never recomputed and compared.
    Reuse,
    /// Rasterize, then offer the result to [`TileCache::store`].
    Rasterize(TileKey),
    /// Rasterize, and do **not** store — the tile is uncacheable this frame.
    RasterizeUncached,
}

/// A tile-cache planning request that does not name a tile in its binning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TilePlanError {
    /// The requested tile index.
    pub tile: usize,
    /// The number of tiles available in the supplied binning.
    pub tile_count: usize,
}

impl std::fmt::Display for TilePlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "tile {} is outside a binning with {} tiles",
            self.tile, self.tile_count
        )
    }
}

impl std::error::Error for TilePlanError {}

impl<T> TileCache<T> {
    /// An empty cache.
    #[must_use]
    pub fn new() -> TileCache<T> {
        TileCache {
            entries: HashMap::new(),
            stats: CacheStats::default(),
        }
    }

    /// Begin a frame: zero the counters.
    ///
    /// The entries survive; that is the whole point. This resets only what is
    /// reported, so a per-frame hit rate means what it says.
    pub fn begin_frame(&mut self) {
        self.stats = CacheStats::default();
    }

    /// Decide what one tile needs, updating the counters.
    ///
    /// Does **not** touch the payload on a hit — §10.8's "a cache hit bypasses
    /// rasterization entirely rather than re-rasterizing-and-comparing" is a
    /// property of this function returning [`TileWork::Reuse`] without the caller
    /// ever producing a second version to compare against.
    ///
    /// # Errors
    /// Returns [`TilePlanError`] before reading or mutating cache state when
    /// `tile` is outside `binning`.
    pub fn plan_tile(
        &mut self,
        binning: &Binning,
        plan: &RenderPlan,
        tile: usize,
        camera: u64,
        output: OutputTransform,
    ) -> Result<TileWork, TilePlanError> {
        self.plan_tile_with_key_budget(binning, plan, tile, camera, output, usize::MAX)
    }

    fn plan_tile_with_key_budget(
        &mut self,
        binning: &Binning,
        plan: &RenderPlan,
        tile: usize,
        camera: u64,
        output: OutputTransform,
        max_key_bytes: usize,
    ) -> Result<TileWork, TilePlanError> {
        let tile_count = binning.tile_count();
        if tile >= tile_count {
            return Err(TilePlanError { tile, tile_count });
        }
        let Some(draws) = binning.tile(tile) else {
            return Err(TilePlanError { tile, tile_count });
        };
        if draws.is_empty() {
            self.entries.remove(&tile);
            self.stats.empty += 1;
            return Ok(TileWork::Empty);
        }
        let required = draws
            .len()
            .checked_mul(std::mem::size_of::<[u64; 15]>() + std::mem::size_of::<Revisions>());
        if required.is_none_or(|bytes| bytes > max_key_bytes) {
            self.entries.remove(&tile);
            self.stats.poisoned += 1;
            return Ok(TileWork::RasterizeUncached);
        }
        if self.entries.get(&tile).is_some_and(|(cached, _)| {
            cached.as_ref().is_some_and(|key| {
                key.retained_bytes() <= max_key_bytes
                    && key.matches_inputs(binning, plan, tile, camera, output)
            })
        }) {
            self.stats.hits += 1;
            return Ok(TileWork::Reuse);
        }

        let previous = self
            .entries
            .get_mut(&tile)
            .and_then(|(key, _)| key.take())
            .filter(|key| key.retained_bytes() <= max_key_bytes);
        let Some(key) = TileKey::build_reusing(previous, binning, plan, tile, camera, output)
            .filter(|key| key.retained_bytes() <= max_key_bytes)
        else {
            self.entries.remove(&tile);
            self.stats.poisoned += 1;
            return Ok(TileWork::RasterizeUncached);
        };
        self.stats.misses += 1;
        Ok(TileWork::Rasterize(key))
    }

    /// Store a freshly rasterized tile under the key [`plan_tile`] returned.
    ///
    /// Taking the key rather than recomputing it is what stops a store keyed on
    /// a state the payload was not rendered from — the one way a complete key
    /// can still cache a stale tile.
    ///
    /// [`plan_tile`]: TileCache::plan_tile
    pub fn store(&mut self, tile: usize, key: TileKey, payload: T) {
        self.entries.insert(tile, (Some(key), payload));
    }

    /// The cached payload for a tile, if any.
    #[must_use]
    pub fn get(&self, tile: usize) -> Option<&T> {
        self.entries.get(&tile).map(|(_, payload)| payload)
    }

    /// The key a tile is currently cached under.
    #[must_use]
    pub fn key(&self, tile: usize) -> Option<TileKey> {
        self.entries.get(&tile).and_then(|(key, _)| key.clone())
    }

    /// This frame's counters.
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        self.stats
    }

    /// How many tiles are retained.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Is the cache empty?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Forget everything — a resolution change, an engine swap, a new scene.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Remove one retained payload so a concrete cache can reuse its storage
    /// under a replacement key.
    fn take_payload(&mut self, tile: usize) -> Option<T> {
        self.entries.remove(&tile).map(|(_, payload)| payload)
    }
}

/// A failure while planning or retaining real `Rgba16F` tile bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PixelTileCacheError {
    /// The cache planner was asked for a tile outside the supplied binning.
    Plan(TilePlanError),
    /// Frame or tile geometry could not be represented in the host address
    /// space, or the retained work table could not reserve its bounded size.
    TooLarge,
    /// The engine attempted to consume a retained payload with the wrong
    /// dimensions. This is an internal invariant failure, never a cache miss.
    PayloadShape {
        /// Fine-tile index.
        tile: usize,
        /// Bytes required by the current tile rectangle.
        expected: usize,
        /// Bytes held by the retained payload.
        actual: usize,
    },
}

impl std::fmt::Display for PixelTileCacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plan(error) => error.fmt(f),
            Self::TooLarge => f.write_str("pixel tile cache exceeds the host address space"),
            Self::PayloadShape {
                tile,
                expected,
                actual,
            } => write!(
                f,
                "pixel tile {tile} retains {actual} bytes, expected {expected}"
            ),
        }
    }
}

impl std::error::Error for PixelTileCacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Plan(error) => Some(error),
            Self::TooLarge | Self::PayloadShape { .. } => None,
        }
    }
}

impl From<TilePlanError> for PixelTileCacheError {
    fn from(error: TilePlanError) -> Self {
        Self::Plan(error)
    }
}

/// Real retained-compositor storage for the CPU engine's raw `Rgba16F` tiles.
///
/// The generic [`TileCache`] owns the key discipline; this adapter owns the
/// missing pixel half: a retained work table and tightly packed tile payloads.
/// Payload allocations are recycled when a dirty tile is replaced, so after a
/// stable geometry warm-up the cache itself performs no per-frame allocation.
///
/// Exact command/revision storage retained after planning is bounded to 64 MiB
/// by default, independently of pixel payloads and cache metadata. Dense tiles
/// exceeding the remaining budget are rasterized uncached. Replacing keys can
/// temporarily hold old and new storage during planning.
#[derive(Debug)]
pub struct PixelTileCache {
    tiles: TileCache<Vec<u8>>,
    work: Vec<TileWork>,
    render_state: Option<Digest>,
    max_key_bytes: usize,
    // Bumped when a render starts and again when it succeeds, so only the
    // surface of the last *completed* render can present the current value.
    serial: u64,
}

impl Default for PixelTileCache {
    fn default() -> Self {
        Self::with_key_budget_bytes(64 * 1024 * 1024)
    }
}

impl PixelTileCache {
    /// An empty cache with a bound on retained exact command/revision storage.
    ///
    /// This does not bound pixel payloads or instantaneous allocation peaks
    /// while replacing old keys. A zero budget disables non-empty tile reuse.
    #[must_use]
    pub fn with_key_budget_bytes(max_key_bytes: usize) -> Self {
        Self {
            tiles: TileCache::new(),
            work: Vec::new(),
            render_state: None,
            max_key_bytes,
            serial: 0,
        }
    }

    /// Bytes of exact command/revision storage, including pending raster work.
    #[must_use]
    pub fn retained_key_bytes(&self) -> usize {
        let retained = self
            .tiles
            .entries
            .values()
            .filter_map(|(key, _)| key.as_ref())
            .fold(0usize, |bytes, key| {
                bytes.saturating_add(key.retained_bytes())
            });
        self.work.iter().fold(retained, |bytes, work| match work {
            TileWork::Rasterize(key) => bytes.saturating_add(key.retained_bytes()),
            _ => bytes,
        })
    }

    /// An empty real-pixel cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// This frame's key-planning counters.
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        self.tiles.stats()
    }

    /// Number of retained non-empty tiles.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    /// Whether no tile payload is retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// Forget all pixels and renderer identity.
    pub fn clear(&mut self) {
        self.tiles.clear();
        self.work.clear();
        self.render_state = None;
        self.serial = self.serial.wrapping_add(1);
    }

    /// Token naming this cache's last completed render.
    ///
    /// A caller that keeps that render's surface untouched may pass the token
    /// to [`crate::engine::FrameJob::render_into_cached_resuming`]. Reused
    /// tiles are then already in place, so they are not copied back in. Any
    /// later render start, failure or [`Self::clear`] retires the token.
    #[must_use]
    pub fn serial(&self) -> u64 {
        self.serial
    }

    pub(crate) fn begin_render(&mut self, resume: Option<u64>) -> bool {
        let resumed = resume == Some(self.serial) && self.serial != 0;
        self.serial = self.serial.wrapping_add(1);
        resumed
    }

    pub(crate) fn complete_render(&mut self) {
        self.serial = self.serial.wrapping_add(1);
    }

    pub(crate) fn prepare_frame(
        &mut self,
        binning: &Binning,
        plan: &RenderPlan,
        camera: u64,
        render_state: Digest,
    ) -> Result<(), PixelTileCacheError> {
        // Background, AA policy, engine/tier, semantic renderer version and
        // tiling all affect raw bytes but are intentionally not scene
        // revisions. The engine's journal digest already names that complete
        // closure; changing any part invalidates every retained payload.
        if self.render_state != Some(render_state) {
            self.tiles.clear();
            self.render_state = Some(render_state);
        }

        self.tiles.begin_frame();
        self.work.clear();
        self.work
            .try_reserve(binning.tile_count())
            .map_err(|_| PixelTileCacheError::TooLarge)?;
        let output = OutputTransform {
            viewport: binning.viewport(),
            map: binning.map(),
            // A tile key lasts only for this process. The discriminant is
            // therefore the intended representation (see OutputTransform).
            pixel_format: transient_format_id(PixelFormat::Rgba16F),
        };
        let mut remaining_key_bytes = self.max_key_bytes;
        for tile in 0..binning.tile_count() {
            let work = self.tiles.plan_tile_with_key_budget(
                binning,
                plan,
                tile,
                camera,
                output,
                remaining_key_bytes,
            )?;
            let key_bytes = match &work {
                TileWork::Rasterize(key) => key.retained_bytes(),
                TileWork::Reuse => self
                    .tiles
                    .entries
                    .get(&tile)
                    .and_then(|(key, _)| key.as_ref())
                    .map_or(0, TileKey::retained_bytes),
                TileWork::Empty | TileWork::RasterizeUncached => 0,
            };
            remaining_key_bytes = remaining_key_bytes.saturating_sub(key_bytes);
            self.work.push(work);
        }
        Ok(())
    }

    pub(crate) fn work(&self, tile: usize) -> Option<&TileWork> {
        self.work.get(tile)
    }

    pub(crate) fn restore_reused_tiles(
        &self,
        frame: &mut FrameBuffer,
        binning: &Binning,
    ) -> Result<(), PixelTileCacheError> {
        let width = frame.layout().width();
        let height = frame.layout().height();
        let tile_edge = binning.tiling().fine_tile.max(1);
        let cols = width.div_ceil(tile_edge);
        let stride = frame.layout().stride(0);
        let plane_len = frame.plane(0).len();

        for tile in 0..self.work.len() {
            if self.work.get(tile) != Some(&TileWork::Reuse) {
                continue;
            }
            let (x0, y0, tile_width, tile_height) =
                tile_rect(tile, cols, tile_edge, width, height)?;
            let row_bytes = rgba16f_row_bytes(tile_width)?;
            let payload_len = row_bytes
                .checked_mul(
                    usize::try_from(tile_height).map_err(|_| PixelTileCacheError::TooLarge)?,
                )
                .ok_or(PixelTileCacheError::TooLarge)?;
            let payload = self
                .tiles
                .get(tile)
                .ok_or(PixelTileCacheError::PayloadShape {
                    tile,
                    expected: payload_len,
                    actual: 0,
                })?;
            if payload.len() != payload_len {
                return Err(PixelTileCacheError::PayloadShape {
                    tile,
                    expected: payload_len,
                    actual: payload.len(),
                });
            }
            for row in 0..usize::try_from(tile_height).map_err(|_| PixelTileCacheError::TooLarge)? {
                let source_start = row
                    .checked_mul(row_bytes)
                    .ok_or(PixelTileCacheError::TooLarge)?;
                let source_end = source_start
                    .checked_add(row_bytes)
                    .ok_or(PixelTileCacheError::TooLarge)?;
                let destination_start = frame_row_start(y0, row, stride, x0)?;
                let destination_end = destination_start
                    .checked_add(row_bytes)
                    .ok_or(PixelTileCacheError::TooLarge)?;
                let source = payload.get(source_start..source_end).ok_or(
                    PixelTileCacheError::PayloadShape {
                        tile,
                        expected: source_end,
                        actual: payload.len(),
                    },
                )?;
                let destination = frame
                    .plane_mut(0)
                    .get_mut(destination_start..destination_end)
                    .ok_or(PixelTileCacheError::PayloadShape {
                        tile,
                        expected: destination_end,
                        actual: plane_len,
                    })?;
                destination.copy_from_slice(source);
            }
        }
        Ok(())
    }

    pub(crate) fn retain_rasterized_tiles(
        &mut self,
        frame: &FrameBuffer,
        binning: &Binning,
    ) -> Result<(), PixelTileCacheError> {
        let width = frame.layout().width();
        let height = frame.layout().height();
        let tile_edge = binning.tiling().fine_tile.max(1);
        let cols = width.div_ceil(tile_edge);
        let stride = frame.layout().stride(0);
        let plane = frame.plane(0);

        for tile in 0..self.work.len() {
            if !matches!(self.work.get(tile), Some(TileWork::Rasterize(_))) {
                continue;
            }
            let (x0, y0, tile_width, tile_height) =
                tile_rect(tile, cols, tile_edge, width, height)?;
            let row_bytes = rgba16f_row_bytes(tile_width)?;
            let payload_len = row_bytes
                .checked_mul(
                    usize::try_from(tile_height).map_err(|_| PixelTileCacheError::TooLarge)?,
                )
                .ok_or(PixelTileCacheError::TooLarge)?;
            let mut payload = self.tiles.take_payload(tile).unwrap_or_default();
            payload
                .try_reserve(payload_len.saturating_sub(payload.len()))
                .map_err(|_| PixelTileCacheError::TooLarge)?;
            payload.resize(payload_len, 0);
            for row in 0..usize::try_from(tile_height).map_err(|_| PixelTileCacheError::TooLarge)? {
                let source_start = frame_row_start(y0, row, stride, x0)?;
                let source_end = source_start
                    .checked_add(row_bytes)
                    .ok_or(PixelTileCacheError::TooLarge)?;
                let destination_start = row
                    .checked_mul(row_bytes)
                    .ok_or(PixelTileCacheError::TooLarge)?;
                let destination_end = destination_start
                    .checked_add(row_bytes)
                    .ok_or(PixelTileCacheError::TooLarge)?;
                let source = plane.get(source_start..source_end).ok_or(
                    PixelTileCacheError::PayloadShape {
                        tile,
                        expected: source_end,
                        actual: plane.len(),
                    },
                )?;
                let payload_actual = payload.len();
                let destination = payload.get_mut(destination_start..destination_end).ok_or(
                    PixelTileCacheError::PayloadShape {
                        tile,
                        expected: destination_end,
                        actual: payload_actual,
                    },
                )?;
                destination.copy_from_slice(source);
            }
            let TileWork::Rasterize(key) =
                std::mem::replace(&mut self.work[tile], TileWork::RasterizeUncached)
            else {
                continue;
            };
            self.tiles.store(tile, key, payload);
        }
        Ok(())
    }
}

const fn transient_format_id(format: PixelFormat) -> u32 {
    match format {
        PixelFormat::Rgba8 => 0,
        PixelFormat::Bgra8 => 1,
        PixelFormat::Rgba16F => 2,
        PixelFormat::Nv12 => 3,
        PixelFormat::P010 => 4,
    }
}

fn rgba16f_row_bytes(width: u32) -> Result<usize, PixelTileCacheError> {
    usize::try_from(width)
        .map_err(|_| PixelTileCacheError::TooLarge)?
        .checked_mul(8)
        .ok_or(PixelTileCacheError::TooLarge)
}

fn tile_rect(
    tile: usize,
    cols: u32,
    tile_edge: u32,
    width: u32,
    height: u32,
) -> Result<(u32, u32, u32, u32), PixelTileCacheError> {
    let tile = u32::try_from(tile).map_err(|_| PixelTileCacheError::TooLarge)?;
    let tx = tile
        .checked_rem(cols)
        .ok_or(PixelTileCacheError::TooLarge)?;
    let ty = tile
        .checked_div(cols)
        .ok_or(PixelTileCacheError::TooLarge)?;
    let x0 = tx
        .checked_mul(tile_edge)
        .ok_or(PixelTileCacheError::TooLarge)?;
    let y0 = ty
        .checked_mul(tile_edge)
        .ok_or(PixelTileCacheError::TooLarge)?;
    Ok((
        x0,
        y0,
        tile_edge.min(width.saturating_sub(x0)),
        tile_edge.min(height.saturating_sub(y0)),
    ))
}

fn frame_row_start(
    y0: u32,
    row: usize,
    stride: usize,
    x0: u32,
) -> Result<usize, PixelTileCacheError> {
    usize::try_from(y0)
        .map_err(|_| PixelTileCacheError::TooLarge)?
        .checked_add(row)
        .and_then(|y| y.checked_mul(stride))
        .and_then(|offset| offset.checked_add(usize::try_from(x0).ok()?.checked_mul(8)?))
        .ok_or(PixelTileCacheError::TooLarge)
}

/// Walk every tile of a frame, returning what each needs.
///
/// The convenience form of [`TileCache::plan_tile`], and the shape the frame
/// loop will call: one pass, counters for §17.2, and a decision per tile.
///
/// # Errors
/// Propagates any [`TilePlanError`] from per-tile planning.
pub fn plan_frame<T>(
    cache: &mut TileCache<T>,
    binning: &Binning,
    plan: &RenderPlan,
    camera: u64,
    output: OutputTransform,
) -> Result<Vec<TileWork>, TilePlanError> {
    cache.begin_frame();
    (0..binning.tile_count())
        .map(|t| cache.plan_tile(binning, plan, t, camera, output))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bin::Tiling;
    use fmn_mobject::{Mob, Mobject, RecordBuffer, RecordSchema, Stage};

    fn rect_mobject(cx: f64, cy: f64, w: f64, h: f64) -> Mobject {
        let (hw, hh) = (0.5 * w, 0.5 * h);
        let pts: Vec<[f64; 3]> = vec![
            [cx - hw, cy - hh, 0.0],
            [cx, cy - hh, 0.0],
            [cx + hw, cy - hh, 0.0],
            [cx + hw, cy, 0.0],
            [cx + hw, cy + hh, 0.0],
            [cx, cy + hh, 0.0],
            [cx - hw, cy + hh, 0.0],
            [cx - hw, cy, 0.0],
            [cx - hw, cy - hh, 0.0],
        ];
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), pts.len()).unwrap();
        for (i, p) in pts.iter().enumerate() {
            buffer.write(i, "point", &[p[0] as f32, p[1] as f32, p[2] as f32]);
            buffer.write(i, "fill_rgba", &[0.2, 0.4, 0.6, 1.0]);
            buffer.write(i, "stroke_rgba", &[1.0, 1.0, 1.0, 1.0]);
            buffer.write(i, "stroke_width", &[2.0]);
        }
        Mobject::from_buffer(buffer)
    }

    fn viewport() -> Viewport {
        Viewport {
            width: 128,
            height: 128,
        }
    }

    fn tile_at(binning: &Binning, x: u32, y: u32) -> usize {
        binning
            .tile_of(x, y)
            .expect("test coordinate is inside the viewport")
    }

    fn output() -> OutputTransform {
        OutputTransform {
            viewport: viewport(),
            map: ScreenMap::default(),
            pixel_format: 0,
        }
    }

    /// A stage with two rectangles, and their handles.
    fn scene() -> (Stage, Vec<Mob>) {
        let mut stage = Stage::new();
        let mut mobs = Vec::new();
        for (cx, cy) in [(40.0, 40.0), (90.0, 90.0)] {
            let mob = stage.add(rect_mobject(cx, cy, 30.0, 30.0));
            stage.add_to_scene(mob).expect("live");
            mobs.push(mob);
        }
        (stage, mobs)
    }

    fn legacy_tile_fingerprints(
        binning: &Binning,
        plan: &RenderPlan,
        tile: usize,
        camera: u64,
        output: OutputTransform,
    ) -> Option<(u64, u64, u64, u64)> {
        if tile >= binning.tile_count()
            || !binning.matches_plan(plan)
            || binning.viewport() != output.viewport
            || binning.map() != output.map
        {
            return None;
        }

        let draws = binning.tile(tile)?;
        let flags = binning.tile_flags(tile)?;
        let instances = plan.shapes().instances();
        let command_capacity = draws
            .len()
            .checked_mul(15)
            .expect("small legacy-oracle fixture");
        let mut commands = Vec::with_capacity(command_capacity);
        let mut revisions = Vec::with_capacity(draws.len());
        for (index, &draw) in draws.iter().enumerate() {
            let instance = instances.get(draw as usize)?;
            if instance.volatile {
                return None;
            }
            commands.push(u64::from(instance.shape));
            commands.push(u64::from(instance.style));
            commands.push(u64::from(*flags.get(index)?));
            commands.extend(
                instance
                    .placement
                    .coefficients()
                    .into_iter()
                    .map(f64::to_bits),
            );
            revisions.push(instance.revisions.fold());
        }

        Some((mix(&commands), mix(&revisions), camera, output.fold()))
    }

    /// The frame loop, retained: one plan and one cache across frames.
    ///
    /// The plan must be retained and not rebuilt — that is the whole of §10.8 —
    /// and it matters to the *cache* for a reason worth stating: shape and style
    /// indices come from append-only interning within one retained-plan epoch.
    /// Ordinary frames therefore keep them stable. A named safe-point compaction
    /// may reshuffle them and deliberately misses once; rebuilding every frame
    /// (as the first version of this harness did) would make every tile key move
    /// for a reason that has nothing to do with the tile.
    struct Compositor {
        plan: RenderPlan,
        cache: TileCache<u32>,
    }

    impl Compositor {
        fn new() -> Compositor {
            Compositor {
                plan: RenderPlan::new(),
                cache: TileCache::new(),
            }
        }

        fn frame(
            &mut self,
            stage: &Stage,
            camera: u64,
            out: OutputTransform,
        ) -> (CacheStats, Binning) {
            self.plan
                .sync(stage, camera)
                .expect("valid compositor fixture");
            let binning = Binning::build(&self.plan, out.viewport, Tiling::default(), out.map)
                .expect("bounded test binning");
            let work = plan_frame(&mut self.cache, &binning, &self.plan, camera, out)
                .expect("binning supplies its own valid tile range");
            // Rasterize the misses, exactly as an engine would, and store what
            // it would have produced.
            for (t, w) in work.into_iter().enumerate() {
                if let TileWork::Rasterize(key) = w {
                    self.cache.store(t, key, t as u32);
                }
            }
            (self.cache.stats(), binning)
        }
    }

    #[test]
    fn a_frame_that_changed_nothing_is_all_hits() {
        // §10.8's headline: "a `wait()` frame reduces to clock advance plus
        // reuse". Not "mostly reuse" — every non-empty tile.
        let (stage, _) = scene();
        let mut c = Compositor::new();
        let (first, _) = c.frame(&stage, 0, output());
        assert_eq!(first.hits, 0, "the first frame cannot hit");
        assert!(first.misses > 0);

        let (second, _) = c.frame(&stage, 0, output());
        assert_eq!(second.misses, 0, "a wait() frame must rasterize nothing");
        assert_eq!(second.hits, first.misses);
        assert!((second.hit_rate() - 1.0).abs() < 1e-12);
        assert!(second.dirty_fraction() < 1e-12);
    }

    /// The adversarial key-completeness harness: apply a mutation, and assert
    /// that at least one tile misses and that the named part of the key moved.
    fn assert_invalidates(
        label: &str,
        mutate: impl FnOnce(&mut Stage, &[Mob]) -> (u64, OutputTransform),
        expect_part: &str,
    ) {
        let (mut stage, mobs) = scene();
        let mut c = Compositor::new();
        let (_, binning) = c.frame(&stage, 0, output());
        let before: Vec<Option<TileKey>> =
            (0..binning.tile_count()).map(|t| c.cache.key(t)).collect();

        let (camera, out) = mutate(&mut stage, &mobs);
        let (stats, _) = c.frame(&stage, camera, out);
        assert!(
            stats.misses > 0 || stats.poisoned > 0,
            "{label}: nothing was invalidated — the key is incomplete"
        );

        // And the part that moved is the one claimed.
        if expect_part.is_empty() {
            return;
        }
        let mut saw = false;
        for (t, old) in before.iter().enumerate() {
            let (Some(old), Some(new)) = (old, c.cache.key(t)) else {
                continue;
            };
            if old != &new {
                let parts = old.diff(&new);
                assert!(
                    parts.contains(&expect_part),
                    "{label}: expected the {expect_part} part to move, got {parts:?}"
                );
                saw = true;
            }
        }
        assert!(saw, "{label}: no tile key changed at all");
    }

    #[test]
    fn a_style_change_invalidates_its_tiles() {
        assert_invalidates(
            "recolour",
            |stage, mobs| {
                stage.get_mut(mobs[0]).expect("live").buffer.write(
                    0,
                    "fill_rgba",
                    &[1.0, 0.0, 0.0, 1.0],
                );
                (0, output())
            },
            "revisions",
        );
    }

    #[test]
    fn a_transform_invalidates_both_the_tiles_it_left_and_the_ones_it_entered() {
        // §10.8: "both old and new bounds dirtied on movement". That falls out
        // of the key rather than needing its own bookkeeping — the command list
        // changes at both ends — and this is the test that says so.
        let (mut stage, mobs) = scene();
        let mut c = Compositor::new();
        let (_, binning) = c.frame(&stage, 0, output());
        let left = tile_at(&binning, 40, 40);
        let entered = tile_at(&binning, 15, 100);
        let key_left = c.cache.key(left).expect("cached");
        let key_entered = c.cache.key(entered);

        // Move the first rectangle from (40,40) to (15,100).
        let entry = stage.get_mut(mobs[0]).expect("live");
        for i in 0..entry.buffer.len() {
            let p = entry.buffer.read(i, "point").expect("field");
            entry
                .buffer
                .write(i, "point", &[p[0] - 25.0, p[1] + 60.0, p[2]]);
        }

        c.frame(&stage, 0, output());
        assert_ne!(
            Some(key_left),
            c.cache.key(left),
            "the tile it left must be dirtied"
        );
        assert_ne!(
            key_entered,
            c.cache.key(entered),
            "the tile it entered must be dirtied"
        );
    }

    #[test]
    fn an_order_change_invalidates_its_tiles() {
        assert_invalidates(
            "z_index",
            |stage, mobs| {
                stage.set_z_index(mobs[0], 5, false);
                stage.add_to_scene(mobs[0]).expect("live");
                (0, output())
            },
            "revisions",
        );
    }

    #[test]
    fn a_camera_move_invalidates_every_tile() {
        // Nothing in the scene changed, and every tile must still miss: the
        // camera is a key part in its own right precisely because no object
        // revision moves when it does.
        let (stage, _) = scene();
        let mut c = Compositor::new();
        let (first, _) = c.frame(&stage, 0, output());
        let (second, _) = c.frame(&stage, 1, output());
        assert_eq!(second.hits, 0, "a camera move cannot hit");
        assert_eq!(second.misses, first.misses);
    }

    #[test]
    fn an_output_transform_change_invalidates_every_tile() {
        // The part most easily forgotten, and the one where the failure is
        // invisible: the same scene at a different zoom, or into a different
        // pixel format, is not the same tile.
        let (stage, _) = scene();
        for changed in [
            OutputTransform {
                map: ScreenMap {
                    scale: 2.0,
                    origin: [0.0, 0.0],
                    y_up: false,
                },
                ..output()
            },
            OutputTransform {
                map: ScreenMap {
                    scale: 1.0,
                    origin: [3.0, 0.0],
                    y_up: false,
                },
                ..output()
            },
            OutputTransform {
                pixel_format: 7,
                ..output()
            },
            OutputTransform {
                viewport: Viewport {
                    width: 256,
                    height: 128,
                },
                ..output()
            },
        ] {
            let mut c = Compositor::new();
            c.frame(&stage, 0, output());
            let (stats, _) = c.frame(&stage, 0, changed);
            assert_eq!(
                stats.hits, 0,
                "a changed output transform must invalidate everything: {changed:?}"
            );
        }
    }

    #[test]
    fn a_writable_live_view_poisons_its_tiles_every_frame() {
        // §8.2: a live view never silently receives weaker semantics to buy
        // speed. The view mutates points with no revision bumped, so a cache
        // that only watched revisions would serve a stale tile forever.
        let (mut stage, mobs) = scene();
        let mut c = Compositor::new();
        c.frame(&stage, 0, output());
        let (clean, _) = c.frame(&stage, 0, output());
        assert_eq!(clean.poisoned, 0);
        assert!(clean.hits > 0);

        let view = stage
            .get_mut(mobs[0])
            .expect("live")
            .buffer
            .export_view(true);

        let (poisoned, _) = c.frame(&stage, 0, output());
        assert!(
            poisoned.poisoned > 0,
            "a writable view must poison the tiles its object touches"
        );
        // And it keeps poisoning them, however many frames pass.
        let (still, _) = c.frame(&stage, 0, output());
        assert_eq!(still.poisoned, poisoned.poisoned);

        // Dropping the view restores reuse — conservatism ends when the reason
        // for it does.
        drop(view);
        c.frame(&stage, 0, output());
        let (restored, _) = c.frame(&stage, 0, output());
        assert_eq!(restored.poisoned, 0);
        assert!(restored.hits > 0);
    }

    #[test]
    fn field_scoped_geometry_and_style_views_poison_their_tiles() {
        for field in ["point", "fill_rgba"] {
            let (mut stage, mobs) = scene();
            let mut c = Compositor::new();
            c.frame(&stage, 0, output());
            let (clean, _) = c.frame(&stage, 0, output());
            assert!(clean.hits > 0);

            let view = stage
                .get_mut(mobs[0])
                .expect("live")
                .buffer
                .export_field_view(field, true)
                .expect("render field");
            let (poisoned, _) = c.frame(&stage, 0, output());
            assert!(
                poisoned.poisoned > 0,
                "writable {field} view left stale tiles reusable"
            );
            drop(view);
        }
    }

    #[test]
    fn a_poisoned_tile_is_never_stored() {
        // The stronger half of poisoning: not merely "does not hit", but leaves
        // nothing behind that a later frame could hit.
        let (mut stage, mobs) = scene();
        let mut c = Compositor::new();
        let (_, binning) = c.frame(&stage, 0, output());
        let tile = tile_at(&binning, 40, 40);
        assert!(c.cache.key(tile).is_some());

        let _view = stage
            .get_mut(mobs[0])
            .expect("live")
            .buffer
            .export_view(true);
        c.frame(&stage, 0, output());
        assert!(
            c.cache.key(tile).is_none(),
            "a poisoned tile must not keep its old entry"
        );
    }

    #[test]
    fn an_emptied_tile_drops_its_entry() {
        // A tile whose object left has nothing to reuse, and keeping a payload
        // that outlives its commands is how a stale tile gets served after the
        // object comes back.
        let (mut stage, mobs) = scene();
        let mut c = Compositor::new();
        let (_, binning) = c.frame(&stage, 0, output());
        let tile = tile_at(&binning, 40, 40);
        assert!(c.cache.get(tile).is_some());

        stage.remove_from_scene(mobs[0]);
        c.frame(&stage, 0, output());
        assert!(c.cache.get(tile).is_none());
    }

    #[test]
    fn reuse_hands_back_the_payload_untouched() {
        // "Reused tiles are byte-identical" is a property of never producing a
        // second version to compare against. The payload here stands in for
        // pixels: if a hit ever recomputed, this would have to change.
        let (stage, _) = scene();
        let mut cache = TileCache::<Vec<u8>>::new();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0).expect("valid cache fixture");
        let binning = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default())
            .expect("bounded test binning");

        let work = plan_frame(&mut cache, &binning, &plan, 0, output())
            .expect("binning supplies its own valid tile range");
        let mut painted = Vec::new();
        for (t, w) in work.into_iter().enumerate() {
            if let TileWork::Rasterize(key) = w {
                cache.store(t, key, vec![t as u8; 16]);
                painted.push(t);
            }
        }

        let again = plan_frame(&mut cache, &binning, &plan, 0, output())
            .expect("binning supplies its own valid tile range");
        for t in painted {
            assert_eq!(again[t], TileWork::Reuse);
            assert_eq!(cache.get(t), Some(&vec![t as u8; 16]));
        }
    }

    #[test]
    fn an_insertion_elsewhere_does_not_dirty_an_untouched_tile() {
        // The reuse a naive key throws away: instance indices shift when
        // anything is added to the scene, so keying on them would dirty every
        // tile in the frame on every insertion. The key holds what a tile
        // *draws*, not where its commands sit in a list.
        let (mut stage, _) = scene();
        let mut c = Compositor::new();
        let (_, binning) = c.frame(&stage, 0, output());
        let far = tile_at(&binning, 90, 90);
        let key_far = c.cache.key(far).expect("cached");

        // Add an object far away, and *behind* both — so it lands first in the
        // draw sequence and shifts every later instance index.
        let newcomer = stage.add(rect_mobject(10.0, 118.0, 12.0, 12.0));
        stage.set_z_index(newcomer, -5, false);
        stage.add_to_scene(newcomer).expect("live");

        c.frame(&stage, 0, output());
        assert_eq!(
            c.cache.key(far),
            Some(key_far),
            "a tile that draws the same things must keep its key"
        );
    }

    #[test]
    fn the_key_reports_which_part_moved() {
        let (stage, _) = scene();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0).expect("valid cache fixture");
        let binning = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default())
            .expect("bounded test binning");
        let tile = tile_at(&binning, 40, 40);
        let a = TileKey::build(&binning, &plan, tile, 0, output()).expect("cacheable");
        let b = TileKey::build(&binning, &plan, tile, 9, output()).expect("cacheable");
        assert_eq!(a.diff(&b), vec!["camera"]);
        assert!(a.diff(&a).is_empty());
    }

    #[test]
    fn streaming_a_tile_key_matches_the_legacy_vector_assembly() {
        let mut stage = Stage::new();
        for (index, size) in [120.0, 40.0].into_iter().enumerate() {
            let mob = stage.add(rect_mobject(64.0, 64.0, size, size));
            if index != 0 {
                stage.shift(mob, [2.0, -1.0, 0.0]);
                stage.set_z_index(mob, index as i32, false);
            }
            stage.add_to_scene(mob).expect("live");
        }

        let mut plan = RenderPlan::new();
        plan.sync(&stage, 17).expect("valid cache fixture");
        let binning = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default())
            .expect("bounded test binning");
        let tile = (0..binning.tile_count())
            .find(|&candidate| {
                let Some(draws) = binning.tile(candidate) else {
                    return false;
                };
                let Some(flags) = binning.tile_flags(candidate) else {
                    return false;
                };
                draws.len() >= 2 && flags.iter().any(|&flag| flag != flags[0])
            })
            .expect("overlap with distinct command classes");

        let draws = binning.tile(tile).expect("selected tile is in range");
        let instances = plan.shapes().instances();
        let first = instances[draws[0] as usize];
        let second = instances[draws[1] as usize];
        assert_ne!(
            first.placement.coefficients(),
            second.placement.coefficients()
        );
        assert_ne!(first.revisions, second.revisions);

        let key = TileKey::build(&binning, &plan, tile, 17, output()).expect("cacheable");
        assert_eq!(
            Some((key.commands, key.revisions, key.camera, key.output)),
            legacy_tile_fingerprints(&binning, &plan, tile, 17, output()),
        );
    }

    #[test]
    fn equal_fingerprints_never_authorize_a_different_tile() {
        let (stage, _) = scene();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0).expect("valid cache fixture");
        let binning = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default())
            .expect("bounded test binning");
        let tile = tile_at(&binning, 40, 40);
        let actual = TileKey::build(&binning, &plan, tile, 0, output()).expect("cacheable");

        // Model the outcome of a short-hash collision directly. The retained
        // inputs differ, while every diagnostic fingerprint remains identical.
        for part in ["commands", "revisions", "output"] {
            let mut stale = actual.clone();
            match part {
                "commands" => stale.command_words[0][0] ^= 1,
                "revisions" => stale.revision_axes[0].geometry ^= 1,
                "output" => stale.output_words[2] ^= 1,
                _ => unreachable!(),
            }
            assert_eq!(
                (stale.commands, stale.revisions, stale.camera, stale.output),
                (
                    actual.commands,
                    actual.revisions,
                    actual.camera,
                    actual.output
                )
            );
            assert_ne!(stale, actual);
            assert_eq!(stale.diff(&actual), vec![part]);
            let mut cache = TileCache::new();
            cache.store(tile, stale, 7u32);
            assert!(matches!(
                cache
                    .plan_tile(&binning, &plan, tile, 0, output())
                    .expect("valid tile"),
                TileWork::Rasterize(_)
            ));
            assert_eq!(
                cache.stats().hits,
                0,
                "equal fingerprints reused stale pixels"
            );
        }
    }

    #[test]
    fn dirty_tiles_recycle_exact_key_storage_and_require_a_completed_store() {
        let (mut stage, mobs) = scene();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0).expect("valid cache fixture");
        let binning = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default())
            .expect("bounded test binning");
        let tile = tile_at(&binning, 40, 40);
        let key = TileKey::build(&binning, &plan, tile, 0, output()).expect("cacheable");
        let command_storage = key.command_words.as_ptr();
        let revision_storage = key.revision_axes.as_ptr();
        let mut cache = TileCache::new();
        cache.store(tile, key, 7u32);
        assert_eq!(
            cache
                .plan_tile(&binning, &plan, tile, 0, output())
                .expect("valid tile"),
            TileWork::Reuse
        );

        stage.shift(mobs[0], [1.0, 0.0, 0.0]);
        plan.sync(&stage, 0).expect("valid moved fixture");
        let moved = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default())
            .expect("bounded moved binning");
        let TileWork::Rasterize(key) = cache
            .plan_tile(&moved, &plan, tile, 0, output())
            .expect("valid moved tile")
        else {
            panic!("the changed tile must rasterize");
        };
        assert_eq!(key.command_words.as_ptr(), command_storage);
        assert_eq!(key.revision_axes.as_ptr(), revision_storage);
        assert!(
            cache.key(tile).is_none(),
            "pending work cannot validate the old payload"
        );
        assert_eq!(
            cache.get(tile),
            Some(&7),
            "dirty payload storage remains recyclable"
        );

        // Abandon that render. Planning again must not treat the old payload
        // as complete merely because a replacement key was already built.
        drop(key);
        let TileWork::Rasterize(key) = cache
            .plan_tile(&moved, &plan, tile, 0, output())
            .expect("valid retry")
        else {
            panic!("abandoned work must rasterize again");
        };
        cache.store(tile, key, 9);
        assert_eq!(
            cache
                .plan_tile(&moved, &plan, tile, 0, output())
                .expect("completed tile"),
            TileWork::Reuse
        );
        assert_eq!(cache.get(tile), Some(&9));
    }

    #[test]
    fn a_retained_table_epoch_change_invalidates_local_row_indices() {
        let (stage, _) = scene();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0).expect("valid cache fixture");
        let binning = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default())
            .expect("bounded test binning");
        let tile = tile_at(&binning, 40, 40);
        let actual = TileKey::build(&binning, &plan, tile, 0, output()).expect("cacheable");
        let mut prior_epoch = actual.clone();
        prior_epoch.epoch = prior_epoch.epoch.wrapping_add(1);
        assert_eq!(prior_epoch.diff(&actual), vec!["commands"]);
        let mut cache = TileCache::new();
        cache.store(tile, prior_epoch, 7u32);
        assert!(matches!(
            cache
                .plan_tile(&binning, &plan, tile, 0, output())
                .expect("valid tile"),
            TileWork::Rasterize(_)
        ));
    }

    #[test]
    fn exact_key_budget_counts_pending_raster_work() {
        let (stage, _) = scene();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0).expect("valid cache fixture");
        let binning = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default())
            .expect("bounded test binning");
        let budget = std::mem::size_of::<[u64; 15]>() + std::mem::size_of::<Revisions>();
        let mut cache = PixelTileCache::with_key_budget_bytes(budget);
        for _ in 0..2 {
            cache
                .prepare_frame(&binning, &plan, 0, Digest::from_bytes([1; 32]))
                .expect("bounded cache planning");
            assert!(cache.retained_key_bytes() <= budget);
            assert!(
                cache
                    .work
                    .iter()
                    .any(|work| matches!(work, TileWork::Rasterize(_)))
            );
            assert!(cache.work.contains(&TileWork::RasterizeUncached));
        }
    }

    #[test]
    fn stale_binning_is_never_admitted_to_the_tile_cache() {
        let (mut stage, mobs) = scene();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0).expect("valid cache fixture");
        let binning = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default())
            .expect("bounded test binning");
        let tile = tile_at(&binning, 40, 40);
        assert!(TileKey::build(&binning, &plan, tile, 0, output()).is_some());

        let mut moved = output();
        moved.map.origin[0] += 1.0;
        assert!(
            TileKey::build(&binning, &plan, tile, 0, moved).is_none(),
            "a key must not bless commands scattered under another map"
        );

        let mut clipped = output();
        clipped.viewport.width -= 1;
        assert!(
            TileKey::build(&binning, &plan, tile, 0, clipped).is_none(),
            "same-grid viewport changes still move the output closure"
        );

        stage.set_z_index(mobs[0], 10, false);
        stage.add_to_scene(mobs[0]).expect("live");
        plan.sync(&stage, 0).expect("valid cache fixture");
        assert!(
            TileKey::build(&binning, &plan, tile, 0, output()).is_none(),
            "stale command indices must never become reusable cache keys"
        );
    }

    #[test]
    fn an_out_of_range_tile_is_not_cacheable() {
        let (stage, _) = scene();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0).expect("valid cache fixture");
        let binning = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default())
            .expect("bounded test binning");

        assert_eq!(
            TileKey::build(&binning, &plan, binning.tile_count(), 0, output()),
            None,
            "the fallible key builder must reject a tile outside the grid",
        );
    }

    #[test]
    fn invalid_tile_planning_is_typed_and_atomic() {
        let (stage, _) = scene();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0).expect("valid cache fixture");
        let binning = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default())
            .expect("bounded test binning");
        let mut cache = TileCache::<u32>::new();
        let valid_tile = tile_at(&binning, 40, 40);
        let key = TileKey::build(&binning, &plan, valid_tile, 0, output())
            .expect("valid tile is cacheable");
        cache.store(valid_tile, key, 7);
        let before_stats = cache.stats();
        let before_len = cache.len();
        let before_key = cache.key(valid_tile);

        let invalid = binning.tile_count();
        let error = cache
            .plan_tile(&binning, &plan, invalid, 0, output())
            .expect_err("one-past-grid tile must be refused");
        assert_eq!(
            error,
            TilePlanError {
                tile: invalid,
                tile_count: invalid,
            }
        );
        assert_eq!(
            error.to_string(),
            format!("tile {invalid} is outside a binning with {invalid} tiles")
        );
        assert_eq!(cache.stats(), before_stats, "refusal must not count work");
        assert_eq!(cache.len(), before_len, "refusal must not evict entries");
        assert_eq!(cache.key(valid_tile), before_key);
    }

    #[test]
    fn counters_partition_every_tile() {
        let (stage, _) = scene();
        let mut c = Compositor::new();
        let (stats, binning) = c.frame(&stage, 0, output());
        assert_eq!(
            stats.hits + stats.misses + stats.poisoned + stats.empty,
            binning.tile_count(),
            "every tile must be accounted for exactly once"
        );
    }
}
