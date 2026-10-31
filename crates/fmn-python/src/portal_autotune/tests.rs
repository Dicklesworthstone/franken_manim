use super::*;
use fmn::autotune::{NAMESPACE, NAMESPACE_VERSION, store_profile};
use fmn_codec::{PngLimits, decode_png};
use fmn_core::AaPolicy;
use fmn_platform::clock::FakeClock;
use fmn_platform::fs::StdFs;
use fmn_platform::topology::HardwareTopology;
use fmn_runtime::{
    AutotuneProfile, ExecutionEngine, ExecutionPlan, OutputPixelFormat, PlanRequest, RenderIntent,
    SurfaceSpec, TopologyFingerprint,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn request() -> PlanRequest {
    PlanRequest::standard(
        RenderIntent::Offline,
        SurfaceSpec::lumen(32, 18),
        OutputPixelFormat::Rgba8,
    )
    .with_max_cpu_threads(1)
}

fn seed(root: &Path, topology: &HardwareTopology, aa: AaPolicy, fine: u32, macro_tile: u32) {
    let mut config = fmn_config::Config::resolve(&[], None).unwrap().config;
    config.directories.cache = root.to_str().expect("test cache path is UTF-8").to_owned();
    config.render.aa = aa;
    store_profile(
        Arc::new(StdFs),
        Arc::new(FakeClock::new()),
        &config,
        request(),
        topology,
        AutotuneProfile {
            topology: TopologyFingerprint::of(topology),
            // Deliberately unlike the live one-thread request. A tile profile
            // must not take over team/queue/scratch admission.
            threads_per_render_team: 7,
            frames_in_flight: 6,
            fine_tile: fine,
            macro_tile,
            scratch_bytes_per_worker: 1024 * 1024,
        },
    )
    .expect("publish a real owned cache entry");
}

fn only_object(root: &Path) -> PathBuf {
    let objects = root
        .join("ns")
        .join(NAMESPACE)
        .join(format!("v{NAMESPACE_VERSION}"))
        .join("objects");
    let mut found = Vec::new();
    for shard in std::fs::read_dir(objects).unwrap() {
        let shard = shard.unwrap();
        assert!(shard.file_type().unwrap().is_dir());
        for entry in std::fs::read_dir(shard.path()).unwrap() {
            let entry = entry.unwrap();
            assert!(entry.file_type().unwrap().is_file());
            found.push(entry.path());
        }
    }
    assert_eq!(found.len(), 1, "one freshly seeded test profile");
    found.pop().unwrap()
}

#[pyfunction]
fn _assert_active_plan(
    scene: &Bound<'_, crate::PyScene>,
    fine: u32,
    macro_tile: u32,
    aa: &str,
) -> PyResult<()> {
    // Copy native facts and release all owner/lock borrows before any Python
    // allocation or exception conversion.
    let observed = {
        let owner = scene.borrow();
        let slot = owner.render.lock().unwrap();
        let Some(crate::PortalRenderSession::Frames(session)) = slot.as_ref() else {
            panic!("the witness must run inside the real file renderer"); // ubs:ignore -- Test-only missing-renderer assertion.
        };
        let config = session.renderer.config();
        (
            config.tiling,
            config.frame.aa,
            session.renderer.camera_tiling(),
            session.camera_tiling,
            session.plan.render_teams[0].threads(),
        )
    };
    assert_eq!(
        (observed.0.fine_tile, observed.0.macro_tile),
        (fine, macro_tile)
    );
    assert_eq!(
        observed.1,
        match aa {
            "adaptive" => AaPolicy::Adaptive,
            "ssaa2x" => AaPolicy::Ssaa2x,
            _ => panic!("test policy is explicit"),
        },
    );
    assert_eq!(
        observed.2, observed.3,
        "owner camera uses its uncached policy"
    );
    assert_eq!(
        observed.4, 1,
        "cached scheduling fields cannot widen the live cap"
    );
    Ok(())
}

#[test]
fn live_configuration_is_typed_bounded_and_bypassed_for_ineligible_cache_routes() {
    crate::with_python_test_module("autotune configuration snapshot", |py, module, globals| {
        let original = module.dict().get_item("manim_config").unwrap().unwrap();
        let live = original.cast::<PyDict>().unwrap();
        let directories = PyDict::new(py);
        directories.set_item("cache", "/typed/cache").unwrap();
        live.set_item("directories", &directories).unwrap();
        let render = PyDict::new(py);
        render.set_item("aa", "ssaa2x").unwrap();
        live.set_item("render", &render).unwrap();
        let cache = PyDict::new(py);
        cache.set_item("max_bytes", 8192).unwrap();
        live.set_item("cache", &cache).unwrap();

        let config = file_snapshot(py, false).unwrap();
        assert_eq!(config.directories.cache, "/typed/cache");
        assert_eq!(config.cache.max_bytes, 8192);
        assert_eq!(config.render.aa, AaPolicy::Ssaa2x);

        directories.set_item("cache", "x".repeat(4097)).unwrap();
        assert!(
            file_snapshot(py, false)
                .unwrap_err()
                .to_string()
                .contains("directories.cache")
        );
        directories.set_item("cache", "/typed/cache").unwrap();
        cache.set_item("max_bytes", true).unwrap();
        assert!(
            file_snapshot(py, false)
                .unwrap_err()
                .to_string()
                .contains("max_bytes")
        );
        cache.set_item("max_bytes", 8192).unwrap();
        render.set_item("aa", "not-an-aa-policy").unwrap();
        assert!(
            file_snapshot(py, false)
                .unwrap_err()
                .to_string()
                .contains("render.aa")
        );

        render.set_item("aa", "ssaa2x").unwrap();
        directories.set_item("cache", py.None()).unwrap();
        live.set_item("cache", py.None()).unwrap();
        let camera = camera_snapshot(py).expect("camera does not inspect unused cache settings");
        assert_eq!(camera.render.aa, AaPolicy::Ssaa2x);
        assert_eq!(
            camera.directories.cache,
            fmn_config::Config::resolve(&[], None)
                .unwrap()
                .config
                .directories
                .cache,
        );

        // Certified start must not even require a usable live configuration.
        globals.set_item("_saved_config", &original).unwrap();
        module.dict().set_item("manim_config", py.None()).unwrap();
        let certified = file_snapshot(py, true).expect("certified context bypass");
        assert_eq!(certified.render.aa, AaPolicy::Adaptive);
        module.dict().set_item("manim_config", original).unwrap();
    });
}

#[test]
fn real_portal_renders_consume_only_matching_planar_profiles_and_report_fallbacks() {
    let nonce = std::time::SystemTime::now() // ubs:ignore -- Test directory suffix, not an auth token; create_dir refuses reuse.
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "fmn-portal-autotune-{}-{nonce}",
        std::process::id(),
    ));
    std::fs::create_dir(&root).unwrap();
    let topology = HardwareTopology::current();
    let warm = root.join("warm-cache");
    let corrupt = root.join("corrupt-cache");
    let foreign = root.join("foreign-cache");
    seed(&warm, &topology, AaPolicy::Adaptive, 8, 64);
    seed(&warm, &topology, AaPolicy::Ssaa2x, 32, 256);
    seed(&corrupt, &topology, AaPolicy::Adaptive, 8, 64);
    std::fs::write(only_object(&corrupt), b"malformed cached envelope").unwrap();

    let mut other_hardware = topology.clone();
    other_hardware.total_memory_bytes = Some(
        topology
            .total_memory_bytes
            .unwrap_or(16 * 1024 * 1024 * 1024)
            + 1024 * 1024,
    );
    assert_ne!(
        TopologyFingerprint::of(&other_hardware),
        TopologyFingerprint::of(&topology),
    );
    seed(&foreign, &other_hardware, AaPolicy::Adaptive, 8, 64);
    let baseline = ExecutionPlan::derive(
        request().with_engine(ExecutionEngine::CertifiedCpu),
        &topology,
        None,
    )
    .unwrap();

    crate::with_python_test_module("persisted portal raster autotune", |py, module, globals| {
        globals
            .set_item("_AUTOTUNE_ROOT", root.to_str().unwrap())
            .unwrap();
        globals
            .set_item("_BASELINE_FINE", baseline.fine_tile)
            .unwrap();
        globals
            .set_item("_BASELINE_MACRO", baseline.macro_tile)
            .unwrap();
        globals
            .set_item(
                "_assert_active_plan",
                pyo3::wrap_pyfunction!(_assert_active_plan, module).unwrap(),
            )
            .unwrap();
        let source =
            std::ffi::CString::new(include_str!("../../tests/portal_autotune.py")).unwrap();
        py.run(source.as_c_str(), Some(globals), Some(globals))
            .inspect_err(|error| error.print(py))
            .expect("real portal cache, route, configuration, and fallback witnesses");
    });

    for name in [
        "warm.png",
        "aa.png",
        "foreign.png",
        "corrupt.png",
        "miss.png",
    ] {
        let bytes = std::fs::read(root.join(name)).unwrap();
        let image = decode_png(&bytes, &PngLimits::default()).unwrap();
        assert_eq!((image.width, image.height), (32, 18));
        let background = &image.rgba[..4];
        assert!(
            image
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel != background)
        );
    }
    assert!(
        !root.join("missing-cache").exists(),
        "cache misses never claim a root"
    );
}
