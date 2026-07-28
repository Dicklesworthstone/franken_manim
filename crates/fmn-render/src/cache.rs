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
//! ## What is generic, and why
//!
//! [`TileCache`] is generic over the payload. The engines own pixels (§10.1), and
//! there is no rasterizer in the tree yet; parameterizing means the cache
//! mechanism — the part where a mistake is catastrophic — is complete and tested
//! now, and the payload type arrives with the engine that produces it rather
//! than being guessed at here.
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
use crate::revision::mix;
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
    /// Fold into the key.
    fn fold(&self) -> u64 {
        mix(&[
            u64::from(self.viewport.width),
            u64::from(self.viewport.height),
            self.map.scale.to_bits(),
            self.map.origin[0].to_bits(),
            self.map.origin[1].to_bits(),
            u64::from(self.pixel_format),
        ])
    }
}

/// §10.8's tile-cache key, in its four declared parts.
///
/// Kept as four fields rather than one hash so a cache miss can say *which* part
/// moved — the difference between a diagnosable reuse regression and a mystery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileKey {
    /// The tile's command run and its class words.
    pub commands: u64,
    /// A fold of every listed instance's seven revision axes.
    pub revisions: u64,
    /// The camera revision.
    pub camera: u64,
    /// The output transform.
    pub output: u64,
}

impl TileKey {
    /// Build the key for one fine tile, or `None` if the tile is uncacheable.
    ///
    /// `None` means poisoned. A §8.2 writable live view is the ordinary source;
    /// stale binning or an output transform different from the one that produced
    /// it is also refused, because caching a wrong command list would make the
    /// wrong pixels persist. It is deliberately not a key that happens never to
    /// match: a caller that sees `None` knows not to *store* the result either,
    /// which a never-matching key would not tell it.
    #[must_use]
    pub fn build(
        binning: &Binning,
        plan: &RenderPlan,
        tile: usize,
        camera: u64,
        output: OutputTransform,
    ) -> Option<TileKey> {
        if !binning.matches_plan(plan)
            || binning.viewport() != output.viewport
            || binning.map() != output.map
        {
            return None;
        }
        let draws = binning.tile(tile);
        let flags = binning.tile_flags(tile);
        let instances = plan.shapes().instances();

        let mut commands: Vec<u64> = Vec::with_capacity(draws.len() * 2);
        let mut revisions: Vec<u64> = Vec::with_capacity(draws.len());
        for (k, &d) in draws.iter().enumerate() {
            let inst = instances.get(d as usize)?;
            if inst.volatile {
                return None;
            }
            // The command's *identity within the frame*, not the instance index:
            // an object inserted earlier in the scene shifts every later index
            // without changing what this tile draws, and a key that moved for
            // that would defeat reuse on every insertion.
            commands.push(u64::from(inst.shape));
            commands.push(u64::from(inst.style));
            commands.push(u64::from(flags[k]));
            commands.push(inst.offset[0].to_bits());
            commands.push(inst.offset[1].to_bits());
            commands.push(inst.offset[2].to_bits());
            revisions.push(inst.revisions);
        }

        Some(TileKey {
            commands: mix(&commands),
            revisions: mix(&revisions),
            camera,
            output: output.fold(),
        })
    }

    /// Which part of the key differs from `other`, for diagnosing a miss.
    #[must_use]
    pub fn diff(&self, other: &TileKey) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.commands != other.commands {
            out.push("commands");
        }
        if self.revisions != other.revisions {
            out.push("revisions");
        }
        if self.camera != other.camera {
            out.push("camera");
        }
        if self.output != other.output {
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
    /// Tiles that could not be cached at all (§8.2's live views).
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
    entries: HashMap<usize, (TileKey, T)>,
    stats: CacheStats,
}

/// What compositing one tile needs to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    pub fn plan_tile(
        &mut self,
        binning: &Binning,
        plan: &RenderPlan,
        tile: usize,
        camera: u64,
        output: OutputTransform,
    ) -> TileWork {
        if binning.tile(tile).is_empty() {
            // An empty tile is not a cache entry: it has no contents to reuse,
            // and keeping one would let a stale payload outlive the commands
            // that produced it.
            self.entries.remove(&tile);
            self.stats.empty += 1;
            return TileWork::Empty;
        }
        let Some(key) = TileKey::build(binning, plan, tile, camera, output) else {
            self.entries.remove(&tile);
            self.stats.poisoned += 1;
            return TileWork::RasterizeUncached;
        };
        match self.entries.get(&tile) {
            Some((cached, _)) if *cached == key => {
                self.stats.hits += 1;
                TileWork::Reuse
            }
            _ => {
                self.stats.misses += 1;
                TileWork::Rasterize(key)
            }
        }
    }

    /// Store a freshly rasterized tile under the key [`plan_tile`] returned.
    ///
    /// Taking the key rather than recomputing it is what stops a store keyed on
    /// a state the payload was not rendered from — the one way a complete key
    /// can still cache a stale tile.
    ///
    /// [`plan_tile`]: TileCache::plan_tile
    pub fn store(&mut self, tile: usize, key: TileKey, payload: T) {
        self.entries.insert(tile, (key, payload));
    }

    /// The cached payload for a tile, if any.
    #[must_use]
    pub fn get(&self, tile: usize) -> Option<&T> {
        self.entries.get(&tile).map(|(_, payload)| payload)
    }

    /// The key a tile is currently cached under.
    #[must_use]
    pub fn key(&self, tile: usize) -> Option<TileKey> {
        self.entries.get(&tile).map(|(k, _)| *k)
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
}

/// Walk every tile of a frame, returning what each needs.
///
/// The convenience form of [`TileCache::plan_tile`], and the shape the frame
/// loop will call: one pass, counters for §17.2, and a decision per tile.
pub fn plan_frame<T>(
    cache: &mut TileCache<T>,
    binning: &Binning,
    plan: &RenderPlan,
    camera: u64,
    output: OutputTransform,
) -> Vec<TileWork> {
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
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), pts.len());
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

    /// The frame loop, retained: one plan and one cache across frames.
    ///
    /// The plan must be retained and not rebuilt — that is the whole of §10.8 —
    /// and it matters to the *cache* for a reason worth stating: shape and style
    /// indices come from append-only interning, so a retained plan keeps them
    /// stable and a rebuilt one reshuffles them on any change. Rebuilding per
    /// frame (as the first version of this harness did) makes every tile key
    /// move for a reason that has nothing to do with the tile.
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
            self.plan.sync(stage, camera);
            let binning = Binning::build(&self.plan, out.viewport, Tiling::default(), out.map);
            let work = plan_frame(&mut self.cache, &binning, &self.plan, camera, out);
            // Rasterize the misses, exactly as an engine would, and store what
            // it would have produced.
            for (t, w) in work.iter().enumerate() {
                if let TileWork::Rasterize(key) = w {
                    self.cache.store(t, *key, t as u32);
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
        let left = binning.tile_of(40, 40);
        let entered = binning.tile_of(15, 100);
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
                },
                ..output()
            },
            OutputTransform {
                map: ScreenMap {
                    scale: 1.0,
                    origin: [3.0, 0.0],
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
    fn a_poisoned_tile_is_never_stored() {
        // The stronger half of poisoning: not merely "does not hit", but leaves
        // nothing behind that a later frame could hit.
        let (mut stage, mobs) = scene();
        let mut c = Compositor::new();
        let (_, binning) = c.frame(&stage, 0, output());
        let tile = binning.tile_of(40, 40);
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
        let tile = binning.tile_of(40, 40);
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
        plan.sync(&stage, 0);
        let binning = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default());

        let work = plan_frame(&mut cache, &binning, &plan, 0, output());
        let mut painted = Vec::new();
        for (t, w) in work.iter().enumerate() {
            if let TileWork::Rasterize(key) = w {
                cache.store(t, *key, vec![t as u8; 16]);
                painted.push(t);
            }
        }

        let again = plan_frame(&mut cache, &binning, &plan, 0, output());
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
        let far = binning.tile_of(90, 90);
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
        plan.sync(&stage, 0);
        let binning = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default());
        let tile = binning.tile_of(40, 40);
        let a = TileKey::build(&binning, &plan, tile, 0, output()).expect("cacheable");
        let b = TileKey::build(&binning, &plan, tile, 9, output()).expect("cacheable");
        assert_eq!(a.diff(&b), vec!["camera"]);
        assert!(a.diff(&a).is_empty());
    }

    #[test]
    fn stale_binning_is_never_admitted_to_the_tile_cache() {
        let (mut stage, mobs) = scene();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);
        let binning = Binning::build(&plan, viewport(), Tiling::default(), ScreenMap::default());
        let tile = binning.tile_of(40, 40);
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
        plan.sync(&stage, 0);
        assert!(
            TileKey::build(&binning, &plan, tile, 0, output()).is_none(),
            "stale command indices must never become reusable cache keys"
        );
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
