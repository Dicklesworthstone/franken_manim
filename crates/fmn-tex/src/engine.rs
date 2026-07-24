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

use crate::error::TexError;
use crate::typeset::{TYPESET_FORMAT_VERSION, Typeset};
use fmd_math::{MacroSet, Style};
use fmn_cache::{CacheKey, KeyBuilder, Namespace};
use fmn_config::{Config, PackRegistry};

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
    #[must_use]
    pub fn cache_key(&self, mode: Mode, source: &str) -> CacheKey {
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
            .unwrap_or_else(|_| {
                // Unreachable in practice (a source string over the serial
                // field cap); an uncacheable key that still typesets.
                CacheKey::of_content(source.as_bytes())
            })
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
        if let Some(ns) = &self.cache {
            let key = self.cache_key(mode, source);
            if let Ok(Some(bytes)) = ns.get(&key)
                && let Some(hit) = Typeset::from_bytes(&bytes)
            {
                return Ok(hit);
            }
            let fresh = self.layout(mode, source)?;
            let _ = ns.put(&key, &fresh.to_bytes());
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
        Ok(Typeset::new(source.to_owned(), layout))
    }

    /// Warm the cache for a batch of strings, in parallel, before the
    /// first frame (§11.5). Returns per-string outcomes in input order;
    /// one failing string never aborts the batch.
    pub fn preflight(&self, items: &[(Mode, &str)]) -> Vec<Result<(), TexError>> {
        let workers = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(1)
            .min(items.len().max(1));
        let next = std::sync::atomic::AtomicUsize::new(0);
        let results: Vec<std::sync::Mutex<Option<Result<(), TexError>>>> = (0..items.len())
            .map(|_| std::sync::Mutex::new(None))
            .collect();
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some((mode, source)) = items.get(i) else {
                            break;
                        };
                        let outcome = self.typeset(*mode, source).map(|_| ());
                        if let Ok(mut slot) = results[i].lock() {
                            *slot = Some(outcome);
                        }
                    }
                });
            }
        });
        results
            .into_iter()
            .map(|slot| {
                slot.into_inner()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .unwrap_or(Ok(()))
            })
            .collect()
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
