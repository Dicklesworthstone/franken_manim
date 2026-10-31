//! Persistent standard-mode raster autotuning (§17.4).
//!
//! Measurements use the existing owned cache store and the runtime's planner.
//! The initial production probe measures tile geometry only. Team placement,
//! frame slots and scratch budgets remain topology/request-derived; no measured
//! CPU-team throughput is inferred from a raster sample. Certified planning
//! returns before resolving a path or consulting any filesystem capability.

pub mod calibration;

use std::fmt;
use std::sync::Arc;

use fmn_cache::{CacheError, CacheKey, KeyBuilder, NamespacePolicy, Store, StoreConfig};
use fmn_config::Config;
use fmn_platform::clock::Clock;
use fmn_platform::fs::FileSystem;
use fmn_platform::topology::HardwareTopology;
use fmn_runtime::{
    AutotuneCache, AutotuneProfile, Determinism, ExecutionEngine, ExecutionPlan, PlanError,
    PlanRequest,
};

/// Versioned namespace for measured raster tile profiles.
pub const NAMESPACE: &str = "raster-autotune";
/// A schema change invalidates this namespace without rewriting other caches.
pub const NAMESPACE_VERSION: u32 = 1;
const PROFILE_BYTES: usize = 56;
const CACHE_CEILING: u64 = 64 * 1024;

/// Failure to produce or persist an optional standard-mode measurement.
#[derive(Debug)]
pub enum AutotuneError {
    /// The requested engine/mode cannot be measured by the CPU raster probe.
    Unsupported(&'static str),
    /// The existing governed cache store refused an operation.
    Cache(CacheError),
    /// The request or persisted measurement is invalid.
    Plan(PlanError),
}

impl fmt::Display for AutotuneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(reason) => write!(f, "autotune unavailable: {reason}"),
            Self::Cache(error) => write!(f, "autotune cache: {error}"),
            Self::Plan(error) => write!(f, "autotune profile: {error}"),
        }
    }
}

impl std::error::Error for AutotuneError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Cache(error) => Some(error),
            Self::Plan(error) => Some(error),
            Self::Unsupported(_) => None,
        }
    }
}

impl From<CacheError> for AutotuneError {
    fn from(error: CacheError) -> Self {
        Self::Cache(error)
    }
}

impl From<PlanError> for AutotuneError {
    fn from(error: PlanError) -> Self {
        Self::Plan(error)
    }
}

/// Cache identity shared by native scenes, the CLI and doctor.
///
/// The CPU renderer version, compiled tier, platform, topology, resolution,
/// intent and explicit CPU ceiling discriminate measurements. Output format,
/// working-surface memory and queue caps remain live planner constraints.
///
/// # Errors
/// Returns an encoding error if canonical key material cannot be represented.
pub fn profile_key(
    config: &Config,
    request: PlanRequest,
    topology: &HardwareTopology,
) -> Result<CacheKey, AutotuneError> {
    KeyBuilder::new("raster-autotune/v1")
        .push_str(env!("CARGO_PKG_VERSION"))
        .push_str(std::env::consts::OS)
        .push_str(std::env::consts::ARCH)
        .push_str(fmn_render::Tier::COMPILED.name())
        .push_u32(fmn_render::RENDERER_VERSION)
        .push_str(match config.render.aa {
            fmn_core::AaPolicy::Adaptive => "adaptive",
            fmn_core::AaPolicy::Ssaa2x => "ssaa2x",
            fmn_core::AaPolicy::Ssaa4x => "ssaa4x",
        })
        .push_u64(fmn_runtime::TopologyFingerprint::of(topology).value())
        .push_bytes(&request.autotune_key_material())
        .finish()
        .map_err(CacheError::Encode)
        .map_err(AutotuneError::Cache)
}

fn admit_profile(profile: &AutotuneProfile) -> Result<(), AutotuneError> {
    // These are the actual probe candidates. Refuse a well-checksummed but
    // unmeasured/hostile value before it can size renderer scratch or bins.
    if !matches!(
        (profile.fine_tile, profile.macro_tile),
        (8, 64) | (16, 128) | (32, 256)
    ) {
        return Err(AutotuneError::Unsupported(
            "cached tile dimensions are outside the measured raster catalog",
        ));
    }
    Ok(())
}

/// Read a bounded, verified profile without creating a cache root or index.
///
/// Certified/annex requests return a miss before any filesystem/path access.
///
/// # Errors
/// Corruption, ownership/traversal refusal or unsupported profile values.
pub fn load_profile(
    fs: &dyn FileSystem,
    config: &Config,
    request: PlanRequest,
    topology: &HardwareTopology,
) -> Result<Option<AutotuneProfile>, AutotuneError> {
    let baseline = ExecutionPlan::derive(request, topology, None)?;
    if baseline.determinism != Determinism::Standard || baseline.engine != ExecutionEngine::FastCpu
    {
        return Ok(None);
    }
    let root = fmn_cache::resolve_host_cache_root(&config.directories.cache)
        .map_err(CacheError::RootResolution)?;
    let key = profile_key(config, request, topology)?;
    let Some(bytes) =
        Store::read_keyed(fs, &root, NAMESPACE, NAMESPACE_VERSION, &key, PROFILE_BYTES)?
    else {
        return Ok(None);
    };
    let profile = AutotuneProfile::decode(&bytes, topology)?;
    admit_profile(&profile)?;
    Ok(Some(profile))
}

/// Derive the real render plan using a matching optional raster measurement.
///
/// Every cache failure falls back to an already-validated baseline and records
/// its named reason in ExecutionPlan::tuning_diagnostic. A cache cannot turn
/// a valid render into a failure. Certified requests do not access the cache.
///
/// # Errors
/// Only the uncached request/topology's own planning errors.
pub fn derive_execution_plan(
    fs: &dyn FileSystem,
    config: &Config,
    request: PlanRequest,
    topology: &HardwareTopology,
) -> Result<ExecutionPlan, PlanError> {
    let mut baseline = ExecutionPlan::derive(request, topology, None)?;
    if baseline.determinism != Determinism::Standard || baseline.engine != ExecutionEngine::FastCpu
    {
        return Ok(baseline);
    }
    let selected = (|| {
        let Some(profile) = load_profile(fs, config, request, topology)? else {
            return Ok(None);
        };
        let cache = AutotuneCache::for_raster_tiles(profile)?;
        ExecutionPlan::derive(request, topology, Some(&cache))
            .map(Some)
            .map_err(AutotuneError::Plan)
    })();
    match selected {
        Ok(Some(plan)) => Ok(plan),
        Ok(None) => Ok(baseline),
        Err(error) => {
            baseline.tuning_diagnostic = Some(format!("{error}; using standard-baseline"));
            Ok(baseline)
        }
    }
}

/// Publish the first measured raster profile for this hardware/request key.
///
/// Keyed records are immutable. Repeating an identical publication is harmless;
/// a concurrent different measurement is a named KeyConflict, preserving the
/// winner. Explicit cache clearing permits a fresh calibration. This method
/// never labels the profile's team/queue/scratch fields as measured.
///
/// # Errors
/// Certified/non-CPU requests, wrong topology, unsupported tile dimensions,
/// invalid values, or owned-cache storage refusal.
pub fn store_profile(
    fs: Arc<dyn FileSystem>,
    clock: Arc<dyn Clock>,
    config: &Config,
    request: PlanRequest,
    topology: &HardwareTopology,
    profile: AutotuneProfile,
) -> Result<(), AutotuneError> {
    let baseline = ExecutionPlan::derive(request, topology, None)?;
    if baseline.determinism != Determinism::Standard || baseline.engine != ExecutionEngine::FastCpu
    {
        return Err(AutotuneError::Unsupported(
            "raster calibration requires standard CPU mode",
        ));
    }
    if config.cache.max_bytes < 4096 {
        return Err(AutotuneError::Unsupported(
            "cache.max_bytes must allow at least 4096 bytes for a measured profile",
        ));
    }
    let bytes = profile.encode()?;
    let profile = AutotuneProfile::decode(&bytes, topology)?;
    admit_profile(&profile)?;
    let key = profile_key(config, request, topology)?;
    let store_config = StoreConfig {
        max_entry_bytes: PROFILE_BYTES,
        ..StoreConfig::default()
    };
    let store = if fs.grants_host_destructive_lifecycle() {
        Store::open_host(fs, clock, &config.directories.cache, store_config)?
    } else {
        let root = fmn_cache::resolve_host_cache_root(&config.directories.cache)
            .map_err(CacheError::RootResolution)?;
        Store::open(fs, clock, root, store_config)?
    };
    let namespace = store.namespace(
        NAMESPACE,
        NAMESPACE_VERSION,
        NamespacePolicy {
            ceiling_bytes: Some(config.cache.max_bytes.min(CACHE_CEILING)),
        },
    )?;
    namespace.put(&key, &bytes)?;
    namespace.evict_to_ceiling()?;
    Ok(())
}
