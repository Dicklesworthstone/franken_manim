//! The Tex engine: fmd-math behind the preamble pack, the content-addressed
//! typeset cache, and the pre-play preflight (§11.4–11.5).
//!
//! # The cache key (§14.4's contract, made structural)
//!
//! A typeset result is cached under the digest of its **complete semantic
//! inputs**: the mode and style, the source string, the macro table's
//! canonical bytes (pack plus caller definitions — a pack edit re-typesets,
//! correctly), and the **engine fingerprint**. The fingerprint is the
//! digest of a fixed probe set typeset at construction — a dozen constructs
//! spanning every mechanism (glyph metrics, fractions, scripts, radicals,
//! drawn delimiters, environments, stretchy bands), resolved to canonical
//! path bytes. Any change to fmd-math's layout semantics or to the bundled
//! faces changes the fingerprint, so a SUITE.lock pin bump cold-starts the
//! cache **by construction** — no manually-bumped version constant to
//! forget. Cold and warm are definitionally equivalent; the serialization
//! codec round-trips bit-for-bit (tested), so certified renders are
//! cache-consistent per §16.7.
//!
//! # The preflight (§11.5 — PG-4's design mechanism)
//!
//! [`TexEngine::preflight`] typesets a batch of strings across a scoped
//! thread pool, warming the cache before the first `play()` — so cold
//! start pays typesetting once, in parallel, off the critical path, and
//! PG-7's cached-path lookups are the common case afterward. W9's scene
//! runtime walks the constructed scene and hands the static strings here
//! (the walk hook lands with fm-5xm/fm-39s); the mechanism, its
//! parallelism, and its cache-warming contract are this crate's and are
//! tested here. Errors are collected per string — a preflight never
//! aborts the batch (the failing string will fail again, precisely, at
//! construction time).

use crate::error::{PreflightError, TexError};
use crate::typeset::{Prim, TYPESET_FORMAT_VERSION, Typeset};
use fmd_math::{Layout, MacroSet, PathContour, Style};
use fmn_cache::{CacheKey, KeyBuilder, Namespace};
use fmn_config::{Config, PackRegistry};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

trait ScopedSpawner {
    fn spawn<'scope, 'env: 'scope, F>(
        &self,
        scope: &'scope std::thread::Scope<'scope, 'env>,
        work: F,
    ) -> std::io::Result<std::thread::ScopedJoinHandle<'scope, ()>>
    where
        F: FnOnce() + Send + 'scope;
}

struct NativeScopedSpawner;

impl ScopedSpawner for NativeScopedSpawner {
    fn spawn<'scope, 'env: 'scope, F>(
        &self,
        scope: &'scope std::thread::Scope<'scope, 'env>,
        work: F,
    ) -> std::io::Result<std::thread::ScopedJoinHandle<'scope, ()>>
    where
        F: FnOnce() + Send + 'scope,
    {
        std::thread::Builder::new().spawn_scoped(scope, work)
    }
}

type PreflightOutcome = Result<(), TexError>;
type PreflightSlot = Mutex<Option<PreflightOutcome>>;

/// How a string is typeset: mathematics at a style, or the TexText
/// text-mainland contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The `Tex` surface (whole string is mathematics).
    Math(Style),
    /// The `TexText` surface (text mainland with `$…$` islands).
    Text,
}

/// The Tex engine: fmd-math + the resolved preamble pack + the cache.
pub struct TexEngine {
    math: fmd_math::Engine,
    macros: MacroSet,
    /// The resolved pack's stable content id (for provenance/doctor).
    pack_content_id: &'static str,
    /// The engine fingerprint: sha-256 over the probe set's canonical
    /// bytes plus the macro table — the cache key's engine component.
    fingerprint: CacheKey,
    cache: Option<Namespace>,
}

impl core::fmt::Debug for TexEngine {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TexEngine")
            .field("pack_content_id", &self.pack_content_id)
            .field("macros", &self.macros.len())
            .field("fingerprint", &self.fingerprint)
            .field("cached", &self.cache.is_some())
            .finish_non_exhaustive()
    }
}

impl TexEngine {
    /// An engine over the bundled faces with the given pack content id
    /// (`fmd-math/pack/default` etc. — the ids fmn-config's registry
    /// records) and optional caller macro definitions layered on top.
    ///
    /// # Errors
    ///
    /// [`TexError::Faces`] if the bundled faces fail to load (build
    /// corruption); [`TexError::UnknownPack`] if the content id names no
    /// pack (registry/pack drift — a wiring bug, reported precisely).
    pub fn new(pack_content_id: &'static str, extra: Option<&MacroSet>) -> Result<Self, TexError> {
        let math = fmd_math::Engine::bundled().map_err(|e| TexError::Faces {
            what: e.to_string(),
        })?;
        let mut macros = MacroSet::pack(pack_content_id).ok_or(TexError::UnknownPack {
            content_id: pack_content_id,
        })?;
        if let Some(extra) = extra {
            // Caller definitions layer over the pack, last wins.
            macros = merged(&macros, extra);
        }
        let fingerprint = fingerprint(&math, &macros);
        Ok(Self {
            math,
            macros,
            pack_content_id,
            fingerprint,
            cache: None,
        })
    }

    /// An engine wired from the typed config: `tex.template` resolves
    /// through the pack registry's compatibility mapping (an out-of-tier
    /// template is the registry's named refusal).
    ///
    /// # Errors
    ///
    /// [`TexError::Pack`] for template refusals, plus [`TexEngine::new`]'s.
    pub fn from_config(config: &Config, registry: &PackRegistry) -> Result<Self, TexError> {
        let pack = registry
            .resolve_template(&config.tex.template)
            .map_err(TexError::Pack)?;
        Self::new(pack.content_id, None)
    }

    /// Attach a cache namespace. The namespace version is the typeset
    /// serialization format's ([`TYPESET_FORMAT_VERSION`]); engine
    /// semantics live in the key's fingerprint instead, so a pin bump
    /// cold-starts without a namespace bump.
    ///
    /// # Errors
    ///
    /// [`TexError::Cache`] if the namespace cannot be opened.
    pub fn with_cache(mut self, store: &fmn_cache::Store) -> Result<Self, TexError> {
        let ns = store
            .namespace(
                "typeset",
                TYPESET_FORMAT_VERSION,
                fmn_cache::NamespacePolicy::default(),
            )
            .map_err(|e| TexError::Cache {
                what: e.to_string(),
            })?;
        self.cache = Some(ns);
        Ok(self)
    }

    /// The resolved pack's content id (provenance, `fmn doctor`).
    #[must_use]
    pub fn pack_content_id(&self) -> &'static str {
        self.pack_content_id
    }

    /// The engine fingerprint (provenance / the input closure).
    #[must_use]
    pub fn fingerprint(&self) -> &CacheKey {
        &self.fingerprint
    }

    /// The cache key for one (mode, source) under this engine.
    ///
    /// `None` means the canonical key material exceeded its fixed format
    /// budget, so the value is deliberately uncacheable. No reduced key is
    /// substituted: dropping mode, source, or fingerprint identity could turn
    /// a cache optimization into a semantic collision.
    #[must_use]
    pub fn cache_key(&self, mode: Mode, source: &str) -> Option<CacheKey> {
        let (tag, style) = match mode {
            Mode::Math(Style::Display) => ("math", 0_u32),
            Mode::Math(Style::Text) => ("math", 1),
            Mode::Math(Style::Script) => ("math", 2),
            Mode::Math(Style::ScriptScript) => ("math", 3),
            Mode::Text => ("text", 0),
        };
        KeyBuilder::new("fmn-tex/typeset")
            .push_str(tag)
            .push_u32(style)
            .push_str(source)
            .push_digest(self.fingerprint.digest())
            .finish()
            .ok()
    }

    /// Typeset through the cache: a verified hit returns paths + span map
    /// without re-layout (PG-7's <100 µs path); a miss lays out and stores
    /// best-effort. Cache trouble degrades to computing — never fatal,
    /// never wrong.
    ///
    /// # Errors
    ///
    /// [`TexError::Math`]: the precise, named, tier-tagged construct
    /// errors surface at construction time — never a blank render.
    pub fn typeset(&self, mode: Mode, source: &str) -> Result<Typeset, TexError> {
        if let Some(ns) = &self.cache
            && let Some(key) = self.cache_key(mode, source)
        {
            if let Ok(Some(bytes)) = ns.get(&key)
                && let Ok(hit) = Typeset::from_bytes(&bytes)
            {
                return Ok(hit);
            }
            let fresh = self.layout(mode, source)?;
            if let Ok(bytes) = fresh.to_bytes() {
                let _ = ns.put(&key, &bytes);
            }
            return Ok(fresh);
        }
        self.layout(mode, source)
    }

    fn layout(&self, mode: Mode, source: &str) -> Result<Typeset, TexError> {
        let layout = match mode {
            Mode::Math(style) => self
                .math
                .typeset_with_macros(source, style, &self.macros)
                .map_err(TexError::Math)?,
            Mode::Text => self
                .math
                .typeset_text_with_macros(source, &self.macros)
                .map_err(TexError::Math)?,
        };
        Typeset::from_borrowed(source, layout).map_err(TexError::from)
    }

    /// Resolve one submobject primitive into its closed quadratic
    /// contours — the span-preserving form of
    /// [`fmd_math::paths::resolve_paths`], which flattens the whole layout
    /// and would destroy the per-`Sub` grouping `TransformMatchingTex`
    /// consumes. The library tier builds one VMobject per `Sub` from
    /// these contours (fm-p5d); glyph resolution reuses the engine's
    /// pinned size/upm transform verbatim (a synthetic one-primitive
    /// layout through `resolve_paths`), rules arrive as rectangle
    /// contours, and drawn paths pass through positioned.
    ///
    /// The output is in ems, y-up, baseline at 0 — the same frame as the
    /// layout itself.
    ///
    /// # Errors
    ///
    /// [`TexError::BadPrim`] if `prim` indexes outside the typeset's
    /// primitive lists (a consumer wiring bug, named); [`TexError::Math`]
    /// if a glyph's outline fails to decode.
    pub fn resolve_prim(
        &self,
        typeset: &Typeset,
        prim: Prim,
    ) -> Result<Vec<PathContour>, TexError> {
        let layout = &typeset.layout;
        let single = match prim {
            Prim::Glyph(i) => Layout {
                glyphs: vec![layout.glyphs.get(i).cloned().ok_or(TexError::BadPrim {
                    what: format!("glyph {i} of {}", layout.glyphs.len()),
                })?],
                ..Layout::default()
            },
            Prim::Rule(i) => Layout {
                rules: vec![layout.rules.get(i).cloned().ok_or(TexError::BadPrim {
                    what: format!("rule {i} of {}", layout.rules.len()),
                })?],
                ..Layout::default()
            },
            Prim::Path(i) => Layout {
                paths: vec![layout.paths.get(i).cloned().ok_or(TexError::BadPrim {
                    what: format!("path {i} of {}", layout.paths.len()),
                })?],
                ..Layout::default()
            },
        };
        fmd_math::paths::resolve_paths(&self.math, &single).map_err(TexError::Math)
    }

    /// Warm the cache for a batch of strings, in parallel, before the
    /// first frame (§11.5). Returns per-string outcomes in input order;
    /// one failing string never aborts the batch.
    ///
    /// Worker availability affects only scheduling: every successfully
    /// started worker remains useful, and a refusal before the first worker
    /// falls back to the caller thread. No input is silently skipped.
    ///
    /// # Errors
    ///
    /// [`PreflightError::ResultStorageAllocationFailed`] if the complete
    /// ordered outcome cannot be reserved before cache-warming work starts.
    pub fn preflight(
        &self,
        items: &[(Mode, &str)],
    ) -> Result<Vec<Result<(), TexError>>, PreflightError> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let workers = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(1)
            .min(items.len());
        self.preflight_with_spawner(items, workers, &NativeScopedSpawner)
    }

    fn preflight_with_spawner<Spawner>(
        &self,
        items: &[(Mode, &str)],
        workers: usize,
        spawner: &Spawner,
    ) -> Result<Vec<Result<(), TexError>>, PreflightError>
    where
        Spawner: ScopedSpawner,
    {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let workers = workers.clamp(1, items.len());
        let next = AtomicUsize::new(0);
        let (results, mut outcomes) = preflight_storage(items.len())?;
        std::thread::scope(|scope| {
            let mut spawned = 0;
            for _ in 0..workers {
                let next = &next;
                let results = &results;
                if spawner
                    .spawn(scope, move || preflight_worker(self, items, next, results))
                    .is_err()
                {
                    break;
                }
                spawned += 1;
            }
            if spawned == 0 {
                preflight_worker(self, items, &next, &results);
            }
        });

        for ((mode, source), slot) in items.iter().copied().zip(results) {
            let outcome = slot
                .into_inner()
                .unwrap_or_else(PoisonError::into_inner)
                .unwrap_or_else(|| self.typeset(mode, source).map(|_| ()));
            outcomes.push(outcome);
        }
        Ok(outcomes)
    }
}

fn preflight_storage(
    items: usize,
) -> Result<(Vec<PreflightSlot>, Vec<PreflightOutcome>), PreflightError> {
    let mut slots = Vec::new();
    slots
        .try_reserve_exact(items)
        .map_err(|_| PreflightError::ResultStorageAllocationFailed { items })?;
    for _ in 0..items {
        slots.push(Mutex::new(None));
    }

    let mut outcomes = Vec::new();
    outcomes
        .try_reserve_exact(items)
        .map_err(|_| PreflightError::ResultStorageAllocationFailed { items })?;
    Ok((slots, outcomes))
}

fn preflight_worker(
    engine: &TexEngine,
    items: &[(Mode, &str)],
    next: &AtomicUsize,
    results: &[PreflightSlot],
) {
    loop {
        let index = next.fetch_add(1, Ordering::Relaxed);
        let Some((slot, (mode, source))) = results.get(index).zip(items.get(index)) else {
            break;
        };
        let outcome = engine.typeset(*mode, source).map(|_| ());
        let mut slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
        *slot = Some(outcome);
    }
}

/// Layer `extra` over `base` (last wins), through canonical bytes: the
/// merged set is rebuilt definition-by-definition so validation and
/// canonical identity stay uniform.
fn merged(base: &MacroSet, extra: &MacroSet) -> MacroSet {
    // MacroSet has no direct iterator over bodies; canonical_bytes is the
    // exchange format. Parse it back: `name US params US body RS` records
    // after the version tag.
    let mut out = base.clone();
    let bytes = extra.canonical_bytes();
    let Some(tag_end) = bytes.iter().position(|&b| b == 0x1e) else {
        return out;
    };
    let mut rest = &bytes[tag_end + 1..];
    while let Some(rec_end) = rest.iter().position(|&b| b == 0x1e) {
        let rec = &rest[..rec_end];
        rest = &rest[rec_end + 1..];
        let mut fields = rec.split(|&b| b == 0x1f);
        let (Some(name), Some(params), Some(body)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if let (Ok(name), Some(&p), Ok(body)) = (
            core::str::from_utf8(name),
            params.first(),
            core::str::from_utf8(body),
        ) {
            // Definitions already validated on the way into `extra`.
            let _ = out.define(name, p.saturating_sub(b'0'), body);
        }
    }
    out
}

/// The engine fingerprint: canonical layout bytes of a fixed probe set
/// spanning every mechanism, plus the macro table's canonical bytes.
fn fingerprint(math: &fmd_math::Engine, macros: &MacroSet) -> CacheKey {
    /// Constructs chosen to touch every layout mechanism: glyph metrics
    /// and kerning, scripts, fractions, radicals, big operators, accents,
    /// drawn delimiters past the ceiling, environments, stretchy bands,
    /// and text mode. A semantics change anywhere shows up here.
    const PROBES: &[&str] = &[
        r"ax + b^2_c",
        r"\frac{1}{1+\frac{1}{x}}",
        r"\sqrt[3]{x+1}",
        r"\sum_{n=1}^{N} n \int_0^1 x\,dx",
        r"\hat x + \overline{AB}",
        r"\left(\frac{\frac{1}{2}}{\frac{3}{4}}\right)",
        r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}",
        r"\begin{cases} x & x > 0 \\ -x & x \le 0 \end{cases}",
        r"\widehat{x+y} + \overbrace{a+b}",
        r"\mathbb{R} \mathrm{d} \mathbf{v}",
    ];
    let mut material = Vec::new();
    for probe in PROBES {
        match math.typeset(probe, Style::Display) {
            Ok(layout) => {
                material.extend_from_slice(fmd_math::paths::layout_dump(&layout).as_bytes());
                if let Ok(contours) = fmd_math::paths::resolve_paths(math, &layout) {
                    material
                        .extend_from_slice(fmd_math::paths::canonical_dump(&contours).as_bytes());
                }
            }
            Err(e) => {
                // A probe that stops typesetting is itself a semantic
                // change; fold the error text in.
                material.extend_from_slice(e.to_string().as_bytes());
            }
        }
        material.push(0x1e);
    }
    material.extend_from_slice(&macros.canonical_bytes());
    CacheKey::of_content(&material)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use fmn_cache::{NamespacePolicy, Store, StoreConfig};
    use fmn_platform::clock::FakeClock;
    use fmn_platform::fs::VirtualFs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct RefusingScopedSpawner {
        refuse_at: usize,
        attempts: AtomicUsize,
    }

    impl RefusingScopedSpawner {
        const fn new(refuse_at: usize) -> Self {
            Self {
                refuse_at,
                attempts: AtomicUsize::new(0),
            }
        }

        fn attempts(&self) -> usize {
            self.attempts.load(Ordering::Relaxed)
        }
    }

    impl ScopedSpawner for RefusingScopedSpawner {
        fn spawn<'scope, 'env: 'scope, F>(
            &self,
            scope: &'scope std::thread::Scope<'scope, 'env>,
            work: F,
        ) -> std::io::Result<std::thread::ScopedJoinHandle<'scope, ()>>
        where
            F: FnOnce() + Send + 'scope,
        {
            let attempt = self.attempts.fetch_add(1, Ordering::Relaxed);
            if attempt == self.refuse_at {
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            NativeScopedSpawner.spawn(scope, work)
        }
    }

    fn store() -> Store {
        Store::open(
            Arc::new(VirtualFs::new()),
            Arc::new(FakeClock::new()),
            "/cache",
            StoreConfig::default(),
        )
        .expect("virtual cache store")
    }

    fn assert_refused_worker_still_warms_every_item(refuse_at: usize) {
        let store = store();
        let engine = TexEngine::new("fmd-math/pack/default", None)
            .expect("bundled engine")
            .with_cache(&store)
            .expect("cache namespace");
        let items = [
            (Mode::Math(Style::Display), "x + 1"),
            (Mode::Math(Style::Display), r"\frac{1}{2}"),
            (Mode::Text, r"area $\pi r^2$"),
            (Mode::Math(Style::Display), r"\sqrt{x + 1}"),
        ];
        let spawner = RefusingScopedSpawner::new(refuse_at);

        let outcomes = engine
            .preflight_with_spawner(&items, items.len(), &spawner)
            .expect("result storage");

        assert_eq!(spawner.attempts(), refuse_at.saturating_add(1));
        assert_eq!(outcomes.len(), items.len());
        assert!(outcomes.iter().all(Result::is_ok));
        let namespace = store
            .namespace(
                "typeset",
                TYPESET_FORMAT_VERSION,
                NamespacePolicy::default(),
            )
            .expect("typeset namespace");
        for (mode, source) in items {
            let key = engine
                .cache_key(mode, source)
                .expect("small test source has a canonical cache key");
            assert!(
                namespace.get(&key).expect("cache read").is_some(),
                "refused startup left {source:?} cold"
            );
        }
    }

    #[test]
    fn first_worker_refusal_falls_back_to_caller_and_warms_every_item() {
        assert_refused_worker_still_warms_every_item(0);
    }

    #[test]
    fn intermediate_worker_refusal_keeps_the_started_subset_semantic() {
        assert_refused_worker_still_warms_every_item(2);
    }

    #[test]
    fn empty_preflight_starts_no_workers() {
        let engine = TexEngine::new("fmd-math/pack/default", None).expect("bundled engine");
        let spawner = RefusingScopedSpawner::new(0);
        let outcomes = engine
            .preflight_with_spawner(&[], 4, &spawner)
            .expect("empty storage");
        assert!(outcomes.is_empty());
        assert_eq!(spawner.attempts(), 0);
    }

    #[test]
    fn preflight_result_storage_refuses_capacity_overflow() {
        assert!(matches!(
            preflight_storage(usize::MAX),
            Err(PreflightError::ResultStorageAllocationFailed { items: usize::MAX })
        ));
    }
}
