//! Persisted raster calibration through the public cache and native render paths.

use fmn::autotune::{
    NAMESPACE, NAMESPACE_VERSION, derive_execution_plan, load_profile, profile_key, store_profile,
};
use fmn::prelude::*;
use fmn_cache::{NamespacePolicy, Store, StoreConfig, object_relative_path};
use fmn_codec::{PngLimits, decode_png};
use fmn_config::Config;
use fmn_config::config::{DeterminismMode, ThreadPolicy};
use fmn_core::AaPolicy;
use fmn_platform::clock::FakeClock;
use fmn_platform::fs::{FileSystem, FsError, FsNodeKind, VirtualFs};
use fmn_platform::topology::HardwareTopology;
use fmn_runtime::{
    AutotuneProfile, ExecutionEngine, ExecutionPlan, OutputPixelFormat, PlanRequest, RenderIntent,
    SurfaceSpec, TopologyFingerprint, TuningSource,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn path(name: &str) -> PathBuf {
    #[cfg(windows)]
    let base = PathBuf::from(r"C:\fmn-autotune-tests");
    #[cfg(not(windows))]
    let base = PathBuf::from("/fmn-autotune-tests");
    base.join(name)
}

fn config() -> Config {
    let mut config = Config::resolve(&[], None).unwrap().config;
    config.directories.cache = path("cache").to_string_lossy().into_owned();
    config.cache.max_bytes = 16 * 1024 * 1024;
    config.camera.resolution = (32, 18);
    config.camera.fps = 10;
    config.sizes.frame_height = 4.0;
    config.determinism.mode = DeterminismMode::Standard;
    config.render.threads = ThreadPolicy::Fixed(1);
    config.render.aa = AaPolicy::Adaptive;
    config
}

fn request() -> PlanRequest {
    PlanRequest::standard(
        RenderIntent::Offline,
        SurfaceSpec::lumen(32, 18),
        OutputPixelFormat::Rgba8,
    )
    .with_max_cpu_threads(1)
    .with_max_frames_in_flight(1)
}

fn profile(topology: &HardwareTopology) -> AutotuneProfile {
    AutotuneProfile {
        topology: TopologyFingerprint::of(topology),
        // Deliberately unlike the live baseline: these are unmeasured fields.
        threads_per_render_team: 7,
        frames_in_flight: 6,
        fine_tile: 8,
        macro_tile: 64,
        scratch_bytes_per_worker: 1024 * 1024,
    }
}

fn persist(
    fs: &Arc<VirtualFs>,
    config: &Config,
    request: PlanRequest,
    topology: &HardwareTopology,
) {
    store_profile(
        fs.clone(),
        Arc::new(FakeClock::new()),
        config,
        request,
        topology,
        profile(topology),
    )
    .expect("publish a real cache entry");
}

fn put_raw_profile(
    fs: &Arc<VirtualFs>,
    config: &Config,
    request: PlanRequest,
    topology: &HardwareTopology,
    payload: &[u8],
) {
    let store = Store::open(
        fs.clone(),
        Arc::new(FakeClock::new()),
        Path::new(&config.directories.cache),
        StoreConfig::default(),
    )
    .unwrap();
    store
        .namespace(NAMESPACE, NAMESPACE_VERSION, NamespacePolicy::default())
        .unwrap()
        .put(&profile_key(config, request, topology).unwrap(), payload)
        .unwrap();
}

fn object_path(config: &Config, request: PlanRequest, topology: &HardwareTopology) -> PathBuf {
    let key = profile_key(config, request, topology).unwrap();
    Path::new(&config.directories.cache)
        .join("ns")
        .join(NAMESPACE)
        .join(format!("v{NAMESPACE_VERSION}"))
        .join("objects")
        .join(object_relative_path(key.digest()))
}

/// Supply the public Linux detector with one CPU; other native platforms
/// currently use the same flat host fallback as the rendering entry point.
fn native_filesystem() -> (Arc<VirtualFs>, HardwareTopology) {
    let fs = Arc::new(VirtualFs::new());
    #[cfg(target_os = "linux")]
    let topology = {
        fs.insert("/sys/devices/system/cpu/online", b"0\n".to_vec());
        fs.insert("/proc/meminfo", b"MemTotal: 1048576 kB\n".to_vec());
        HardwareTopology::detect_linux(fs.as_ref()).unwrap()
    };
    #[cfg(not(target_os = "linux"))]
    let topology = {
        let logical = std::thread::available_parallelism()
            .ok()
            .and_then(|count| u32::try_from(count.get()).ok())
            .unwrap_or(1);
        HardwareTopology::fallback(logical)
    };
    (fs, topology)
}

fn options(config: &Config, name: &str) -> RenderOptions {
    let mut options = RenderOptions::with_format(path(name), RenderFormat::Png).unwrap();
    options.config = config.clone();
    options.typeset_cache = false;
    options.frames_in_flight = 1;
    options.max_frames = 1;
    options.max_output_bytes = 1024 * 1024;
    // Native admission includes a 64 MiB codec allowance for this tiny image.
    options.max_resident_bytes = 96 * 1024 * 1024;
    options
}

struct StaticCircle;

impl SceneConstruct for StaticCircle {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let circle = stage.add(Circle::new().radius(0.65).color(BLUE))?;
        stage.set_fill(circle, Some(BLUE), Some(1.0), None, true);
        stage.set_stroke(circle, None, Some(0.0), None, None, true);
        Ok(())
    }
}

fn assert_circle_png(fs: &VirtualFs, name: &str) -> Vec<u8> {
    let bytes = fs.read(&path(name)).unwrap();
    let image = decode_png(&bytes, &PngLimits::default()).unwrap();
    assert_eq!((image.width, image.height), (32, 18));
    let background = &image.rgba[..4];
    assert!(
        image.rgba.chunks_exact(4).any(|pixel| pixel != background),
        "the real scene must produce visible geometry",
    );
    bytes
}

fn assert_only_tiles_changed(plan: &ExecutionPlan, baseline: &ExecutionPlan) {
    assert_eq!(plan.tuning_source, TuningSource::StandardAutotuneCache);
    assert_eq!((plan.fine_tile, plan.macro_tile), (8, 64));
    assert!(plan.tuning_diagnostic.is_none());
    let mut scheduling = plan.clone();
    scheduling.fine_tile = baseline.fine_tile;
    scheduling.macro_tile = baseline.macro_tile;
    scheduling.tuning_source = baseline.tuning_source;
    assert_eq!(
        scheduling, *baseline,
        "tile calibration changed live planning"
    );
}

#[test]
fn standard_native_render_consumes_the_persisted_raster_profile() {
    let (fs, topology) = native_filesystem();
    let config = config();
    let cold = render_with_fs(
        &mut StaticCircle,
        options(&config, "standard-cold.png"),
        fs.clone(),
    )
    .unwrap();
    assert_eq!(
        cold.execution_plan.tuning_source,
        TuningSource::StandardBaseline
    );
    assert_eq!(
        cold.execution_plan.topology_fingerprint,
        TopologyFingerprint::of(&topology)
    );
    assert_circle_png(fs.as_ref(), "standard-cold.png");
    persist(&fs, &config, request(), &topology);
    assert_eq!(
        load_profile(fs.as_ref(), &config, request(), &topology).unwrap(),
        Some(profile(&topology)),
    );
    let warm = render_with_fs(
        &mut StaticCircle,
        options(&config, "standard-warm.png"),
        fs.clone(),
    )
    .unwrap();
    assert_eq!(warm.artifact.frame_count, 1);
    assert_eq!(warm.scene.play_count, 0);
    assert_only_tiles_changed(&warm.execution_plan, &cold.execution_plan);
    assert_circle_png(fs.as_ref(), "standard-warm.png");
}

#[test]
fn malformed_profiles_and_corrupt_envelopes_fall_back_without_failing_render() {
    for case in 0..3 {
        let (fs, topology) = native_filesystem();
        let config = config();
        match case {
            0 => put_raw_profile(
                &fs,
                &config,
                request(),
                &topology,
                b"not an autotune record",
            ),
            1 => {
                let mut unsupported = profile(&topology);
                unsupported.fine_tile = 4;
                unsupported.macro_tile = 32;
                let payload = unsupported.encode().unwrap();
                put_raw_profile(&fs, &config, request(), &topology, &payload);
            }
            _ => {
                persist(&fs, &config, request(), &topology);
                let object = object_path(&config, request(), &topology);
                let mut bytes = fs.read(&object).unwrap();
                bytes[0] ^= 1;
                fs.insert(object, bytes);
            }
        }
        let object = object_path(&config, request(), &topology);
        let rejected = fs.read(&object).unwrap();
        assert!(load_profile(fs.as_ref(), &config, request(), &topology).is_err());
        let report = render_with_fs(
            &mut StaticCircle,
            options(&config, "fallback.png"),
            fs.clone(),
        )
        .expect("cache failure must not fail rendering");
        assert_eq!(
            report.execution_plan.tuning_source,
            TuningSource::StandardBaseline
        );
        let diagnostic = report.execution_plan.tuning_diagnostic.as_deref().unwrap();
        assert!(diagnostic.contains("autotune"), "{diagnostic}");
        assert!(diagnostic.contains("standard-baseline"), "{diagnostic}");
        assert_circle_png(fs.as_ref(), "fallback.png");
        assert_eq!(
            fs.read(&object).unwrap(),
            rejected,
            "read must not evict or repair"
        );
    }
}

#[test]
fn certified_png_is_identical_with_a_cold_or_warm_standard_cache() {
    let (fs, topology) = native_filesystem();
    let standard = config();
    let mut certified = standard.clone();
    certified.determinism.mode = DeterminismMode::Certified;
    let cold = render_with_fs(
        &mut StaticCircle,
        options(&certified, "certified-cold.png"),
        fs.clone(),
    )
    .unwrap();
    let cold_png = assert_circle_png(fs.as_ref(), "certified-cold.png");
    persist(&fs, &standard, request(), &topology);
    let warm = render_with_fs(
        &mut StaticCircle,
        options(&certified, "certified-warm.png"),
        fs.clone(),
    )
    .unwrap();
    assert_eq!(
        assert_circle_png(fs.as_ref(), "certified-warm.png"),
        cold_png
    );
    assert_eq!(warm.execution_plan, cold.execution_plan);
    assert_eq!(
        warm.execution_plan.tuning_source,
        TuningSource::CertifiedProfile
    );
    assert!(warm.execution_plan.tuning_diagnostic.is_none());
}

#[test]
fn different_hardware_and_measured_render_contexts_are_cache_misses() {
    let fs = Arc::new(VirtualFs::new());
    let config = config();
    let topology = HardwareTopology::fallback(4);
    persist(&fs, &config, request(), &topology);
    assert!(
        load_profile(fs.as_ref(), &config, request(), &topology)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        load_profile(
            fs.as_ref(),
            &config,
            request(),
            &HardwareTopology::fallback(5)
        )
        .unwrap(),
        None,
    );
    for other in [
        PlanRequest::standard(
            RenderIntent::Preview,
            SurfaceSpec::lumen(32, 18),
            OutputPixelFormat::Rgba8,
        )
        .with_max_cpu_threads(1)
        .with_max_frames_in_flight(1),
        PlanRequest::standard(
            RenderIntent::Offline,
            SurfaceSpec::lumen(64, 18),
            OutputPixelFormat::Rgba8,
        )
        .with_max_cpu_threads(1)
        .with_max_frames_in_flight(1),
        request().with_max_cpu_threads(2),
        request().with_engine(ExecutionEngine::CertifiedCpu),
    ] {
        assert_eq!(
            load_profile(fs.as_ref(), &config, other, &topology).unwrap(),
            None
        );
    }
    let mut different_aa = config.clone();
    different_aa.render.aa = AaPolicy::Ssaa2x;
    assert_eq!(
        load_profile(fs.as_ref(), &different_aa, request(), &topology).unwrap(),
        None
    );
}

#[test]
fn cached_tiles_preserve_live_queue_memory_thread_and_format_limits() {
    for memory in [2 * 1024 * 1024, 128 * 1024 * 1024] {
        let fs = Arc::new(VirtualFs::new());
        let config = config();
        let mut topology = HardwareTopology::from_group_sizes(&[4, 4]).unwrap();
        topology.total_memory_bytes = Some(memory);
        let measured = request().with_max_cpu_threads(2);
        persist(&fs, &config, measured, &topology);
        let stored_key = profile_key(&config, measured, &topology).unwrap();
        for (slots, format, working_bytes_per_pixel) in [
            (1, OutputPixelFormat::Rgba8, 8),
            (4, OutputPixelFormat::Rgba8, 8),
            (4, OutputPixelFormat::P010, 16),
        ] {
            let mut surface = SurfaceSpec::lumen(32, 18);
            surface.working_bytes_per_pixel = working_bytes_per_pixel;
            let live = PlanRequest::standard(RenderIntent::Offline, surface, format)
                .with_max_cpu_threads(2)
                .with_max_frames_in_flight(slots);
            assert_eq!(profile_key(&config, live, &topology).unwrap(), stored_key);
            let baseline = ExecutionPlan::derive(live, &topology, None).unwrap();
            let plan = derive_execution_plan(fs.as_ref(), &config, live, &topology).unwrap();
            assert_only_tiles_changed(&plan, &baseline);
            assert!(plan.frames_in_flight <= slots);
            assert!(
                plan.render_teams
                    .iter()
                    .map(|team| team.threads())
                    .sum::<usize>()
                    <= 2
            );
            assert_eq!(plan.output_format, format);
            if memory == 2 * 1024 * 1024 {
                assert_eq!(plan.frames_in_flight, 1, "RAM bound remains active");
            }
        }
    }
}

struct ForbiddenFs;
fn forbidden_fs_use<T>() -> T {
    panic!("certified planning must not consult its filesystem capability")
}
impl FileSystem for ForbiddenFs {
    fn identity(&self) -> &'static str {
        forbidden_fs_use()
    }
    fn grants_host_destructive_lifecycle(&self) -> bool {
        forbidden_fs_use()
    }
    fn node_kind_no_follow(&self, _path: &Path) -> Result<Option<FsNodeKind>, FsError> {
        forbidden_fs_use()
    }
    fn read(&self, _path: &Path) -> Result<Vec<u8>, FsError> {
        forbidden_fs_use()
    }
    fn read_bounded(&self, _path: &Path, _max_bytes: usize) -> Result<Vec<u8>, FsError> {
        forbidden_fs_use()
    }
    fn create_dir(&self, _path: &Path) -> Result<bool, FsError> {
        forbidden_fs_use()
    }
    fn write_atomic(&self, _path: &Path, _bytes: &[u8]) -> Result<(), FsError> {
        forbidden_fs_use()
    }
    fn create_new(&self, _path: &Path, _bytes: &[u8]) -> Result<bool, FsError> {
        forbidden_fs_use()
    }
    fn remove_file(&self, _path: &Path) -> Result<(), FsError> {
        forbidden_fs_use()
    }
    fn remove_dir_all(&self, _path: &Path) -> Result<(), FsError> {
        forbidden_fs_use()
    }
    fn exists(&self, _path: &Path) -> bool {
        forbidden_fs_use()
    }
    fn list_dir(&self, _path: &Path) -> Result<Vec<PathBuf>, FsError> {
        forbidden_fs_use()
    }
    fn count_dir_entries_bounded(
        &self,
        _path: &Path,
        _max_entries: usize,
    ) -> Result<usize, FsError> {
        forbidden_fs_use()
    }
}

#[test]
fn certified_planning_ignores_cache_configuration_before_any_filesystem_access() {
    let mut config = config();
    config.determinism.mode = DeterminismMode::Certified;
    config.directories.cache = "../invalid-cache-root".to_owned();
    let topology = HardwareTopology::fallback(4);
    let request = PlanRequest::certified(
        RenderIntent::Offline,
        SurfaceSpec::lumen(32, 18),
        OutputPixelFormat::Rgba8,
    )
    .with_max_cpu_threads(1)
    .with_max_frames_in_flight(1);
    let baseline = ExecutionPlan::derive(request, &topology, None).unwrap();
    assert_eq!(
        derive_execution_plan(&ForbiddenFs, &config, request, &topology).unwrap(),
        baseline
    );
}
