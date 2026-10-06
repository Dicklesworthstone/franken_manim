//! Lazy, capability-bound typesetting for a single native scene run.
//!
//! Constructing a session does not load fonts or open files. The first actual
//! TeX request creates one engine, resolves its template, and attaches the
//! existing content-addressed store. Storage is optional; layout is not.

use std::cell::{OnceCell, RefCell};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use fmn_cache::{Store, StoreConfig, resolve_host_cache_root};
use fmn_config::{Config, PackRegistry};
use fmn_platform::clock::Clock;
use fmn_platform::fs::FileSystem;

use crate::{TexEngine, TexError, TypesetCacheStats};

/// Actual work performed by a session, not part of certified scene identity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypesetSessionReport {
    /// Whether any caller requested an engine (shape-only scenes stay lazy).
    pub initialized: bool,
    /// Whether the engine has an attached persistent namespace.
    pub persistent: bool,
    /// A storage refusal leaves layout available and is reported here.
    pub cache_error: Option<String>,
    /// Engine-local memory-front counters and resident encoded bytes.
    pub memory: TypesetCacheStats,
    /// Checksum-verified persistent hits, excluding memory-front hits.
    pub persistent_hits: u64,
    /// Actual layouts, excluding engine-fingerprint probes.
    pub layout_computations: u64,
}

struct CacheBinding {
    root: Result<Option<PathBuf>, String>,
    fs: Arc<dyn FileSystem>,
    clock: Arc<dyn Clock>,
}

/// One template, one engine and one cache binding for a native scene run.
///
/// The session belongs to the serial scene owner. Its engine can run the
/// existing scoped parallel preflight; no live scene or mutable callback is
/// sent to those workers. Separate sessions never share mutable macro state.
pub struct TexSession {
    template: String,
    cache: Option<CacheBinding>,
    engine: OnceCell<TexEngine>,
    cache_error: RefCell<Option<String>>,
}

impl TexSession {
    /// Filesystem-free authoring, retaining only the bounded memory cache.
    #[must_use]
    pub fn memory(template: impl Into<String>) -> Self {
        Self {
            template: template.into(),
            cache: None,
            engine: OnceCell::new(),
            cache_error: RefCell::new(None),
        }
    }

    /// Use the resolved template and cache directory with explicit capabilities.
    ///
    /// Native filesystems use the same owned-root resolver as `fmn doctor` and
    /// `--clear-cache`. An injected filesystem requires an explicit absolute
    /// cache path: it never falls through to the ambient host filesystem,
    /// environment or working directory. Root ownership and entry validation
    /// remain fmn-cache's responsibility. Explicit host paths are anchored
    /// before scene code can change the working directory. The platform
    /// default is resolved on first use. An unavailable cache is nonfatal.
    #[must_use]
    pub fn with_cache(config: &Config, fs: Arc<dyn FileSystem>, clock: Arc<dyn Clock>) -> Self {
        Self {
            cache: Some(CacheBinding {
                root: cache_root(&config.directories.cache, fs.as_ref()),
                fs,
                clock,
            }),
            ..Self::memory(config.tex.template.clone())
        }
    }

    /// The same lazily initialized engine for preflight and real constructors.
    ///
    /// # Errors
    /// A template or bundled-font refusal retains its original [`TexError`].
    /// Storage errors are available in [`Self::report`], not substituted for
    /// typesetting errors and never a reason to emit blank geometry.
    pub fn engine(&self) -> Result<&TexEngine, TexError> {
        if let Some(engine) = self.engine.get() {
            return Ok(engine);
        }
        let registry = PackRegistry::builtin();
        let template = if self.template.is_empty() {
            "default"
        } else {
            &self.template
        };
        let pack = registry
            .resolve_template(template)
            .map_err(TexError::Pack)?;
        let engine = TexEngine::new(pack.content_id, None)?;
        if let Some(binding) = &self.cache {
            let result = open_store(binding).and_then(|store| {
                engine
                    .set_cache(Some(&store))
                    .map_err(|error| error.to_string())
            });
            if let Err(error) = result {
                *self.cache_error.borrow_mut() = Some(error);
            }
        }
        Ok(self.engine.get_or_init(|| engine))
    }

    /// Inspect counters without initializing the engine or touching the cache.
    #[must_use]
    pub fn report(&self) -> TypesetSessionReport {
        let Some(engine) = self.engine.get() else {
            return TypesetSessionReport::default();
        };
        TypesetSessionReport {
            initialized: true,
            persistent: engine.persistent_cache_enabled(),
            cache_error: self.cache_error.borrow().clone(),
            memory: engine.memory_cache_stats(),
            persistent_hits: engine.persistent_cache_hits(),
            layout_computations: engine.layout_computations(),
        }
    }
}

impl Default for TexSession {
    fn default() -> Self {
        Self::memory("default")
    }
}

fn cache_root(directory: &str, fs: &dyn FileSystem) -> Result<Option<PathBuf>, String> {
    if fs.grants_host_destructive_lifecycle() {
        return if directory.is_empty() {
            Ok(None)
        } else {
            resolve_host_cache_root(directory)
                .map(Some)
                .map_err(|error| error.to_string())
        };
    }
    let root = Path::new(directory);
    if !root.is_absolute() || root.components().any(|part| part == Component::ParentDir) {
        return Err(
            "injected typeset cache requires an explicit absolute directory without '..'"
                .to_owned(),
        );
    }
    Ok(Some(root.to_path_buf()))
}

fn open_store(binding: &CacheBinding) -> Result<Store, String> {
    let root = binding.root.as_ref().map_err(Clone::clone)?;
    let Some(root) = root else {
        return Store::open_host(
            Arc::clone(&binding.fs),
            Arc::clone(&binding.clock),
            "",
            StoreConfig::default(),
        )
        .map_err(|error| error.to_string());
    };
    Store::open(
        Arc::clone(&binding.fs),
        Arc::clone(&binding.clock),
        root,
        StoreConfig::default(),
    )
    .map_err(|error| error.to_string())
}
